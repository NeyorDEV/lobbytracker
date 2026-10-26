use tracker_core::{PlayerId, PlayerProfile, Relation, Team};
use tracker_heuristics::{EntryState, LobbyEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    New,
    Changed,
    Unchanged,
}

#[derive(Debug, Default)]
pub struct Lobby {
    players: Vec<LobbyEntry>,
    self_team: Option<Team>,
}

impl Lobby {
    pub fn reset(&mut self) {
        self.players.clear();
        self.self_team = None;
    }

    pub fn set_self_team(&mut self, team: Team) -> bool {
        if team == Team::Unknown || self.self_team == Some(team) {
            return false;
        }
        self.self_team = Some(team);
        true
    }

    pub fn relation_of(&self, team: Team) -> Relation {
        match (self.self_team, team) {
            (Some(me), t) if t != Team::Unknown => {
                if me == t {
                    Relation::Teammate
                } else {
                    Relation::Opponent
                }
            }
            _ => Relation::Unknown,
        }
    }

    pub fn upsert(&mut self, id: &PlayerId, name: Option<String>, team: Option<Team>) -> Upsert {
        let team = team.unwrap_or(Team::Unknown);
        if let Some(p) = self.players.iter_mut().find(|p| &p.id == id) {
            let mut changed = false;
            if team != Team::Unknown && p.team != team {
                p.team = team;
                changed = true;
            }
            if let Some(n) = name.filter(|n| !n.is_empty() && *n != p.name) {
                p.name = n;
                changed = true;
            }
            self.refresh_relations();
            return if changed { Upsert::Changed } else { Upsert::Unchanged };
        }
        let name = name.filter(|n| !n.is_empty()).unwrap_or_else(|| short_id(id));
        self.players.push(LobbyEntry {
            id: id.clone(),
            name,
            team,
            relation: Relation::Unknown,
            state: EntryState::Loading,
        });
        self.refresh_relations();
        Upsert::New
    }

    fn refresh_relations(&mut self) {
        let me = self.self_team;
        for p in &mut self.players {
            p.relation = match (me, p.team) {
                (Some(m), t) if t != Team::Unknown => {
                    if m == t {
                        Relation::Teammate
                    } else {
                        Relation::Opponent
                    }
                }
                _ => Relation::Unknown,
            };
        }
    }

    /// À appeler après `set_self_team` pour que les relations soient à jour.
    pub fn recompute_relations(&mut self) {
        self.refresh_relations();
    }

    pub fn set_profile(&mut self, profile: PlayerProfile) -> bool {
        match self.players.iter_mut().find(|p| p.id == profile.id) {
            Some(p) => {
                p.state = EntryState::Ready(Box::new(profile));
                true
            }
            None => false,
        }
    }

    pub fn set_failed(&mut self, id: &PlayerId) -> bool {
        match self.players.iter_mut().find(|p| &p.id == id) {
            Some(p) => {
                p.state = EntryState::Failed;
                true
            }
            None => false,
        }
    }

    pub fn entries(&self) -> &[LobbyEntry] {
        &self.players
    }

    pub fn loading_count(&self) -> usize {
        self.players.iter().filter(|p| matches!(p.state, EntryState::Loading)).count()
    }

    pub fn relation_for(&self, id: &PlayerId) -> Relation {
        self.players.iter().find(|p| &p.id == id).map_or(Relation::Unknown, |p| p.relation)
    }
}

fn short_id(id: &PlayerId) -> String {
    let tail: String = id.id.chars().take(8).collect();
    format!("{}:{tail}…", id.platform.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::Platform;

    fn id(n: u32) -> PlayerId {
        PlayerId::new(Platform::Steam, format!("7656119800000{n:04}"))
    }

    #[test]
    fn upsert_reports_new_changed_unchanged() {
        let mut l = Lobby::default();
        assert_eq!(l.upsert(&id(1), None, None), Upsert::New);
        assert_eq!(l.upsert(&id(1), None, None), Upsert::Unchanged);
        assert_eq!(l.upsert(&id(1), Some("Zed".into()), Some(Team::Blue)), Upsert::Changed);
        assert_eq!(l.upsert(&id(1), Some("Zed".into()), Some(Team::Blue)), Upsert::Unchanged);
        assert_eq!(l.entries()[0].name, "Zed");
    }

    #[test]
    fn relations_follow_self_team() {
        let mut l = Lobby::default();
        l.upsert(&id(1), None, Some(Team::Blue));
        l.upsert(&id(2), None, Some(Team::Orange));
        l.upsert(&id(3), None, None);
        assert!(l.entries().iter().all(|e| e.relation == Relation::Unknown));
        assert!(l.set_self_team(Team::Blue));
        l.recompute_relations();
        assert_eq!(l.relation_for(&id(1)), Relation::Teammate);
        assert_eq!(l.relation_for(&id(2)), Relation::Opponent);
        assert_eq!(l.relation_for(&id(3)), Relation::Unknown);
    }

    #[test]
    fn reset_clears_players_and_self_team() {
        let mut l = Lobby::default();
        l.set_self_team(Team::Orange);
        l.upsert(&id(1), None, None);
        l.reset();
        assert!(l.entries().is_empty());
        assert_eq!(l.relation_of(Team::Blue), Relation::Unknown);
    }

    #[test]
    fn loading_count_tracks_state() {
        let mut l = Lobby::default();
        l.upsert(&id(1), None, None);
        l.upsert(&id(2), None, None);
        assert_eq!(l.loading_count(), 2);
        l.set_failed(&id(1));
        assert_eq!(l.loading_count(), 1);
        assert!(!l.set_failed(&id(9)));
    }
}
