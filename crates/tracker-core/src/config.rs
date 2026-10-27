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
            defensive_anchor: true,
            on_tilt: true,
            peak_disparity: true,
            only_opponents: false,
        }
    }
}

/// Port du serveur local (OBS). Le 49124 est réservé par la Stats API du jeu (`WebPort`).
pub const DEFAULT_SERVER_PORT: u16 = 49125;
const OLD_SERVER_PORT: u16 = 49124;

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

/// Couleurs de l'overlay, au format `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayColors {
    pub background: String,
    pub text: String,
    pub muted: String,
    /// Liseré des adversaires, des coéquipiers et des joueurs d'équipe inconnue.
    pub opponent: String,
    pub teammate: String,
    pub unknown: String,
    pub badge_smurf: String,
    pub badge_one_v_one: String,
    pub badge_anchor: String,
    pub badge_tilt: String,
    pub badge_peak: String,
}

impl Default for OverlayColors {
    fn default() -> Self {
        let s = |v: &str| v.to_string();
        Self {
            background: s("#0c0e14"),
            text: s("#ffffff"),
            muted: s("#9aa4b8"),
            opponent: s("#ff7a2b"),
            teammate: s("#3b9dff"),
            unknown: s("#888888"),
            badge_smurf: s("#e03a3a"),
            badge_one_v_one: s("#7b2cff"),
            badge_anchor: s("#7b2cff"),
            badge_tilt: s("#6b7280"),
            badge_peak: s("#b7791f"),
        }
    }
}

/// Quand l'overlay se masque tout seul après l'arrivée d'un lobby.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoHideMode {
    /// Jamais : l'overlay reste affiché.
    #[default]
    Off,
    /// Dès la fin du décompte 3-2-1 (événement `RoundStarted` de la Stats API).
    Kickoff,
    /// Après `auto_hide_seconds` secondes.
    Timer,
}

/// Emplacement de l'overlay : libre (glisser-déposer) ou accroché à un coin / bord de l'écran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayAnchor {
    #[default]
    Free,
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl OverlayAnchor {
    /// Position (coin haut-gauche de la fenêtre) en pixels physiques, ou `None` si libre.
    /// `monitor` = (x, y, largeur, hauteur), `window` = (largeur, hauteur).
    pub fn position(
        self,
        monitor: (i32, i32, i32, i32),
        window: (i32, i32),
        margin: (i32, i32),
    ) -> Option<(i32, i32)> {
        let (mx, my, mw, mh) = monitor;
        let (ww, wh) = window;
        let left = mx + margin.0;
        let center = mx + (mw - ww) / 2;
        let right = mx + mw - ww - margin.0;
        let top = my + margin.1;
        let bottom = my + mh - wh - margin.1;
        Some(match self {
            OverlayAnchor::Free => return None,
            OverlayAnchor::TopLeft => (left, top),
            OverlayAnchor::TopCenter => (center, top),
            OverlayAnchor::TopRight => (right, top),
            OverlayAnchor::BottomLeft => (left, bottom),
            OverlayAnchor::BottomCenter => (center, bottom),
            OverlayAnchor::BottomRight => (right, bottom),
        })
    }
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
    /// Opacité du fond des cartes, en pourcentage (10 à 100).
    pub overlay_opacity: u32,
    pub overlay_colors: OverlayColors,
    pub overlay_anchor: OverlayAnchor,
    /// Marges par rapport au bord de l'écran, en pixels logiques.
    pub overlay_margin_x: u32,
    pub overlay_margin_y: u32,
    /// Masque l'overlay quelques secondes après l'arrivée des joueurs d'un nouveau lobby.
    pub auto_hide_mode: AutoHideMode,
    pub auto_hide_seconds: u32,
    pub overlay_position: Option<OverlayPosition>,
    /// Jeton API officiel Tracker Network (fallback).
    pub trn_api_key: Option<String>,
    /// Utiliser la WebView2 invisible pour contourner Cloudflare.
    pub use_webview_scraper: bool,
    pub stats_api_enabled: bool,
    pub stats_api_port: u16,
    /// Fréquence d'envoi voulue (`PacketSendRate`) quand LobbyTracker règle la Stats API du jeu.
    pub stats_api_rate: u32,
    /// Vrai après un premier « Réappliquer le réglage » : l'application remet alors le réglage
    /// en place d'elle-même si une mise à jour du jeu l'a effacé.
    pub stats_api_managed: bool,
    /// Dossier de Rocket League choisi à la main, si la détection automatique ne le trouve pas.
    pub rocket_league_dir: Option<PathBuf>,
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
            overlay_opacity: 82,
            overlay_colors: OverlayColors::default(),
            overlay_anchor: OverlayAnchor::default(),
            overlay_margin_x: 24,
            overlay_margin_y: 24,
            auto_hide_mode: AutoHideMode::Off,
            auto_hide_seconds: 30,
            overlay_position: None,
            trn_api_key: None,
            use_webview_scraper: true,
            stats_api_enabled: true,
            stats_api_port: 49123,
            stats_api_rate: 10,
            stats_api_managed: false,
            rocket_league_dir: None,
            server_port: DEFAULT_SERVER_PORT,
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
            Ok(s) => {
                let mut cfg: Self = serde_json::from_str(&s)?;
                // Ancienne valeur par défaut, en conflit avec le `WebPort` du jeu.
                if cfg.server_port == OLD_SERVER_PORT {
                    cfg.server_port = DEFAULT_SERVER_PORT;
                }
                Ok(cfg)
            }
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
    fn anchors_compute_positions_on_a_monitor() {
        let mon = (0, 0, 1920, 1080);
        let win = (400, 200);
        let m = (24, 24);
        assert_eq!(OverlayAnchor::Free.position(mon, win, m), None);
        assert_eq!(OverlayAnchor::TopLeft.position(mon, win, m), Some((24, 24)));
        assert_eq!(OverlayAnchor::TopCenter.position(mon, win, m), Some((760, 24)));
        assert_eq!(OverlayAnchor::TopRight.position(mon, win, m), Some((1496, 24)));
        assert_eq!(OverlayAnchor::BottomLeft.position(mon, win, m), Some((24, 856)));
        assert_eq!(OverlayAnchor::BottomRight.position(mon, win, m), Some((1496, 856)));
        // Deuxième écran positionné à droite du premier.
        assert_eq!(OverlayAnchor::TopLeft.position((1920, 0, 1280, 720), win, m), Some((1944, 24)));
    }

    #[test]
    fn old_default_server_port_is_migrated() {
        let dir = std::env::temp_dir().join(format!("lt-port-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"server_port":49124}"#).unwrap();
        assert_eq!(Config::load(&path).unwrap().server_port, DEFAULT_SERVER_PORT);
        std::fs::write(&path, r#"{"server_port":50000}"#).unwrap();
        assert_eq!(Config::load(&path).unwrap().server_port, 50000);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_gives_default() {
        let c = Config::load(Path::new("definitely/not/here.json")).unwrap();
        assert_eq!(c, Config::default());
    }
}
