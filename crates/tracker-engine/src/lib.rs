//! Orchestration : watcher → parser → cache / file d'attente → heuristiques → snapshots.

mod demo;
pub mod lobby;

use lobby::{Lobby, Upsert};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracker_cache::Cache;
use tracker_core::{unix_now, Config, LobbySnapshot, Platform, PlayerId, Relation, Team};
use tracker_parser::{parse_stats_message, LogParser, ParsedEvent};
use tracker_scraper::{
    FetchOptions, Fetcher, HybridFetcher, Priority, ScrapeError, SourceFetcher, TrackerQueue,
    TrnApiSource, WebviewDriver, WebviewSource,
};
use tracker_server::ServerHandle;
use tracker_watcher::WatchEvent;
use tracker_heuristics::EntryState;

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
    /// Un message par coup d'envoi (fin du décompte), pour masquer l'overlay.
    kickoffs: broadcast::Sender<()>,
    webview: Option<WebviewSource>,
    parser: LogParser,
    /// Compte local lu dans `Launch.log` ; les champs de la config le surchargent.
    detected_self: Mutex<Option<(PlayerId, Option<String>)>>,
    /// Affiche un lobby fictif à la place du vrai (bouton « Prévisualiser »).
    preview: AtomicBool,
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
        let (kickoffs, _) = broadcast::channel(8);
        let inner = Arc::new(Inner {
            cfg: RwLock::new(cfg),
            cache,
            queue: TrackerQueue::new(),
            lobby: Mutex::new(Lobby::default()),
            snapshots,
            kickoffs,
            webview,
            parser: LogParser::new(),
            detected_self: Mutex::new(None),
            preview: AtomicBool::new(false),
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

    /// Notifie chaque coup d'envoi détecté par la Stats API.
    pub fn subscribe_kickoff(&self) -> broadcast::Receiver<()> {
        self.inner.kickoffs.subscribe()
    }

    pub fn snapshot(&self) -> LobbySnapshot {
        self.inner.build_snapshot()
    }

    pub fn config(&self) -> Config {
        self.inner.cfg.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// Active ou coupe la prévisualisation de l'overlay avec un lobby fictif.
    pub fn set_preview(&self, enabled: bool) {
        self.inner.preview.store(enabled, Ordering::SeqCst);
        self.inner.emit();
    }

    pub fn is_preview(&self) -> bool {
        self.inner.preview.load(Ordering::SeqCst)
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
    ///
    /// Un problème non bloquant (port du serveur local déjà pris, par exemple) n'empêche pas la
    /// détection du joueur ni la surveillance du log : il est renvoyé comme avertissement.
    pub async fn start(self: &Arc<Self>) -> Result<Vec<String>, EngineError> {
        let cfg = self.config();
        let inner = self.inner.clone();
        let mut warnings = Vec::new();

        // Le jeu est souvent déjà lancé : on retrouve le compte local dans l'historique du log.
        if let Some(path) = cfg.effective_log_path() {
            inner.bootstrap_self_from(&path);
        }

        // Serveur local (OBS + ingest WebView).
        let ingest_src = inner.webview.clone();
        let ingest: tracker_server::IngestFn =
            Arc::new(move |id, body| ingest_src.as_ref().is_some_and(|w| w.submit(id, body)));
        match tracker_server::start(cfg.server_port, inner.snapshots.clone(), ingest).await {
            Ok(server) => *self.server.lock().unwrap() = Some(server),
            Err(e) => {
                tracing::error!("{e}");
                warnings.push(format!(
                    "{e}. LobbyTracker est peut-être déjà ouvert : l'overlay OBS et le scraper WebView sont indisponibles."
                ));
            }
        }

        let mut tasks = Vec::new();

        // File d'attente + worker réseau.
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let i = inner.clone();
        let options: Arc<dyn Fn() -> FetchOptions + Send + Sync> = Arc::new(move || FetchOptions {
            with_sessions: i
                .cfg
                .read()
                .map(|c| c.filters.on_tilt || c.filters.smurf_suspect)
                .unwrap_or(false),
            lookup: None,
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
        Ok(warnings)
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
            WatchEvent::LogReset => {
                // Rocket League vient d'être (re)lancé : le compte peut avoir changé
                // (Epic <-> Steam), on oublie l'ancien jusqu'à la prochaine connexion.
                tracing::info!("nouveau lancement de Rocket League détecté");
                *self.detected_self.lock().unwrap() = None;
                self.queue.clear();
                self.lobby.lock().unwrap().reset();
                self.emit();
                return;
            }
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

    /// Si le jeu tourne déjà (log modifié très récemment), retrouve le compte local dans le log.
    /// Un log ancien appartient à une session terminée : le compte a pu changer depuis, on
    /// attend alors le prochain lancement du jeu.
    fn bootstrap_self_from(&self, path: &Path) {
        const MAX_BYTES: u64 = 64 * 1024 * 1024;
        const RUNNING_WINDOW: Duration = Duration::from_secs(120);
        let Ok(meta) = std::fs::metadata(path) else { return };
        if meta.len() > MAX_BYTES {
            return;
        }
        let recent = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age <= RUNNING_WINDOW);
        if !recent {
            return;
        }
        let Ok(bytes) = std::fs::read(path) else { return };
        let text = String::from_utf8_lossy(&bytes);
        let last = text.lines().filter_map(|l| self.parser.parse_local_player(l)).last();
        if let Some(ev) = last {
            self.handle_parsed(ev);
        }
    }

    fn handle_parsed(&self, ev: ParsedEvent) {
        match ev {
            ParsedEvent::LocalPlayer { id, name } => {
                tracing::info!("joueur local détecté : {} ({id})", name.as_deref().unwrap_or("?"));
                self.lobby.lock().unwrap().remove(&id);
                *self.detected_self.lock().unwrap() = Some((id, name));
                self.emit();
            }
            ParsedEvent::Kickoff => {
                let _ = self.kickoffs.send(());
            }
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
        if let Some((did, dname)) = self.detected_self.lock().unwrap().as_ref() {
            let by_detected_name = matches!((dname.as_deref(), name),
                (Some(me), Some(n)) if me.eq_ignore_ascii_case(n));
            if did == id || by_detected_name {
                return true;
            }
        }
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

        let lookup = name.clone().filter(|n| !n.is_empty());
        let (state, relation, loading) = {
            let mut lobby = self.lobby.lock().unwrap();
            let state = lobby.upsert(&id, name, team);
            let loading = lobby.entries().iter().any(|e| e.id == id && matches!(e.state, EntryState::Loading));
            (state, lobby.relation_for(&id), loading)
        };
        if state == Upsert::Unchanged {
            return;
        }
        // Hors Steam, Tracker Network cherche par pseudo : on attend de le connaître.
        let needs_name = id.platform != Platform::Steam;
        if state == Upsert::Changed && loading && lookup.is_some() && needs_name {
            self.queue.enqueue_named(id, priority_of(relation), lookup);
            self.emit();
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
                    if !needs_name || lookup.is_some() {
                        self.queue.enqueue_named(id, priority_of(relation), lookup);
                    }
                }
                Err(e) => {
                    tracing::warn!("lecture du cache impossible : {e}");
                    if !needs_name || lookup.is_some() {
                        self.queue.enqueue_named(id, priority_of(relation), lookup);
                    }
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
        if self.preview.load(Ordering::SeqCst) {
            let entries = demo::demo_entries();
            return LobbySnapshot {
                pending: 1,
                players: tracker_heuristics::build_views(&entries, &cfg),
                updated_at: unix_now(),
                me: self.me_label(&cfg),
            };
        }
        let lobby = self.lobby.lock().unwrap();
        LobbySnapshot {
            players: tracker_heuristics::build_views(lobby.entries(), &cfg),
            pending: lobby.loading_count(),
            updated_at: unix_now(),
            me: self.me_label(&cfg),
        }
    }

    fn me_label(&self, cfg: &Config) -> Option<String> {
        if let Some(n) = cfg.self_name.as_deref().filter(|n| !n.is_empty()) {
            return Some(n.to_string());
        }
        let d = self.detected_self.lock().unwrap();
        d.as_ref().map(|(id, name)| {
            format!("{} · {}", name.as_deref().unwrap_or("?"), id.platform.as_str())
        })
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
            saves_per_match: None,
            goals_per_match: None,
            shots_per_match: None,
            loss_streak: 0,
            recent: None,
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
        assert!(snap.players[0].badge_labels.iter().any(|l| l == "Potentiel smurf"));
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
    fn local_player_is_detected_from_log_history_and_excluded() {
        let e = engine(Config::default());
        let dir = std::env::temp_dir().join(format!("lt-boot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("Launch.log");
        std::fs::write(
            &log,
            "noise\nParty: HandleLocalPlayerLoginStatusChanged PlayerName=neyor. PlayerID=Epic|6897812333f244858dddff1fbb06554f|0 LoginStatus=LS_LoggedIn IsPrimary=True IsInParty=False\n",
        )
        .unwrap();
        e.inner.bootstrap_self_from(&log);
        assert_eq!(e.snapshot().me.as_deref(), Some("neyor. · epic"));

        // Le joueur local n'apparaît jamais dans le lobby, ni via son id ni via son pseudo.
        let me = PlayerId::new(Platform::Epic, "6897812333f244858dddff1fbb06554f");
        e.inject_player(me, None, Some(Team::Blue));
        e.inject_player(id(7), Some("neyor.".into()), Some(Team::Blue));
        assert!(e.snapshot().players.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_log_is_ignored_and_new_launch_replaces_the_account() {
        const LOGIN: &str = "Party: HandleLocalPlayerLoginStatusChanged PlayerName=neyor. PlayerID=Epic|6897812333f244858dddff1fbb06554f|0 LoginStatus=LS_LoggedIn IsPrimary=True IsInParty=False";
        let e = engine(Config::default());
        let dir = std::env::temp_dir().join(format!("lt-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("Launch.log");
        std::fs::write(&log, format!("{LOGIN}\n")).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::OpenOptions::new().write(true).open(&log).unwrap().set_modified(old).unwrap();

        // Jeu fermé depuis une heure : on n'affiche pas un compte peut-être périmé.
        e.inner.bootstrap_self_from(&log);
        assert_eq!(e.snapshot().me, None);

        // Nouveau lancement avec un autre compte (Steam).
        e.inner.handle_watch_event(WatchEvent::LogReset);
        e.inner.handle_watch_event(WatchEvent::LogLine(LOGIN.into()));
        assert_eq!(e.snapshot().me.as_deref(), Some("neyor. · epic"));
        e.inner.handle_watch_event(WatchEvent::LogReset);
        assert_eq!(e.snapshot().me, None, "le relancement efface l'ancien compte");
        e.inner.handle_watch_event(WatchEvent::LogLine(
            "Party: HandleLocalPlayerLoginStatusChanged PlayerName=SteamMe PlayerID=Steam|76561198012345678|0 LoginStatus=LS_LoggedIn IsPrimary=True".into(),
        ));
        assert_eq!(e.snapshot().me.as_deref(), Some("SteamMe · steam"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_shows_demo_badges_respects_filters_and_restores_real_lobby() {
        let mut cfg = Config::default();
        let e = engine(cfg.clone());
        e.inject_player(id(1), Some("Réel".into()), None);
        e.set_preview(true);
        let snap = e.snapshot();
        assert!(snap.players.iter().any(|p| p.name == "BumpMachine"));
        let all: Vec<_> = snap.players.iter().flat_map(|p| p.badge_labels.clone()).collect();
        for l in ["Potentiel smurf", "Joueur 1v1", "Gardien", "Lose streak", "Ancien haut rang"] {
            let l = l.to_string();
            assert!(all.iter().any(|x| x.starts_with(&l)), "badge démo manquant : {l}");
        }
        cfg.filters.on_tilt = false;
        e.update_config(cfg);
        assert!(!e.snapshot().players.iter().any(|p| p.badge_labels.iter().any(|l| l.starts_with("Lose streak"))));
        e.set_preview(false);
        let real = e.snapshot();
        assert_eq!(real.players.len(), 1);
        assert_eq!(real.players[0].name, "Réel");
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
