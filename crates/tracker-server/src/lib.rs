//! Serveur local (127.0.0.1 uniquement) : page « Source Navigateur » pour OBS, WebSocket de
//! snapshots du lobby et point d'entrée `/ingest` utilisé par la WebView masquée.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracker_core::LobbySnapshot;

const OBS_PAGE: &str = include_str!("../assets/obs.html");
/// Taille maximale d'un corps `/ingest` (un profil TRN fait quelques dizaines de Ko).
const MAX_INGEST_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("impossible d'écouter sur 127.0.0.1:{port} : {source}")]
    Bind { port: u16, source: std::io::Error },
}

/// Reçoit `(request_id, corps)` ; renvoie `true` si l'identifiant était attendu.
pub type IngestFn = Arc<dyn Fn(&str, String) -> bool + Send + Sync>;

#[derive(Clone)]
struct AppState {
    snapshots: broadcast::Sender<LobbySnapshot>,
    latest: Arc<RwLock<LobbySnapshot>>,
    ingest: IngestFn,
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

pub async fn start(
    port: u16,
    snapshots: broadcast::Sender<LobbySnapshot>,
    ingest: IngestFn,
) -> Result<ServerHandle, ServerError> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|source| ServerError::Bind { port, source })?;
    let addr = listener.local_addr().map_err(|source| ServerError::Bind { port, source })?;

    let latest = Arc::new(RwLock::new(LobbySnapshot::default()));
    let state = AppState { snapshots: snapshots.clone(), latest: latest.clone(), ingest };

    // Garde le dernier snapshot pour les clients qui se connectent en cours de lobby.
    let mut rx = snapshots.subscribe();
    let keeper = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(s) => {
                    if let Ok(mut l) = latest.write() {
                        *l = s;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    });

    let app = router(state);
    let serve = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("serveur local arrêté : {e}");
        }
    });
    Ok(ServerHandle { addr, tasks: vec![keeper, serve] })
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(obs_page))
        .route("/obs", get(obs_page))
        .route("/api/snapshot", get(snapshot))
        .route("/ws", get(ws_upgrade))
        .route(
            "/ingest/:id",
            post(ingest).layer(axum::extract::DefaultBodyLimit::max(MAX_INGEST_BYTES)),
        )
        .with_state(state)
}

async fn obs_page() -> Html<&'static str> {
    Html(OBS_PAGE)
}

async fn snapshot(State(s): State<AppState>) -> Json<LobbySnapshot> {
    Json(s.latest.read().map(|l| l.clone()).unwrap_or_default())
}

async fn ingest(State(s): State<AppState>, Path(id): Path<String>, body: String) -> StatusCode {
    if (s.ingest)(&id, body) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(s): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, s))
}

async fn ws_session(mut socket: WebSocket, s: AppState) {
    let mut rx = s.snapshots.subscribe();
    let first = s.latest.read().map(|l| l.clone()).unwrap_or_default();
    if send_snapshot(&mut socket, &first).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(snap) => if send_snapshot(&mut socket, &snap).await.is_err() { return },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
        }
    }
}

async fn send_snapshot(socket: &mut WebSocket, snap: &LobbySnapshot) -> Result<(), axum::Error> {
    let json = serde_json::to_string(snap).unwrap_or_else(|_| "{}".into());
    socket.send(Message::Text(json)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn http(addr: SocketAddr, req: String) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn serves_obs_page_snapshot_and_ingest() {
        let (tx, _rx) = broadcast::channel(8);
        let got = Arc::new(AtomicBool::new(false));
        let got2 = got.clone();
        let ingest: IngestFn = Arc::new(move |id, body| {
            let ok = id == "known" && body == "{\"a\":1}";
            got2.store(ok, Ordering::SeqCst);
            id == "known"
        });
        let h = start(0, tx.clone(), ingest).await.unwrap();

        let page = http(h.addr, "GET /obs HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".into()).await;
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(page.contains("LobbyTracker"));

        tx.send(LobbySnapshot { pending: 3, ..Default::default() }).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let snap = http(h.addr, "GET /api/snapshot HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".into()).await;
        assert!(snap.contains("\"pending\":3"));

        let body = "{\"a\":1}";
        let ok = http(
            h.addr,
            format!("POST /ingest/known HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()),
        )
        .await;
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        assert!(got.load(Ordering::SeqCst));

        let bad = http(
            h.addr,
            format!("POST /ingest/nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()),
        )
        .await;
        assert!(bad.starts_with("HTTP/1.1 404"));
    }

    #[tokio::test]
    async fn port_in_use_is_a_clear_error() {
        let (tx, _rx) = broadcast::channel(1);
        let ingest: IngestFn = Arc::new(|_, _| false);
        let first = start(0, tx.clone(), ingest.clone()).await.unwrap();
        let err = start(first.addr.port(), tx, ingest).await.err().unwrap();
        assert!(err.to_string().contains("impossible d'écouter"));
    }
}
