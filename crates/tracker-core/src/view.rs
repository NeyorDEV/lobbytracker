use crate::model::{PlaylistKind, Relation, Team};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Badge {
    SmurfSuspect,
    OneVOneSpecialist,
    Demolisher,
    DefensiveAnchor,
    OnTilt,
    PeakDisparity,
}

impl Badge {
    pub fn label_fr(self) -> &'static str {
        match self {
            Badge::SmurfSuspect => "Smurf suspect",
            Badge::OneVOneSpecialist => "Spécialiste 1v1",
            Badge::Demolisher => "Démolisseur",
            Badge::DefensiveAnchor => "Ancre défensive",
            Badge::OnTilt => "En tilt",
            Badge::PeakDisparity => "Ancien haut rang",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerStatus {
    Loading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankView {
    pub playlist: PlaylistKind,
    pub label: String,
    pub tier: u32,
    pub tier_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerView {
    pub key: String,
    pub name: String,
    pub team: Team,
    pub relation: Relation,
    pub status: PlayerStatus,
    /// 0-100, 0 tant que le profil n'est pas chargé.
    pub smurf_index: u32,
    pub badges: Vec<Badge>,
    pub badge_labels: Vec<String>,
    pub ranks: Vec<RankView>,
    pub best_tier_name: String,
    pub total_wins: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LobbySnapshot {
    pub players: Vec<PlayerView>,
    /// Nombre de joueurs encore en cours de résolution.
    pub pending: usize,
    pub updated_at: i64,
    /// Joueur local détecté ou configuré (ex. « neyor. · epic »).
    #[serde(default)]
    pub me: Option<String>,
}
