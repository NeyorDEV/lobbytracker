//! Lobby fictif pour prévisualiser l'overlay : les badges sont calculés par les vraies
//! heuristiques, donc les cases décochées dans les paramètres s'y appliquent aussi.

use tracker_core::{
    PlayerId, PlayerProfile, Platform, PlaylistKind, PlaylistStat, Relation, Team,
};
use tracker_heuristics::{EntryState, LobbyEntry};

fn pl(kind: PlaylistKind, tier: u32, matches: u32) -> PlaylistStat {
    PlaylistStat { kind, tier, mmr: None, matches }
}

#[allow(clippy::too_many_arguments)]
fn profile(
    n: u32,
    name: &str,
    wins: u32,
    peak: Option<u32>,
    playlists: Vec<PlaylistStat>,
    saves: Option<f32>,
    goals: Option<f32>,
    streak: u32,
) -> PlayerProfile {
    PlayerProfile {
        id: id(n),
        display_name: name.into(),
        total_wins: wins,
        peak_tier: peak,
        playlists,
        saves_per_match: saves,
        goals_per_match: goals,
        shots_per_match: None,
        loss_streak: streak,
        recent: None,
        fetched_at: 0,
    }
}

fn id(n: u32) -> PlayerId {
    PlayerId::new(Platform::Steam, format!("7656119800099{n:04}"))
}

fn entry(n: u32, name: &str, relation: Relation, state: EntryState) -> LobbyEntry {
    let team = match relation {
        Relation::Opponent => Team::Orange,
        Relation::Teammate => Team::Blue,
        Relation::Unknown => Team::Unknown,
    };
    LobbyEntry { id: id(n), name: name.into(), team, relation, state }
}

pub fn demo_entries() -> Vec<LobbyEntry> {
    use PlaylistKind::{Doubles, Duel, Standard};
    let ready = |p| EntryState::Ready(Box::new(p));
    vec![
        entry(
            1,
            "Xx_FreshAccount_xX",
            Relation::Opponent,
            ready(profile(
                1,
                "Xx_FreshAccount_xX",
                90,
                None,
                vec![pl(Duel, 20, 40), pl(Doubles, 18, 30), pl(Standard, 17, 20)],
                None,
                None,
                0,
            )),
        ),
        entry(
            2,
            "BumpMachine",
            Relation::Opponent,
            ready(profile(
                2,
                "BumpMachine",
                2100,
                Some(20),
                vec![pl(Doubles, 17, 200), pl(Standard, 17, 300)],
                Some(0.6),
                Some(0.9),
                5,
            )),
        ),
        entry(
            3,
            "LastManStanding",
            Relation::Opponent,
            ready(profile(
                3,
                "LastManStanding",
                3000,
                None,
                vec![pl(Doubles, 16, 400), pl(Standard, 16, 500)],
                Some(2.4),
                Some(0.5),
                0,
            )),
        ),
        entry(
            4,
            "Coéquipier",
            Relation::Teammate,
            ready(profile(
                4,
                "Coéquipier",
                800,
                None,
                vec![pl(Doubles, 15, 100), pl(Standard, 15, 120)],
                Some(0.8),
                Some(0.7),
                0,
            )),
        ),
        entry(5, "Joueur en chargement", Relation::Teammate, EntryState::Loading),
    ]
}
