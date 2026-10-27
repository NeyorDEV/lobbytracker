use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("lecture/écriture de la configuration impossible : {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration JSON invalide : {0}")]
    Json(#[from] serde_json::Error),
}

/// Cases à cocher de l'écran de paramètres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub smurf_suspect: bool,
    pub one_v_one_specialist: bool,
    pub demolisher: bool,
    pub defensive_anchor: bool,
    pub on_tilt: bool,
    pub peak_disparity: bool,
    pub only_opponents: bool,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            smurf_suspect: true,
            one_v_one_specialist: true,
            demolisher: true,
            defensive_anchor: true,
            on_tilt: true,
            peak_disparity: true,
            only_opponents: false,
        }
    }
}

/// Disposition de l'overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayLayout {
    /// Lignes compactes empilées.
    Column,
    /// Bande horizontale de puces compactes.
    Row,
    /// Cartes empilées verticalement.
    #[default]
    CardVertical,
    /// Cartes côte à côte.
    CardHorizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayPosition {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub filters: Filters,
    /// Raccourci global afficher/masquer l'overlay (ex. `F9`, `Ctrl+Shift+S`).
    pub overlay_hotkey: String,
    /// Raccourci global activer/désactiver le click-through.
    pub click_through_hotkey: String,
    pub click_through: bool,
    pub overlay_layout: OverlayLayout,
    /// Échelle de l'overlay en pourcentage (50 à 200).
    pub overlay_scale: u32,
    /// Masque l'overlay quelques secondes après l'arrivée des joueurs d'un nouveau lobby.
    pub auto_hide_enabled: bool,
    pub auto_hide_seconds: u32,
    pub overlay_position: Option<OverlayPosition>,
    /// Jeton API officiel Tracker Network (fallback).
    pub trn_api_key: Option<String>,
    /// Utiliser la WebView2 invisible pour contourner Cloudflare.
    pub use_webview_scraper: bool,
    pub stats_api_enabled: bool,
    pub stats_api_port: u16,
    /// Port du serveur local (overlay OBS, WebSocket).
    pub server_port: u16,
    /// Chemin de `Launch.log` si différent du chemin par défaut.
    pub log_path: Option<PathBuf>,
    /// Pseudo / identifiant de l'utilisateur, pour distinguer coéquipiers et adversaires.
    pub self_name: Option<String>,
    pub self_id: Option<String>,
    pub cache_ttl_hours: u64,
    /// Seuil de victoires totales sous lequel un haut rang est suspect.
    pub smurf_max_wins: u32,
    /// Tier minimal (17 = Champion 2) pour l'alerte smurf.
    pub smurf_min_tier: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            filters: Filters::default(),
            overlay_hotkey: "F9".into(),
            click_through_hotkey: "Ctrl+Shift+C".into(),
            click_through: false,
            overlay_layout: OverlayLayout::default(),
            overlay_scale: 100,
            auto_hide_enabled: false,
            auto_hide_seconds: 30,
            overlay_position: None,
            trn_api_key: None,
            use_webview_scraper: true,
            stats_api_enabled: true,
            stats_api_port: 49123,
            server_port: 49124,
            log_path: None,
            self_name: None,
            self_id: None,
            cache_ttl_hours: 24,
            smurf_max_wins: 250,
            smurf_min_tier: crate::model::TIER_C2,
        }
    }
}

impl Config {
    /// Charge la configuration ; un fichier absent donne la configuration par défaut.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// `Documents\My Games\Rocket League\TAGame\Logs\Launch.log`
    pub fn default_log_path() -> Option<PathBuf> {
        dirs::document_dir().map(|d| {
            d.join("My Games").join("Rocket League").join("TAGame").join("Logs").join("Launch.log")
        })
    }

    pub fn effective_log_path(&self) -> Option<PathBuf> {
        self.log_path.clone().or_else(Self::default_log_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_falls_back_to_defaults() {
        let c: Config = serde_json::from_str(r#"{"filters":{"on_tilt":false}}"#).unwrap();
        assert!(!c.filters.on_tilt);
        assert!(c.filters.smurf_suspect);
        assert_eq!(c.overlay_hotkey, "F9");
    }

    #[test]
    fn overlay_layout_roundtrips_as_snake_case() {
        let c: Config = serde_json::from_str(r#"{"overlay_layout":"card_horizontal","overlay_scale":150}"#).unwrap();
        assert_eq!(c.overlay_layout, OverlayLayout::CardHorizontal);
        assert_eq!(c.overlay_scale, 150);
        assert!(serde_json::to_string(&c).unwrap().contains("\"card_horizontal\""));
    }

    #[test]
    fn missing_file_gives_default() {
        let c = Config::load(Path::new("definitely/not/here.json")).unwrap();
        assert_eq!(c, Config::default());
    }
}
