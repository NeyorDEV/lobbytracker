//! Coquille Tauri : fenêtres (paramètres + overlay transparent), raccourcis globaux,
//! WebView masquée pour le scraping. Toute la logique métier vit dans `tracker-engine`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tracker_core::{Config, LobbySnapshot, OverlayPosition};
use tracker_engine::Engine;
use tracker_scraper::WebviewDriver;

const OVERLAY: &str = "overlay";

struct AppState {
    engine: Arc<Engine>,
    config_path: PathBuf,
    move_generation: AtomicU64,
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
    apply_click_through(&app, config.click_through);
    register_shortcuts(&app, &config)
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
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            get_snapshot,
            default_log_path,
            set_click_through,
            toggle_overlay,
            submit_scrape
        ])
        .on_window_event(|window, event| match event {
            WindowEvent::Moved(pos) if window.label() == OVERLAY => {
                schedule_position_save(window.app_handle(), *pos);
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
            tauri::async_runtime::spawn(async move {
                if let Err(e) = engine.start().await {
                    tracing::error!("démarrage du moteur : {e}");
                    let _ = handle.emit("engine-error", e.to_string());
                }
                loop {
                    match rx.recv().await {
                        Ok(snap) => {
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
