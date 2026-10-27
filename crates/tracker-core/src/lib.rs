//! Types partagés par tous les crates LobbyTracker : identifiants, profils,
//! configuration utilisateur et vues envoyées à l'overlay / OBS.

pub mod config;
pub mod model;
pub mod view;

pub use config::{Config, ConfigError, Filters, OverlayAnchor, OverlayLayout, OverlayPosition};
pub use model::{
    tier_name, PlayerId, PlayerProfile, Platform, PlaylistKind, PlaylistStat, Relation, Team,
    TIER_C2,
};
pub use view::{Badge, LobbySnapshot, PlayerStatus, PlayerView, RankView};

use std::time::{SystemTime, UNIX_EPOCH};

/// Secondes Unix actuelles.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
