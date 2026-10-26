//! Orchestration : watcher → parser → cache / file d'attente → heuristiques → snapshots.

pub mod lobby;

use lobby::{Lobby, Upsert};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracker_cache::Cache;
use tracker_core::{unix_now, Config, LobbySnapshot, PlayerId, Relation, Team};
use tracker_parser::{parse_stats_message, LogParser, ParsedEvent};
use tracker_scraper::{
    FetchOptions, Fetcher, HybridFetcher, Priority, ScrapeError, SourceFetcher, TrackerQueue,
    TrnApiSource, WebviewDriver, WebviewSource,
};
use tracker_server::ServerHandle;
use tracker_watcher::WatchEvent;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Cache(#[from] tracker_cache::CacheError),
    #[error(transparent)]
    Scrape(#[from] ScrapeError),
    #[error(transparent)]
    Server(#[from] tracker_server::ServerError),
}

/// Désactive la source WebView quand l'utilisateur la coupe dans les paramètres.
struct Gated {
    enabled: Arc<dyn Fn() -> bool + Send + Sync>,
    inner: Arc<dyn Fetcher>,
}

#[async_trait::async_trait]
impl Fetcher for Gated {
    async fn fetch(
        &self,
        id: &PlayerId,
        opts: FetchOptions,
    ) -> Result<tracker_core::PlayerProfile, ScrapeError> {
        if (self.enabled)() {
            self.inner.fetch(id, opts).await
        } else {
            Err(ScrapeError::NoSource)
        }
    }
}

struct Inner {
    cfg: RwLock<Config>,
    cache: Arc<Cache>,
    queue: Arc<TrackerQueue>,
    lobby: Mutex<Lobby>,
    snapshots: broadcast::Sender<LobbySnapshot>,
    webview: Option<WebviewSource>,
    parser: LogParser,
}

pub struct Engine {
    inner: Arc<Inner>,
    fetcher: Arc<dyn Fetcher>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    server: Mutex<Option<ServerHandle>>,
}

impl Engine {
    pub fn new(
        cfg: Config,
        cache_path: &Path,
        driver: Option<Arc<dyn WebviewDriver>>,
    ) -> Result<Arc<Self>, EngineError> {
        let cache = Arc::new(Cache::open(cache_path)?);
        let ttl = cfg.cache_ttl_hours as i64 * 3600;
        if let Err(e) = cache.purge_expired(ttl * 7) {
            tracing::warn!("purge du cache impossible : {e}");
        }
        let webview = driver.map(|d| WebviewSource::new(d, cfg.server_port));
        let (snapshots, _) = broadcast::channel(32);
        let inner = Arc::new(Inner {
            cfg: RwLock::new(cfg),
            cache,
            queue: TrackerQueue::new(),
            lobby: Mutex::new(Lobby::default()),
            snapshots,
            webview,
            parser: LogParser::new(),
        });

        let mut sources: Vec<Arc<dyn Fetcher>> = Vec::new();
        if let Some(w) = inner.webview.clone() {
            let i = inner.clone();
            sources.push(Arc::new(Gated {
                enabled: Arc::new(move || i.cfg.read().map(|c| c.use_webview_scraper).unwrap_or(true)),
                inner: Arc::new(SourceFetcher::new(w)),
            }));
        }
        let i = inner.clone();
        let key = Arc::new(move || i.cfg.read().ok().and_then(|c| c.trn_api_key.clone()));
        sources.push(Arc::new(SourceFetcher::new(TrnApiSource::new(key)?)));

        Ok(Arc::new(Self {
            inner,
            fetcher: Arc::new(HybridFetcher::new(sources)),
            tasks: Mutex::default(),
            server: Mutex::default(),
        }))
    }

    /// Flux de snapshots pour l'overlay (événements Tauri) et le serveur OBS.
    pub fn subscribe(&self) -> broadcast::Receiver<LobbySnapshot> {
        self.inner.snapshots.subscribe()
    }

    pub fn snapshot(&self) -> LobbySnapshot {
        self.inner.build_snapshot()
    }

    pub fn config(&self) -> Config {
        self.inner.cfg.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// Applique une nouvelle configuration. Les filtres, jetons et options du scraper sont
    /// pris en compte à chaud ; chemins et ports exigent un redémarrage.
    pub fn update_config(&self, cfg: Config) {
        if let Ok(mut c) = self.inner.cfg.write() {
            *c = cfg;
        }
        self.inner.emit();
    }

    /// Démarre serveur local, watchers, file d'attente. À appeler depuis un runtime tokio.
    pub async fn start(self: &Arc<Self>) -> Result<(), EngineError> {
        let cfg = self.config();
        let inner = self.inner.clone();

        // Serveur local (OBS + ingest WebView).
        let ingest_src = inner.webview.clone();
        let ingest: tracker_server::IngestFn =
            Arc::new(move |id, body| ingest_src.as_ref().is_some_and(|w| w.submit(id, body)));
        let server = tracker_server::start(cfg.server_port, inner.snapshots.clone(), ingest).await?;
        *self.server.lock().unwrap() = Some(server);

        let mut tasks = Vec::new();

        // File d'attente + worker réseau.
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let i = inner.clone();
        let options: Arc<dyn Fn() -> FetchOptions + Send + Sync> = Arc::new(move || FetchOptions {
            with_sessions: i.cfg.read().map(|c| c.filters.on_tilt).unwrap_or(false),
        });
        tasks.push(inner.queue.spawn_worker(
            self.fetcher.clone(),
            inner.cache.clone(),
            cfg.cache_ttl_hours as i64 * 3600,
            options,
            Duration::from_millis(1200),
            out_tx,
        ));
        let i = inner.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(outcome) = out_rx.recv().await {
                i.apply_outcome(outcome);
            }
        }));

        // Watchers passifs.
        let (tx, mut rx) = mpsc::channel::<WatchEvent>(1024);
        match cfg.effective_log_path() {
            Some(path) => {
                tracing::info!("surveillance de {}", path.display());
                tasks.push(tracker_watcher::spawn_log_tailer(path, tx.clone()));
            }
            None => tracing::warn!("dossier Documents introuvable : Launch.log non surveillé"),
        }
        if cfg.stats_api_enabled {
            tasks.push(tracker_watcher::spawn_stats_listener(cfg.stats_api_port, tx.clone()));
        }
        drop(tx);

        let i = inner.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                i.handle_watch_event(ev);
            }
        }));

        *self.tasks.lock().unwrap() = tasks;
        inner.emit();
        Ok(())
    }

    /// Reçoit le JSON renvoyé par une WebView masquée (voie IPC Tauri).
    pub fn ingest(&self, request_id: &str, body: String) -> bool {
        self.inner.webview.as_ref().is_some_and(|w| w.submit(request_id, body))
    }

    /// Injecte un identifiant à la main (utile pour tester sans lancer le jeu).
    pub fn inject_player(&self, id: PlayerId, name: Option<String>, team: Option<Team>) {
        self.inner.handle_parsed(ParsedEvent::PlayerSeen {
            id,
            name,
            team,
            context: tracker_parser::SeenContext::Other,
        });
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Ok(tasks) = self.tasks.lock() {
            for t in tasks.iter() {
                t.abort();
            }
        }
    }
}

impl Inner {
    fn handle_watch_event(&self, ev: WatchEvent) {
        let events = match ev {
            WatchEvent::LogLine(line) => self.parser.parse_line(&line),
            WatchEvent::StatsMessage(msg) => match parse_stats_message(&msg) {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!("{e}");
                    return;
                }
            },
        };
        for e in events {
            self.handle_parsed(e);
        }
    }

    fn handle_parsed(&self, ev: ParsedEvent) {
        match ev {
            ParsedEvent::LobbyReset => {
                self.queue.clear();
                self.lobby.lock().unwrap().reset();
                self.emit();
            }
            ParsedEvent::PlayerSeen { id, name, team, .. } => self.handle_seen(id, name, team),
        }
    }

    fn is_self(&self, id: &PlayerId, name: Option<&str>) -> bool {
        let cfg = self.cfg.read().unwrap();
        let by_id = cfg
            .self_id
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(&id.id) || s.eq_ignore_ascii_case(&id.key()));
        let by_name = matches!((cfg.self_name.as_deref(), name),
            (Some(me), Some(n)) if !me.is_empty() && me.eq_ignore_ascii_case(n));
        by_id || by_name
    }

    fn handle_seen(&self, id: PlayerId, name: Option<String>, team: Option<Team>) {
        if self.is_self(&id, name.as_deref()) {
            // On ne se profile pas soi-même, mais on en déduit notre équipe.
            let mut lobby = self.lobby.lock().unwrap();
            if team.is_some_and(|t| lobby.set_self_team(t)) {
                lobby.recompute_relations();
                drop(lobby);
                self.emit();
            }
            return;
        }

        let (state, relation) = {
            let mut lobby = self.lobby.lock().unwrap();
            let state = lobby.upsert(&id, name, team);
            (state, lobby.relation_for(&id))
        };
        if state == Upsert::Unchanged {
            return;
        }

        if state == Upsert::New {
            let ttl = self.cfg.read().unwrap().cache_ttl_hours as i64 * 3600;
            match self.cache.get_fresh(&id, ttl) {
                // Chemin rapide : affichage immédiat depuis SQLite.
                Ok(Some(profile)) => {
                    self.lobby.lock().unwrap().set_profile(profile);
                }
                Ok(None) => {
                    self.queue.enqueue(id, priority_of(relation));
                }
                Err(e) => {
                    tracing::warn!("lecture du cache impossible : {e}");
                    self.queue.enqueue(id, priority_of(relation));
                }
            }
        }
        self.emit();
    }

    fn apply_outcome(&self, outcome: tracker_scraper::FetchOutcome) {
        {
            let mut lobby = self.lobby.lock().unwrap();
            match outcome {
                tracker_scraper::FetchOutcome::Fetched(p) => {
                    lobby.set_profile(*p);
                }
                tracker_scraper::FetchOutcome::Failed { id, error } => {
                    tracing::info!("profil {id} indisponible : {error}");
                    lobby.set_failed(&id);
                }
            }
        }
        self.emit();
    }

    fn build_snapshot(&self) -> LobbySnapshot {
        let cfg = self.cfg.read().unwrap().clone();
        let lobby = self.lobby.lock().unwrap();
        LobbySnapshot {
            players: tracker_heuristics::build_views(lobby.entries(), &cfg),
            pending: lobby.loading_count(),
            updated_at: unix_now(),
        }
    }

    fn emit(&self) {
        let _ = self.snapshots.send(self.build_snapshot());
    }
}

fn priority_of(r: Relation) -> Priority {
    match r {
        Relation::Opponent => Priority::Opponent,
        Relation::Unknown => Priority::Unknown,
        Relation::Teammate => Priority::Teammate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::{Platform, PlayerProfile, PlayerStatus, PlaylistKind, PlaylistStat};

    fn id(n: u32) -> PlayerId {
        PlayerId::new(Platform::Steam, format!("7656119800000{n:04}"))
    }

    fn engine(cfg: Config) -> Arc<Engine> {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lt-engine-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Engine::new(cfg, &dir.join("cache.db"), None).unwrap()
    }

    fn smurf_profile(n: u32) -> PlayerProfile {
        PlayerProfile {
            id: id(n),
            display_name: "Smurfy".into(),
            total_wins: 90,
            peak_tier: None,
            playlists: vec![PlaylistStat { kind: PlaylistKind::Standard, tier: 19, mmr: None, matches: 50 }],
            demos_per_match: None,
            saves_per_match: None,
            goals_per_match: None,
            shots_per_match: None,
            loss_streak: 0,
            fetched_at: unix_now(),
        }
    }

    #[test]
    fn cached_player_is_ready_immediately_without_network() {
        let e = engine(Config::default());
        e.inner.cache.put(&smurf_profile(1)).unwrap();
        e.inject_player(id(1), Some("raw".into()), Some(Team::Orange));
        let snap = e.snapshot();
        assert_eq!(snap.players.len(), 1);
        assert_eq!(snap.players[0].status, PlayerStatus::Ready);
        assert!(snap.players[0].badge_labels.iter().any(|l| l == "Smurf suspect"));
        assert_eq!(snap.pending, 0);
    }

    #[test]
    fn uncached_player_is_loading_and_queued_by_priority() {
        let mut cfg = Config::default();
        cfg.self_name = Some("Me".into());
        let e = engine(cfg);
        e.inject_player(id(0), Some("Me".into()), Some(Team::Blue));
        e.inject_player(id(1), Some("Mate".into()), Some(Team::Blue));
        e.inject_player(id(2), Some("Foe".into()), Some(Team::Orange));
        let snap = e.snapshot();
        assert_eq!(snap.players.len(), 2, "soi-même n'est pas profilé");
        assert_eq!(snap.players[0].name, "Foe", "adversaires en premier");
        assert_eq!(snap.pending, 2);
        // L'adversaire est servi avant le coéquipier même s'il est arrivé après.
        assert_eq!(e.inner.queue.pop_next(), Some(id(2)));
    }

    #[test]
    fn only_opponents_filter_applies_live() {
        let mut cfg = Config::default();
        cfg.self_name = Some("Me".into());
        let e = engine(cfg.clone());
        e.inject_player(id(0), Some("Me".into()), Some(Team::Blue));
        e.inject_player(id(1), Some("Mate".into()), Some(Team::Blue));
        e.inject_player(id(2), Some("Foe".into()), Some(Team::Orange));
        assert_eq!(e.snapshot().players.len(), 2);
        cfg.filters.only_opponents = true;
        e.update_config(cfg);
        let snap = e.snapshot();
        assert_eq!(snap.players.len(), 1);
        assert_eq!(snap.players[0].name, "Foe");
    }

    #[test]
    fn lobby_reset_event_clears_players() {
        let e = engine(Config::default());
        e.inject_player(id(1), None, None);
        e.inner.handle_parsed(ParsedEvent::LobbyReset);
        assert!(e.snapshot().players.is_empty());
    }

    #[test]
    fn log_lines_flow_through_parser() {
        let e = engine(Config::default());
        e.inner.handle_watch_event(WatchEvent::LogLine(
            "LogNet: BeaconJoin PlayerName=Zed, Steam|76561198000000001|0".into(),
        ));
        let snap = e.snapshot();
        assert_eq!(snap.players.len(), 1);
        assert_eq!(snap.players[0].name, "Zed");
    }
}
