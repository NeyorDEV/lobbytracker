//! Source « WebView2 invisible » : une vraie fenêtre de navigateur résout le défi Cloudflare,
//! puis un script injecté dans *notre propre* page web renvoie le JSON au serveur local.
//! Rien n'est injecté dans le processus du jeu.

use crate::fetcher::JsonSource;
use crate::ScrapeError;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tracker_core::PlayerId;

const PAGE_TIMEOUT: Duration = Duration::from_secs(25);

/// Implémenté par l'application Tauri : ouvre/ferme une fenêtre masquée.
pub trait WebviewDriver: Send + Sync {
    fn open(&self, request_id: &str, url: &str, init_script: &str) -> Result<(), String>;
    fn close(&self, request_id: &str);
}

/// Script exécuté dans la fenêtre masquée : dès que la page affiche du JSON valide
/// (donc après le passage de Cloudflare), il le poste à `127.0.0.1`.
pub fn ingest_script(port: u16, request_id: &str) -> String {
    format!(
        r#"(function () {{
  if (window.top !== window || window.__lt_armed) return;
  window.__lt_armed = true;
  var timer = setInterval(function () {{
    try {{
      var txt = ((document.body && document.body.innerText) || "").trim();
      if (txt.charAt(0) !== "{{") return;
      JSON.parse(txt);
      clearInterval(timer);
      var viaHttp = function () {{
        fetch("http://127.0.0.1:{port}/ingest/{request_id}", {{
          method: "POST", mode: "no-cors",
          headers: {{ "Content-Type": "text/plain" }}, body: txt
        }});
      }};
      var ipc = window.__TAURI_INTERNALS__;
      if (ipc && ipc.invoke) {{
        ipc.invoke("submit_scrape", {{ requestId: "{request_id}", body: txt }}).catch(viaHttp);
      }} else {{
        viaHttp();
      }}
    }} catch (e) {{}}
  }}, 400);
}})();"#
    )
}

fn new_request_id() -> String {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let a = h.finish();
    let b = RandomState::new().build_hasher().finish();
    format!("{a:016x}{b:016x}")
}

#[derive(Clone)]
pub struct WebviewSource {
    driver: Arc<dyn WebviewDriver>,
    port: u16,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
}

impl WebviewSource {
    pub fn new(driver: Arc<dyn WebviewDriver>, port: u16) -> Self {
        Self { driver, port, pending: Arc::default() }
    }

    /// Appelé par le serveur local quand la page masquée renvoie son JSON.
    /// Renvoie `false` si l'identifiant de requête est inconnu (donc ignoré).
    pub fn submit(&self, request_id: &str, body: String) -> bool {
        let tx = self.pending.lock().ok().and_then(|mut m| m.remove(request_id));
        match tx {
            Some(tx) => tx.send(body).is_ok(),
            None => false,
        }
    }
}

#[async_trait]
impl JsonSource for WebviewSource {
    fn name(&self) -> &'static str {
        "webview"
    }

    fn profile_url(&self, id: &PlayerId) -> String {
        format!(
            "https://api.tracker.gg/api/v2/rocket-league/standard/profile/{}/{}",
            id.platform.tracker_slug(),
            id.id
        )
    }

    fn sessions_url(&self, id: &PlayerId) -> String {
        format!("{}/sessions", self.profile_url(id))
    }

    async fn get_json(&self, url: &str) -> Result<Value, ScrapeError> {
        let request_id = new_request_id();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| ScrapeError::Webview("verrou empoisonné".into()))?
            .insert(request_id.clone(), tx);

        let script = ingest_script(self.port, &request_id);
        if let Err(e) = self.driver.open(&request_id, url, &script) {
            if let Ok(mut m) = self.pending.lock() {
                m.remove(&request_id);
            }
            return Err(ScrapeError::Webview(e));
        }

        let result = tokio::time::timeout(PAGE_TIMEOUT, rx).await;
        self.driver.close(&request_id);
        if let Ok(mut m) = self.pending.lock() {
            m.remove(&request_id);
        }
        let body = match result {
            Err(_) => return Err(ScrapeError::Timeout),
            Ok(Err(_)) => return Err(ScrapeError::Webview("canal fermé".into())),
            Ok(Ok(b)) => b,
        };
        serde_json::from_str(&body).map_err(|e| ScrapeError::BadPayload(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::Platform;

    /// Simule la fenêtre masquée : renvoie le JSON via `submit` comme le ferait le script.
    struct FakeDriver {
        source: Mutex<Option<WebviewSource>>,
        reply: Option<String>,
        closed: Mutex<Vec<String>>,
    }

    impl WebviewDriver for FakeDriver {
        fn open(&self, request_id: &str, _url: &str, script: &str) -> Result<(), String> {
            assert!(script.contains(request_id));
            if let Some(body) = self.reply.clone() {
                let src = self.source.lock().unwrap().clone().unwrap();
                let rid = request_id.to_string();
                tokio::spawn(async move {
                    src.submit(&rid, body);
                });
            }
            Ok(())
        }
        fn close(&self, request_id: &str) {
            self.closed.lock().unwrap().push(request_id.to_string());
        }
    }

    fn setup(reply: Option<&str>) -> (WebviewSource, Arc<FakeDriver>) {
        let driver = Arc::new(FakeDriver {
            source: Mutex::new(None),
            reply: reply.map(str::to_owned),
            closed: Mutex::default(),
        });
        let src = WebviewSource::new(driver.clone(), 49124);
        *driver.source.lock().unwrap() = Some(src.clone());
        (src, driver)
    }

    #[tokio::test]
    async fn returns_json_submitted_by_page_and_closes_window() {
        let (src, driver) = setup(Some(r#"{"data":{"segments":[]}}"#));
        let id = PlayerId::new(Platform::Steam, "76561198012345678");
        let v = src.get_json(&src.profile_url(&id)).await.unwrap();
        assert!(v.get("data").is_some());
        assert_eq!(driver.closed.lock().unwrap().len(), 1);
        assert!(src.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_body_is_bad_payload() {
        let (src, _d) = setup(Some("not json"));
        assert!(matches!(src.get_json("https://x").await, Err(ScrapeError::BadPayload(_))));
    }

    #[test]
    fn unknown_request_ids_are_rejected() {
        let (src, _d) = setup(None);
        assert!(!src.submit("deadbeef", "{}".into()));
    }

    #[test]
    fn script_targets_local_ingest_endpoint() {
        let s = ingest_script(49124, "abc");
        assert!(s.contains("http://127.0.0.1:49124/ingest/abc"));
    }

    #[test]
    fn request_ids_are_unique_and_long() {
        let a = new_request_id();
        assert_eq!(a.len(), 32);
        assert_ne!(a, new_request_id());
    }
}
