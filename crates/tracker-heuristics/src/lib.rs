//! Smurf Index et attribution des badges, en respectant les filtres de l'utilisateur.
//!
//! Un badge dont la case est décochée n'est jamais calculé ni exposé : l'overlay ne reçoit
//! aucune trace de la catégorie masquée.

use tracker_core::{
    tier_name, Badge, Config, PlayerId, PlayerProfile, PlayerStatus, PlayerView, PlaylistKind,
    RankView, Relation, Team,
};

/// Écart de tiers entre le 1v1 et le reste du lobby pour déclencher « Spécialiste 1v1 ».
const DUEL_GAP: f32 = 2.0;
const DUEL_MIN_MATCHES: u32 = 10;
/// Valeur de repli de démolitions/match quand le lobby est trop petit pour une moyenne.
const DEFAULT_DEMOS: f32 = 0.5;
const DEMO_RATIO: f32 = 1.6;
const DEMO_FLOOR: f32 = 0.7;
const ANCHOR_SAVES: f32 = 1.6;
const ANCHOR_SAVES_TO_GOALS: f32 = 2.5;
const TILT_STREAK: u32 = 4;
const PEAK_GAP: u32 = 3;
const PEAK_MIN_TIER: u32 = 19;

#[derive(Debug, Clone)]
pub enum EntryState {
    Loading,
    Failed,
    Ready(Box<PlayerProfile>),
}

#[derive(Debug, Clone)]
pub struct LobbyEntry {
    pub id: PlayerId,
    pub name: String,
    pub team: Team,
    pub relation: Relation,
    pub state: EntryState,
}

/// Statistiques du lobby servant de référence « relative ».
#[derive(Debug, Clone, Copy, Default)]
pub struct LobbyContext {
    pub median_team_tier: Option<f32>,
    pub avg_demos: Option<f32>,
}

impl LobbyContext {
    pub fn from_profiles<'a>(profiles: impl IntoIterator<Item = &'a PlayerProfile>) -> Self {
        let mut tiers: Vec<u32> = Vec::new();
        let mut demos: Vec<f32> = Vec::new();
        for p in profiles {
            if let Some(t) = p.best_team_tier() {
                tiers.push(t);
            }
            if let Some(d) = p.demos_per_match {
                demos.push(d);
            }
        }
        tiers.sort_unstable();
        let median_team_tier = match tiers.len() {
            0 | 1 => None,
            n if n % 2 == 1 => Some(tiers[n / 2] as f32),
            n => Some((tiers[n / 2 - 1] + tiers[n / 2]) as f32 / 2.0),
        };
        let avg_demos = (demos.len() >= 3).then(|| demos.iter().sum::<f32>() / demos.len() as f32);
        Self { median_team_tier, avg_demos }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Analysis {
    pub smurf_index: u32,
    pub badges: Vec<Badge>,
}

/// Indice de 0 à 100 : plus un compte a peu de victoires pour son rang, plus il est élevé.
pub fn smurf_index(p: &PlayerProfile, cfg: &Config) -> u32 {
    let max_wins = cfg.smurf_max_wins.max(1) as f32;
    let wins_factor = (1.0 - p.total_wins as f32 / (2.0 * max_wins)).clamp(0.0, 1.0);
    let floor = cfg.smurf_min_tier.saturating_sub(3) as f32;
    let tier_factor = ((p.best_tier() as f32 - floor) / 6.0).clamp(0.0, 1.0);
    (100.0 * (wins_factor * tier_factor).sqrt()).round() as u32
}

pub fn analyze(p: &PlayerProfile, ctx: &LobbyContext, cfg: &Config) -> Analysis {
    let f = &cfg.filters;
    let mut out = Analysis::default();
    let best = p.best_tier();

    if f.smurf_suspect {
        out.smurf_index = smurf_index(p, cfg);
        if p.total_wins < cfg.smurf_max_wins && best >= cfg.smurf_min_tier {
            out.badges.push(Badge::SmurfSuspect);
        }
    }

    if f.one_v_one_specialist {
        let duel = p.playlists.iter().find(|s| s.kind == PlaylistKind::Duel);
        if let Some(d) = duel.filter(|d| d.tier > 0 && d.matches >= DUEL_MIN_MATCHES) {
            let reference = ctx.median_team_tier.or_else(|| p.best_team_tier().map(|t| t as f32));
            if reference.is_some_and(|r| d.tier as f32 >= r + DUEL_GAP) {
                out.badges.push(Badge::OneVOneSpecialist);
            }
        }
    }

    if f.demolisher {
        if let Some(d) = p.demos_per_match {
            let baseline = ctx.avg_demos.unwrap_or(DEFAULT_DEMOS);
            if d >= DEMO_FLOOR && d >= baseline * DEMO_RATIO {
                out.badges.push(Badge::Demolisher);
            }
        }
    }

    if f.defensive_anchor {
        if let Some(s) = p.saves_per_match {
            let goals_ok = p.goals_per_match.is_none_or(|g| s >= g * ANCHOR_SAVES_TO_GOALS);
            if s >= ANCHOR_SAVES && goals_ok {
                out.badges.push(Badge::DefensiveAnchor);
            }
        }
    }

    if f.on_tilt && p.loss_streak >= TILT_STREAK {
        out.badges.push(Badge::OnTilt);
    }

    if f.peak_disparity {
        if let Some(peak) = p.peak_tier {
            if peak >= PEAK_MIN_TIER && best > 0 && peak >= best + PEAK_GAP {
                out.badges.push(Badge::PeakDisparity);
            }
        }
    }
    out
}

/// Construit les vues de l'overlay : filtre « adversaires uniquement », badges et tri.
pub fn build_views(entries: &[LobbyEntry], cfg: &Config) -> Vec<PlayerView> {
    let ctx = LobbyContext::from_profiles(entries.iter().filter_map(|e| match &e.state {
        EntryState::Ready(p) => Some(p.as_ref()),
        _ => None,
    }));

    let mut views: Vec<PlayerView> = entries
        .iter()
        .filter(|e| !(cfg.filters.only_opponents && e.relation == Relation::Teammate))
        .map(|e| view_of(e, &ctx, cfg))
        .collect();

    let rank = |r: Relation| match r {
        Relation::Opponent => 0,
        Relation::Unknown => 1,
        Relation::Teammate => 2,
    };
    views.sort_by(|a, b| {
        rank(a.relation).cmp(&rank(b.relation)).then(b.smurf_index.cmp(&a.smurf_index))
    });
    views
}

fn view_of(e: &LobbyEntry, ctx: &LobbyContext, cfg: &Config) -> PlayerView {
    let mut v = PlayerView {
        key: e.id.key(),
        name: e.name.clone(),
        team: e.team,
        relation: e.relation,
        status: PlayerStatus::Loading,
        smurf_index: 0,
        badges: Vec::new(),
        badge_labels: Vec::new(),
        ranks: Vec::new(),
        best_tier_name: String::new(),
        total_wins: None,
    };
    match &e.state {
        EntryState::Loading => {}
        EntryState::Failed => v.status = PlayerStatus::Failed,
        EntryState::Ready(p) => {
            let a = analyze(p, ctx, cfg);
            v.status = PlayerStatus::Ready;
            v.smurf_index = a.smurf_index;
            v.badge_labels = a.badges.iter().map(|b| b.label_fr().to_string()).collect();
            v.badges = a.badges;
            if !p.display_name.is_empty() {
                v.name = p.display_name.clone();
            }
            v.best_tier_name = tier_name(p.best_tier()).to_string();
            v.total_wins = Some(p.total_wins);
            v.ranks = p
                .playlists
                .iter()
                .filter(|s| s.kind.is_core_ranked() && s.tier > 0)
                .map(|s| RankView {
                    playlist: s.kind,
                    label: s.kind.short_label().to_string(),
                    tier: s.tier,
                    tier_name: tier_name(s.tier).to_string(),
                })
                .collect();
            v.ranks.sort_by_key(|r| match r.playlist {
                PlaylistKind::Duel => 0,
                PlaylistKind::Doubles => 1,
                _ => 2,
            });
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::{Platform, PlaylistStat};

    fn pl(kind: PlaylistKind, tier: u32, matches: u32) -> PlaylistStat {
        PlaylistStat { kind, tier, mmr: None, matches }
    }

    fn base(n: &str) -> PlayerProfile {
        PlayerProfile {
            id: PlayerId::new(Platform::Steam, format!("7656119800000{n}")),
            display_name: n.into(),
            total_wins: 1000,
            peak_tier: None,
            playlists: vec![pl(PlaylistKind::Standard, 14, 100)],
            demos_per_match: None,
            saves_per_match: None,
            goals_per_match: None,
            shots_per_match: None,
            loss_streak: 0,
            fetched_at: 0,
        }
    }

    fn badges(p: &PlayerProfile, cfg: &Config) -> Vec<Badge> {
        analyze(p, &LobbyContext::default(), cfg).badges
    }

    #[test]
    fn smurf_flagged_at_high_rank_low_wins() {
        let mut p = base("1");
        p.total_wins = 120;
        p.playlists = vec![pl(PlaylistKind::Standard, 18, 80)];
        let cfg = Config::default();
        assert!(badges(&p, &cfg).contains(&Badge::SmurfSuspect));
        assert!(smurf_index(&p, &cfg) > 50);
    }

    #[test]
    fn smurf_not_flagged_for_low_rank_or_veteran() {
        let cfg = Config::default();
        let mut low = base("1");
        low.total_wins = 50;
        low.playlists = vec![pl(PlaylistKind::Standard, 12, 30)];
        assert!(!badges(&low, &cfg).contains(&Badge::SmurfSuspect));
        let mut vet = base("2");
        vet.playlists = vec![pl(PlaylistKind::Standard, 20, 900)];
        assert!(!badges(&vet, &cfg).contains(&Badge::SmurfSuspect));
    }

    #[test]
    fn smurf_thresholds_are_configurable() {
        let mut p = base("1");
        p.total_wins = 300;
        p.playlists = vec![pl(PlaylistKind::Doubles, 17, 50)];
        let mut cfg = Config::default();
        assert!(!badges(&p, &cfg).contains(&Badge::SmurfSuspect));
        cfg.smurf_max_wins = 400;
        assert!(badges(&p, &cfg).contains(&Badge::SmurfSuspect));
    }

    #[test]
    fn disabled_filter_hides_badge_and_index() {
        let mut p = base("1");
        p.total_wins = 100;
        p.playlists = vec![pl(PlaylistKind::Standard, 19, 50)];
        let mut cfg = Config::default();
        cfg.filters.smurf_suspect = false;
        let a = analyze(&p, &LobbyContext::default(), &cfg);
        assert!(a.badges.is_empty());
        assert_eq!(a.smurf_index, 0);
    }

    #[test]
    fn one_v_one_specialist_relative_to_lobby() {
        let mut p = base("1");
        p.playlists = vec![pl(PlaylistKind::Duel, 18, 60), pl(PlaylistKind::Standard, 15, 60)];
        let ctx = LobbyContext { median_team_tier: Some(14.0), avg_demos: None };
        let mut cfg = Config::default();
        assert!(analyze(&p, &ctx, &cfg).badges.contains(&Badge::OneVOneSpecialist));
        cfg.filters.one_v_one_specialist = false;
        assert!(!analyze(&p, &ctx, &cfg).badges.contains(&Badge::OneVOneSpecialist));
    }

    #[test]
    fn one_v_one_requires_enough_matches_and_gap() {
        let cfg = Config::default();
        let mut few = base("1");
        few.playlists = vec![pl(PlaylistKind::Duel, 20, 3), pl(PlaylistKind::Standard, 14, 60)];
        assert!(!badges(&few, &cfg).contains(&Badge::OneVOneSpecialist));
        let mut close = base("2");
        close.playlists = vec![pl(PlaylistKind::Duel, 15, 60), pl(PlaylistKind::Standard, 14, 60)];
        assert!(!badges(&close, &cfg).contains(&Badge::OneVOneSpecialist));
    }

    #[test]
    fn demolisher_compares_against_lobby_average() {
        let cfg = Config::default();
        let mut p = base("1");
        p.demos_per_match = Some(1.5);
        let ctx = LobbyContext { median_team_tier: None, avg_demos: Some(0.6) };
        assert!(analyze(&p, &ctx, &cfg).badges.contains(&Badge::Demolisher));
        let ctx_high = LobbyContext { median_team_tier: None, avg_demos: Some(1.4) };
        assert!(!analyze(&p, &ctx_high, &cfg).badges.contains(&Badge::Demolisher));
    }

    #[test]
    fn defensive_anchor_needs_saves_over_goals() {
        let cfg = Config::default();
        let mut p = base("1");
        p.saves_per_match = Some(2.2);
        p.goals_per_match = Some(0.5);
        assert!(badges(&p, &cfg).contains(&Badge::DefensiveAnchor));
        p.goals_per_match = Some(1.5);
        assert!(!badges(&p, &cfg).contains(&Badge::DefensiveAnchor));
    }

    #[test]
    fn tilt_triggers_at_four_losses() {
        let cfg = Config::default();
        let mut p = base("1");
        p.loss_streak = 3;
        assert!(!badges(&p, &cfg).contains(&Badge::OnTilt));
        p.loss_streak = 4;
        assert!(badges(&p, &cfg).contains(&Badge::OnTilt));
    }

    #[test]
    fn peak_disparity_flags_fallen_grand_champion() {
        let cfg = Config::default();
        let mut p = base("1");
        p.peak_tier = Some(20); // GC2
        p.playlists = vec![pl(PlaylistKind::Standard, 16, 100)]; // C1
        assert!(badges(&p, &cfg).contains(&Badge::PeakDisparity));
        p.playlists = vec![pl(PlaylistKind::Standard, 19, 100)];
        assert!(!badges(&p, &cfg).contains(&Badge::PeakDisparity));
    }

    fn entry(n: &str, relation: Relation, p: Option<PlayerProfile>) -> LobbyEntry {
        LobbyEntry {
            id: PlayerId::new(Platform::Steam, format!("7656119800000{n}")),
            name: n.into(),
            team: Team::Unknown,
            relation,
            state: p.map_or(EntryState::Loading, |p| EntryState::Ready(Box::new(p))),
        }
    }

    #[test]
    fn only_opponents_hides_teammates() {
        let entries = vec![
            entry("1", Relation::Teammate, Some(base("1"))),
            entry("2", Relation::Opponent, Some(base("2"))),
            entry("3", Relation::Unknown, None),
        ];
        let mut cfg = Config::default();
        assert_eq!(build_views(&entries, &cfg).len(), 3);
        cfg.filters.only_opponents = true;
        let v = build_views(&entries, &cfg);
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|x| x.relation != Relation::Teammate));
    }

    #[test]
    fn views_sorted_opponents_first_then_by_smurf_index() {
        let mut smurf = base("2");
        smurf.total_wins = 50;
        smurf.playlists = vec![pl(PlaylistKind::Standard, 19, 40)];
        let entries = vec![
            entry("1", Relation::Teammate, Some(smurf.clone())),
            entry("3", Relation::Opponent, Some(base("3"))),
            entry("2", Relation::Opponent, Some(smurf)),
        ];
        let v = build_views(&entries, &Config::default());
        assert_eq!(v[0].name, "2");
        assert_eq!(v[1].name, "3");
        assert_eq!(v[2].relation, Relation::Teammate);
        assert!(v[0].badges.contains(&Badge::SmurfSuspect));
    }

    #[test]
    fn loading_entries_have_no_badges() {
        let v = build_views(&[entry("1", Relation::Opponent, None)], &Config::default());
        assert_eq!(v[0].status, PlayerStatus::Loading);
        assert!(v[0].badges.is_empty());
    }

    #[test]
    fn lobby_context_median_and_demo_average() {
        let mut a = base("1");
        a.demos_per_match = Some(0.4);
        let mut b = base("2");
        b.demos_per_match = Some(0.6);
        b.playlists = vec![pl(PlaylistKind::Doubles, 16, 10)];
        let mut c = base("3");
        c.demos_per_match = Some(0.8);
        c.playlists = vec![pl(PlaylistKind::Doubles, 18, 10)];
        let ctx = LobbyContext::from_profiles([&a, &b, &c]);
        assert_eq!(ctx.median_team_tier, Some(16.0));
        assert!((ctx.avg_demos.unwrap() - 0.6).abs() < 1e-5);
    }
}
