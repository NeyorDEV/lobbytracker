use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Epic,
    Steam,
    Xbox,
    Psn,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Epic => "epic",
            Platform::Steam => "steam",
            Platform::Xbox => "xbox",
            Platform::Psn => "psn",
        }
    }

    /// Segment de plateforme utilisé par les URL de Tracker Network.
    pub fn tracker_slug(self) -> &'static str {
        match self {
            Platform::Epic => "epic",
            Platform::Steam => "steam",
            Platform::Xbox => "xbl",
            Platform::Psn => "psn",
        }
    }

    pub fn from_str_key(s: &str) -> Option<Self> {
        match s {
            "epic" => Some(Platform::Epic),
            "steam" => Some(Platform::Steam),
            "xbox" => Some(Platform::Xbox),
            "psn" => Some(Platform::Psn),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlayerId {
    pub platform: Platform,
    pub id: String,
}

impl PlayerId {
    /// Les identifiants sont normalisés en minuscules pour servir de clé de cache.
    pub fn new(platform: Platform, id: impl AsRef<str>) -> Self {
        Self { platform, id: id.as_ref().to_ascii_lowercase() }
    }

    /// Clé stable `plateforme:id`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.platform.as_str(), self.id)
    }

    pub fn from_key(key: &str) -> Option<Self> {
        let (p, id) = key.split_once(':')?;
        Some(Self::new(Platform::from_str_key(p)?, id))
    }
}

impl fmt::Display for PlayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Team {
    Blue,
    Orange,
    #[default]
    Unknown,
}

impl Team {
    /// `TeamNum` de la Stats API : 0 = bleu, 1 = orange.
    pub fn from_num(n: i64) -> Self {
        match n {
            0 => Team::Blue,
            1 => Team::Orange,
            _ => Team::Unknown,
        }
    }
}

/// Position d'un joueur par rapport à l'utilisateur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Relation {
    Opponent,
    Teammate,
    Unknown,
}

/// Tiers de rang Rocket League : 0 = non classé, 1 = Bronze 1 … 22 = SSL.
pub const TIER_C2: u32 = 17;

const TIER_NAMES: [&str; 23] = [
    "Unranked", "Bronze 1", "Bronze 2", "Bronze 3", "Silver 1", "Silver 2", "Silver 3", "Gold 1",
    "Gold 2", "Gold 3", "Platinum 1", "Platinum 2", "Platinum 3", "Diamond 1", "Diamond 2",
    "Diamond 3", "Champion 1", "Champion 2", "Champion 3", "Grand Champion 1",
    "Grand Champion 2", "Grand Champion 3", "Supersonic Legend",
];

pub fn tier_name(tier: u32) -> &'static str {
    TIER_NAMES.get(tier as usize).copied().unwrap_or("?")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistKind {
    Duel,
    Doubles,
    Standard,
    Hoops,
    Rumble,
    Dropshot,
    Snowday,
    Tournament,
    Other,
}

impl PlaylistKind {
    /// Reconnaît le nom de playlist renvoyé par Tracker Network
    /// (« Ranked Duel 1v1 », « Ranked Doubles 2v2 », « Hoops », …).
    pub fn from_name(name: &str) -> Self {
        let n = name.to_ascii_lowercase();
        if n.contains("duel") || n.contains("1v1") {
            PlaylistKind::Duel
        } else if n.contains("doubles") || n.contains("2v2") {
            PlaylistKind::Doubles
        } else if n.contains("standard") || n.contains("3v3") {
            PlaylistKind::Standard
        } else if n.contains("hoops") {
            PlaylistKind::Hoops
        } else if n.contains("rumble") {
            PlaylistKind::Rumble
        } else if n.contains("dropshot") {
            PlaylistKind::Dropshot
        } else if n.contains("snow") {
            PlaylistKind::Snowday
        } else if n.contains("tournament") {
            PlaylistKind::Tournament
        } else {
            PlaylistKind::Other
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PlaylistKind::Duel => "duel",
            PlaylistKind::Doubles => "doubles",
            PlaylistKind::Standard => "standard",
            PlaylistKind::Hoops => "hoops",
            PlaylistKind::Rumble => "rumble",
            PlaylistKind::Dropshot => "dropshot",
            PlaylistKind::Snowday => "snowday",
            PlaylistKind::Tournament => "tournament",
            PlaylistKind::Other => "other",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "duel" => PlaylistKind::Duel,
            "doubles" => PlaylistKind::Doubles,
            "standard" => PlaylistKind::Standard,
            "hoops" => PlaylistKind::Hoops,
            "rumble" => PlaylistKind::Rumble,
            "dropshot" => PlaylistKind::Dropshot,
            "snowday" => PlaylistKind::Snowday,
            "tournament" => PlaylistKind::Tournament,
            _ => PlaylistKind::Other,
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            PlaylistKind::Duel => "1v1",
            PlaylistKind::Doubles => "2v2",
            PlaylistKind::Standard => "3v3",
            PlaylistKind::Hoops => "Hoops",
            PlaylistKind::Rumble => "Rumble",
            PlaylistKind::Dropshot => "Dropshot",
            PlaylistKind::Snowday => "Snow",
            PlaylistKind::Tournament => "Tournoi",
            PlaylistKind::Other => "?",
        }
    }

    /// Playlists « standard » dont le rang sert au calcul du rang global.
    pub fn is_core_ranked(self) -> bool {
        matches!(self, PlaylistKind::Duel | PlaylistKind::Doubles | PlaylistKind::Standard)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistStat {
    pub kind: PlaylistKind,
    pub tier: u32,
    pub mmr: Option<u32>,
    pub matches: u32,
}

/// Profil public agrégé d'un joueur (source : Tracker Network).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerProfile {
    pub id: PlayerId,
    pub display_name: String,
    pub total_wins: u32,
    pub peak_tier: Option<u32>,
    pub playlists: Vec<PlaylistStat>,
    pub demos_per_match: Option<f32>,
    pub saves_per_match: Option<f32>,
    pub goals_per_match: Option<f32>,
    pub shots_per_match: Option<f32>,
    /// Défaites consécutives les plus récentes de la session (0 si inconnu).
    pub loss_streak: u32,
    pub fetched_at: i64,
}

impl PlayerProfile {
    pub fn tier_of(&self, kind: PlaylistKind) -> Option<u32> {
        self.playlists.iter().find(|p| p.kind == kind && p.tier > 0).map(|p| p.tier)
    }

    /// Meilleur rang actuel parmi 1v1 / 2v2 / 3v3.
    pub fn best_tier(&self) -> u32 {
        self.playlists
            .iter()
            .filter(|p| p.kind.is_core_ranked())
            .map(|p| p.tier)
            .max()
            .unwrap_or(0)
    }

    /// Meilleur rang actuel en 2v2 / 3v3 uniquement (référence du « lobby »).
    pub fn best_team_tier(&self) -> Option<u32> {
        self.playlists
            .iter()
            .filter(|p| matches!(p.kind, PlaylistKind::Doubles | PlaylistKind::Standard) && p.tier > 0)
            .map(|p| p.tier)
            .max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_key_roundtrip_normalizes_case() {
        let id = PlayerId::new(Platform::Epic, "ABCDEF0123456789ABCDEF0123456789");
        assert_eq!(id.key(), "epic:abcdef0123456789abcdef0123456789");
        assert_eq!(PlayerId::from_key(&id.key()), Some(id));
    }

    #[test]
    fn playlist_names_are_recognised() {
        assert_eq!(PlaylistKind::from_name("Ranked Duel 1v1"), PlaylistKind::Duel);
        assert_eq!(PlaylistKind::from_name("Ranked Standard 3v3"), PlaylistKind::Standard);
        assert_eq!(PlaylistKind::from_name("Hoops"), PlaylistKind::Hoops);
        assert_eq!(PlaylistKind::from_name("Casual"), PlaylistKind::Other);
    }
}
