//! Coquille Tauri : fenêtres (paramètres + overlay transparent), raccourcis globaux,
//! WebView masquée pour le scraping. Toute la logique métier vit dans `tracker-engine`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tracker_core::{AutoHideMode, Config, LobbySnapshot, OverlayAnchor, OverlayPosition};
use tracker_engine::Engine;
use tracker_scraper::WebviewDriver;

const OVERLAY: &str = "overlay";

struct AppState {
    engine: Arc<Engine>,
    config_path: PathBuf,
    move_generation: AtomicU64,
    preview_generation: AtomicU64,
    autohide_generation: AtomicU64,
    /// Vrai entre l'arrivée d'un lobby et son premier coup d'envoi.
    awaiting_kickoff: AtomicBool,
}

/// Fenêtre WebView2 masquée, créée par requête et détruite juste après.
struct HiddenWebviewDriver(AppHandle);

fn scrape_label(request_id: &str) -> String {
    format!("scrape-{}", &request_id[..request_id.len().min(12)])
}

impl WebviewDriver for HiddenWebviewDriver {
    fn open(&self, request_id: &str, url: &str, init_script: &str) -> Result<(), String> {
        let url = url.parse().map_err(|e| format!("URL invalide : {e}"))?;
        WebviewWindowBuilder::new(&self.0, scrape_label(request_id), WebviewUrl::External(url))
            .visible(false)
            .skip_taskbar(true)
            .focused(false)
            .inner_size(800.0, 600.0)
            .initialization_script(init_script)
            .build()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn close(&self, request_id: &str) {
        if let Some(w) = self.0.get_webview_window(&scrape_label(request_id)) {
            let _ = w.destroy();
        }
    }
}

// ---------------------------------------------------------------- commandes

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    state.engine.config()
}

#[tauri::command]
fn save_config(app: AppHandle, state: State<'_, AppState>, config: Config) -> Result<(), String> {
    config.save(&state.config_path).map_err(|e| e.to_string())?;
    state.engine.update_config(config.clone());
    let _ = app.emit("config-changed", &config);
    apply_anchor(&app);
    apply_click_through(&app, config.click_through);
    register_shortcuts(&app, &config)
}

#[derive(serde::Serialize)]
struct UpdateInfo {
    version: String,
    notes: Option<String>,
}

/// Cherche une version plus récente sur GitHub Releases (`latest.json` signé).
#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateInfo { version: u.version, notes: u.body }))
}

/// Télécharge, vérifie la signature, installe puis relance l'application.
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let Some(update) = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    app.restart()
}

/// État de la Stats API du jeu, pour la section « Stats API de Rocket League ».
#[derive(serde::Serialize)]
struct StatsApiStatus {
    installs: Vec<tracker_gamecfg::Install>,
    /// Fréquence la plus haute parmi les installations ; 0 = désactivée partout.
    active_rate: u32,
    configured_rate: u32,
    managed: bool,
    /// Lanceur du compte actuellement connecté dans le jeu (`epic`, `steam`…), s'il tourne.
    running: Option<String>,
}

fn statsapi_status_of(state: &AppState) -> StatsApiStatus {
    let cfg = state.engine.config();
    let installs = tracker_gamecfg::detect(cfg.rocket_league_dir.as_deref());
    let active_rate = installs.iter().filter_map(|i| i.rate).max().unwrap_or(0);
    let running = state
        .engine
        .snapshot()
        .me
        .and_then(|m| m.rsplit(" · ").next().map(str::to_owned));
    StatsApiStatus { installs, active_rate, configured_rate: cfg.stats_api_rate, managed: cfg.stats_api_managed, running }
}

#[tauri::command]
async fn statsapi_status(app: AppHandle) -> Result<StatsApiStatus, String> {
    // Lecture de fichiers et appel à `reg` : hors du thread principal de l'interface.
    tauri::async_runtime::spawn_blocking(move || statsapi_status_of(&app.state::<AppState>()))
        .await
        .map_err(|e| e.to_string())
}

/// Écrit la fréquence choisie dans chaque installation détectée (action explicite de l'utilisateur).
#[tauri::command]
async fn statsapi_apply(app: AppHandle, rate: u32) -> Result<StatsApiStatus, String> {
    tauri::async_runtime::spawn_blocking(move || statsapi_apply_blocking(&app.state::<AppState>(), rate))
        .await
        .map_err(|e| e.to_string())?
}

fn statsapi_apply_blocking(state: &AppState, rate: u32) -> Result<StatsApiStatus, String> {
    let rate = rate.clamp(1, tracker_gamecfg::MAX_RATE);
    let mut cfg = state.engine.config();
    cfg.stats_api_rate = rate;
    cfg.stats_api_managed = true;
    cfg.save(&state.config_path).map_err(|e| e.to_string())?;
    state.engine.update_config(cfg.clone());

    let installs = tracker_gamecfg::detect(cfg.rocket_league_dir.as_deref());
    if installs.is_empty() {
        return Err("Aucune installation de Rocket League trouvée : choisissez le dossier du jeu.".into());
    }
    let mut errors = Vec::new();
    for i in &installs {
        if let Err(e) = tracker_gamecfg::apply_rate(&i.root, rate) {
            errors.push(e.to_string());
        }
    }
    if !errors.is_empty() {
        return Err(errors.join(" "));
    }
    Ok(statsapi_status_of(state))
}

/// Ouvre le sélecteur de dossier et mémorise l'installation choisie.
#[tauri::command]
async fn statsapi_pick_dir(app: AppHandle) -> Result<Option<StatsApiStatus>, String> {
    use tauri_plugin_dialog::DialogExt;
    let Some(picked) = app.dialog().file().blocking_pick_folder() else { return Ok(None) };
    let dir = picked.into_path().map_err(|e| e.to_string())?;
    if !tracker_gamecfg::is_install(&dir) {
        return Err(format!("« {} » ne contient pas Rocket League (dossier TAGame introuvable).", dir.display()));
    }
    let state = app.state::<AppState>();
    let mut cfg = state.engine.config();
    cfg.rocket_league_dir = Some(dir);
    cfg.save(&state.config_path).map_err(|e| e.to_string())?;
    state.engine.update_config(cfg);
    Ok(Some(statsapi_status_of(&state)))
}

/// Si l'utilisateur a déjà activé le réglage automatique et qu'une mise à jour du jeu l'a effacé,
/// le remet en place. Renvoie vrai si un fichier a été modifié.
fn ensure_stats_api(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let cfg = state.engine.config();
    if !cfg.stats_api_managed || !cfg.stats_api_enabled {
        return false;
    }
    let mut changed = false;
    for i in tracker_gamecfg::detect(cfg.rocket_league_dir.as_deref()) {
        if i.rate.unwrap_or(0) == 0 {
            match tracker_gamecfg::apply_rate(&i.root, cfg.stats_api_rate) {
                Ok(c) => changed |= c,
                Err(e) => tracing::warn!("Stats API : {e}"),
            }
        }
    }
    changed
}

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> LobbySnapshot {
    state.engine.snapshot()
}

#[tauri::command]
fn default_log_path() -> Option<String> {
    Config::default_log_path().map(|p| p.display().to_string())
}

#[tauri::command]
fn set_click_through(app: AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    persist_click_through(&app, &state, enabled)
}

#[tauri::command]
fn toggle_overlay(app: AppHandle) {
    toggle_overlay_visibility(&app);
}

/// Affiche un lobby fictif sur l'overlay (et OBS) pendant 20 s, ou l'arrête.
#[tauri::command]
fn set_preview(app: AppHandle, state: State<'_, AppState>, enabled: bool) {
    let generation = state.preview_generation.fetch_add(1, Ordering::SeqCst) + 1;
    state.engine.set_preview(enabled);
    if !enabled {
        return;
    }
    if let Some(w) = app.get_webview_window(OVERLAY) {
        let _ = w.show();
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        let state = app.state::<AppState>();
        if state.preview_generation.load(Ordering::SeqCst) == generation {
            state.engine.set_preview(false);
            let _ = app.emit("preview-ended", ());
        }
    });
}

/// Seule commande exposée aux fenêtres masquées (`api.tracker.gg`).
#[tauri::command]
fn submit_scrape(state: State<'_, AppState>, request_id: String, body: String) -> bool {
    state.engine.ingest(&request_id, body)
}

// ------------------------------------------------------------------ overlay

fn apply_click_through(app: &AppHandle, enabled: bool) {
    if let Some(w) = app.get_webview_window(OVERLAY) {
        if let Err(e) = w.set_ignore_cursor_events(enabled) {
            tracing::warn!("click-through impossible : {e}");
        }
    }
    let _ = app.emit("click-through", enabled);
}

fn persist_click_through(app: &AppHandle, state: &AppState, enabled: bool) -> Result<(), String> {
    let mut cfg = state.engine.config();
    cfg.click_through = enabled;
    cfg.save(&state.config_path).map_err(|e| e.to_string())?;
    state.engine.update_config(cfg);
    apply_click_through(app, enabled);
    Ok(())
}

fn toggle_overlay_visibility(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(OVERLAY) {
        let visible = w.is_visible().unwrap_or(true);
        let _ = if visible { w.hide() } else { w.show() };
    }
}

fn register_shortcuts(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let gs = app.global_shortcut();
    gs.unregister_all().map_err(|e| e.to_string())?;

    gs.on_shortcut(cfg.overlay_hotkey.as_str(), |app, _, ev| {
        if ev.state() == ShortcutState::Pressed {
            toggle_overlay_visibility(app);
        }
    })
    .map_err(|e| format!("raccourci « {} » : {e}", cfg.overlay_hotkey))?;

    if cfg.click_through_hotkey != cfg.overlay_hotkey {
        gs.on_shortcut(cfg.click_through_hotkey.as_str(), |app, _, ev| {
            if ev.state() == ShortcutState::Pressed {
                let state = app.state::<AppState>();
                let next = !state.engine.config().click_through;
                if let Err(e) = persist_click_through(app, &state, next) {
                    tracing::warn!("{e}");
                }
            }
        })
        .map_err(|e| format!("raccourci « {} » : {e}", cfg.click_through_hotkey))?;
    }
    Ok(())
}

/// Place l'overlay sur l'emplacement choisi (coin ou bord de l'écran où il se trouve).
fn apply_anchor(app: &AppHandle) {
    let state = app.state::<AppState>();
    let cfg = state.engine.config();
    if cfg.overlay_anchor == OverlayAnchor::Free {
        return;
    }
    let Some(w) = app.get_webview_window(OVERLAY) else { return };
    let (Ok(Some(mon)), Ok(size)) = (w.current_monitor(), w.outer_size()) else { return };
    let sf = mon.scale_factor();
    let (mp, ms) = (mon.position(), mon.size());
    let margin = ((f64::from(cfg.overlay_margin_x) * sf) as i32, (f64::from(cfg.overlay_margin_y) * sf) as i32);
    if let Some((x, y)) = cfg.overlay_anchor.position(
        (mp.x, mp.y, ms.width as i32, ms.height as i32),
        (size.width as i32, size.height as i32),
        margin,
    ) {
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
}

/// Un nouveau lobby vient de se remplir : montre l'overlay, puis le masque selon le mode choisi
/// (au coup d'envoi, ou après le délai).
fn on_new_lobby(app: &AppHandle) {
    let state = app.state::<AppState>();
    let cfg = state.engine.config();
    if cfg.auto_hide_mode == AutoHideMode::Off || state.engine.is_preview() {
        return;
    }
    let generation = state.autohide_generation.fetch_add(1, Ordering::SeqCst) + 1;
    state.awaiting_kickoff.store(cfg.auto_hide_mode == AutoHideMode::Kickoff, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window(OVERLAY) {
        let _ = w.show();
    }
    if cfg.auto_hide_mode != AutoHideMode::Timer {
        return;
    }
    let app = app.clone();
    let delay = Duration::from_secs(u64::from(cfg.auto_hide_seconds.clamp(5, 600)));
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        let state = app.state::<AppState>();
        if state.autohide_generation.load(Ordering::SeqCst) == generation && !state.engine.is_preview() {
            if let Some(w) = app.get_webview_window(OVERLAY) {
                let _ = w.hide();
            }
        }
    });
}

/// Coup d'envoi (fin du décompte) : masque l'overlay tout de suite, une seule fois par lobby.
fn on_kickoff(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.engine.config().auto_hide_mode != AutoHideMode::Kickoff || state.engine.is_preview() {
        return;
    }
    if !state.awaiting_kickoff.swap(false, Ordering::SeqCst) {
        return;
    }
    tracing::info!("coup d'envoi détecté : overlay masqué");
    // Annule le masquage différé : l'overlay est déjà caché.
    state.autohide_generation.fetch_add(1, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window(OVERLAY) {
        let _ = w.hide();
    }
}

/// Sauvegarde la position de l'overlay 600 ms après le dernier déplacement.
fn schedule_position_save(app: &AppHandle, pos: PhysicalPosition<i32>) {
    let state = app.state::<AppState>();
    let generation = state.move_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let state = app.state::<AppState>();
        if state.move_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let mut cfg = state.engine.config();
        if cfg.overlay_anchor != OverlayAnchor::Free {
            return; // position pilotée par l'ancrage, pas par le glisser-déposer
        }
        cfg.overlay_position = Some(OverlayPosition { x: pos.x, y: pos.y });
        if let Err(e) = cfg.save(&state.config_path) {
            tracing::warn!("position de l'overlay non sauvegardée : {e}");
        }
        state.engine.update_config(cfg);
    });
}

// --------------------------------------------------------------------- run

pub fn run() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            get_snapshot,
            default_log_path,
            set_click_through,
            toggle_overlay,
            set_preview,
            submit_scrape,
            check_update,
            install_update,
            statsapi_status,
            statsapi_apply,
            statsapi_pick_dir
        ])
        .on_window_event(|window, event| match event {
            WindowEvent::Moved(pos) if window.label() == OVERLAY => {
                schedule_position_save(window.app_handle(), *pos);
            }
            // La fenêtre se redimensionne selon le contenu : on la replace sur son ancrage.
            WindowEvent::Resized(_) if window.label() == OVERLAY => {
                apply_anchor(window.app_handle());
            }
            WindowEvent::CloseRequested { .. } if window.label() == "main" => {
                // Détruit d'abord overlay et fenêtres de scraping pour que WebView2 libère
                // proprement ses classes de fenêtre avant la sortie du processus.
                let app = window.app_handle();
                for (label, w) in app.webview_windows() {
                    if label != "main" {
                        let _ = w.destroy();
                    }
                }
                app.exit(0);
            }
            _ => {}
        })
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let config_path = dir.join("config.json");
            let config = Config::load(&config_path).unwrap_or_else(|e| {
                tracing::warn!("configuration illisible, valeurs par défaut utilisées : {e}");
                Config::default()
            });

            let driver: Arc<dyn WebviewDriver> = Arc::new(HiddenWebviewDriver(app.handle().clone()));
            let engine = Engine::new(config.clone(), &dir.join("cache.db"), Some(driver))?;
            app.manage(AppState {
                engine: engine.clone(),
                config_path,
                move_generation: AtomicU64::new(0),
                preview_generation: AtomicU64::new(0),
                autohide_generation: AtomicU64::new(0),
                awaiting_kickoff: AtomicBool::new(false),
            });

            if let Some(w) = app.get_webview_window(OVERLAY) {
                if let Some(p) = config.overlay_position {
                    let _ = w.set_position(PhysicalPosition::new(p.x, p.y));
                }
            }
            apply_click_through(app.handle(), config.click_through);
            if let Err(e) = register_shortcuts(app.handle(), &config) {
                tracing::warn!("{e}");
            }

            // Moteur + pont snapshots → événements Tauri.
            let handle = app.handle().clone();
            let mut rx = engine.subscribe();
            let mut kickoff_rx = engine.subscribe_kickoff();
            let kickoff_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match kickoff_rx.recv().await {
                        Ok(()) => on_kickoff(&kickoff_handle),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            tauri::async_runtime::spawn(async move {
                match engine.start().await {
                    Ok(warnings) => {
                        for w in warnings {
                            let _ = handle.emit("engine-error", w);
                        }
                    }
                    Err(e) => {
                        tracing::error!("démarrage du moteur : {e}");
                        let _ = handle.emit("engine-error", e.to_string());
                    }
                }
                // Jeu fermé : on peut remettre le réglage sans qu'un redémarrage soit nécessaire.
                ensure_stats_api(&handle);
                let mut had_players = false;
                let mut had_account = false;
                loop {
                    match rx.recv().await {
                        Ok(snap) => {
                            let has_account = snap.me.is_some();
                            if has_account && !had_account && ensure_stats_api(&handle) {
                                let _ = handle.emit(
                                    "engine-error",
                                    "Stats API réactivée après une mise à jour du jeu : relancez Rocket League pour la prendre en compte.",
                                );
                            }
                            had_account = has_account;
                            let has_players = !snap.players.is_empty();
                            if has_players && !had_players {
                                on_new_lobby(&handle);
                            }
                            had_players = has_players;
                            let _ = handle.emit("lobby-snapshot", snap);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("erreur au lancement de LobbyTracker");
}
