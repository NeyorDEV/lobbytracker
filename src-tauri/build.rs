fn main() {
    // Les commandes applicatives doivent être déclarées pour pouvoir être autorisées
    // (y compris `submit_scrape`, appelée depuis la WebView masquée de tracker.gg).
    let manifest = tauri_build::AppManifest::new().commands(&[
        "get_config",
        "save_config",
        "get_snapshot",
        "default_log_path",
        "set_click_through",
        "toggle_overlay",
        "set_preview",
        "submit_scrape",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("échec de tauri-build");
}
