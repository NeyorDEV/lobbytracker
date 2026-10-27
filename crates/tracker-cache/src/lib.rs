//! Cache SQLite (mode WAL) des profils joueurs, avec TTL.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;
use tracker_core::{unix_now, PlayerId, PlayerProfile, PlaylistKind, PlaylistStat, RecentForm};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("erreur SQLite : {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("verrou du cache empoisonné")]
    Poisoned,
    #[error("identifiant en cache illisible : {0}")]
    BadKey(String),
    #[error("création du dossier du cache impossible : {0}")]
    Io(#[from] std::io::Error),
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS players (
    key TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    total_wins INTEGER NOT NULL,
    peak_tier INTEGER,
    saves_pm REAL, goals_pm REAL, shots_pm REAL,
    loss_streak INTEGER NOT NULL DEFAULT 0,
    fetched_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS playlists (
    key TEXT NOT NULL REFERENCES players(key) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    tier INTEGER NOT NULL,
    mmr INTEGER,
    matches INTEGER NOT NULL,
    PRIMARY KEY (key, kind)
);
CREATE INDEX IF NOT EXISTS idx_players_fetched ON players(fetched_at);
";

pub struct Cache {
    conn: Mutex<Connection>,
}

impl Cache {
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, CacheError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, CacheError> {
        // `journal_mode` renvoie une ligne : on la lit via query_row.
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        // Colonnes ajoutées après la première version : la base existante est migrée sur place.
        for col in ["recent_matches INTEGER", "recent_wins INTEGER", "recent_gain INTEGER"] {
            let _ = conn.execute(&format!("ALTER TABLE players ADD COLUMN {col}"), []);
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CacheError> {
        self.conn.lock().map_err(|_| CacheError::Poisoned)
    }

    /// Profil encore valide (plus récent que `ttl_secs`), sinon `None`.
    pub fn get_fresh(&self, id: &PlayerId, ttl_secs: i64) -> Result<Option<PlayerProfile>, CacheError> {
        self.get_fresh_at(id, ttl_secs, unix_now())
    }

    pub fn get_fresh_at(
        &self,
        id: &PlayerId,
        ttl_secs: i64,
        now: i64,
    ) -> Result<Option<PlayerProfile>, CacheError> {
        let conn = self.lock()?;
        let key = id.key();
        let row = conn
            .query_row(
                "SELECT display_name,total_wins,peak_tier,saves_pm,goals_pm,shots_pm,loss_streak,fetched_at,recent_matches,recent_wins,recent_gain
                 FROM players WHERE key=?1 AND fetched_at >= ?2",
                params![key, now - ttl_secs],
                |r| {
                    Ok(PlayerProfile {
                        id: id.clone(),
                        display_name: r.get(0)?,
                        total_wins: r.get::<_, i64>(1)? as u32,
                        peak_tier: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                        playlists: Vec::new(),
                        saves_per_match: r.get::<_, Option<f64>>(3)?.map(|v| v as f32),
                        goals_per_match: r.get::<_, Option<f64>>(4)?.map(|v| v as f32),
                        shots_per_match: r.get::<_, Option<f64>>(5)?.map(|v| v as f32),
                        loss_streak: r.get::<_, i64>(6)? as u32,
                        recent: match r.get::<_, Option<i64>>(8)? {
                            Some(m) => Some(RecentForm {
                                matches: m as u32,
                                wins: r.get::<_, Option<i64>>(9)?.unwrap_or(0) as u32,
                                mmr_gain: r.get::<_, Option<i64>>(10)?.unwrap_or(0) as i32,
                            }),
                            None => None,
                        },
                        fetched_at: r.get(7)?,
                    })
                },
            )
            .optional()?;
        let Some(mut profile) = row else { return Ok(None) };

        let mut stmt = conn.prepare("SELECT kind,tier,mmr,matches FROM playlists WHERE key=?1")?;
        let rows = stmt.query_map(params![key], |r| {
            Ok(PlaylistStat {
                kind: PlaylistKind::from_db(&r.get::<_, String>(0)?),
                tier: r.get::<_, i64>(1)? as u32,
                mmr: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                matches: r.get::<_, i64>(3)? as u32,
            })
        })?;
        for p in rows {
            profile.playlists.push(p?);
        }
        Ok(Some(profile))
    }

    /// Insère ou remplace un profil et ses playlists (transaction unique).
    pub fn put(&self, p: &PlayerProfile) -> Result<(), CacheError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let key = p.id.key();
        tx.execute(
            "INSERT OR REPLACE INTO players
             (key,display_name,total_wins,peak_tier,saves_pm,goals_pm,shots_pm,loss_streak,fetched_at,
              recent_matches,recent_wins,recent_gain)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                key,
                p.display_name,
                p.total_wins,
                p.peak_tier,
                p.saves_per_match,
                p.goals_per_match,
                p.shots_per_match,
                p.loss_streak,
                p.fetched_at,
                p.recent.map(|r| r.matches),
                p.recent.map(|r| r.wins),
                p.recent.map(|r| r.mmr_gain)
            ],
        )?;
        tx.execute("DELETE FROM playlists WHERE key=?1", params![key])?;
        for pl in &p.playlists {
            tx.execute(
                "INSERT OR REPLACE INTO playlists (key,kind,tier,mmr,matches) VALUES (?1,?2,?3,?4,?5)",
                params![key, pl.kind.as_str(), pl.tier, pl.mmr, pl.matches],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Supprime les profils plus vieux que `ttl_secs` ; renvoie le nombre de lignes retirées.
    pub fn purge_expired(&self, ttl_secs: i64) -> Result<usize, CacheError> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM players WHERE fetched_at < ?1", params![unix_now() - ttl_secs])?)
    }

    pub fn len(&self) -> Result<usize, CacheError> {
        let conn = self.lock()?;
        Ok(conn.query_row("SELECT COUNT(*) FROM players", [], |r| r.get::<_, i64>(0))? as usize)
    }

    pub fn is_empty(&self) -> Result<bool, CacheError> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::Platform;

    fn profile(fetched_at: i64) -> PlayerProfile {
        PlayerProfile {
            id: PlayerId::new(Platform::Steam, "76561198012345678"),
            display_name: "Tester".into(),
            total_wins: 120,
            peak_tier: Some(19),
            playlists: vec![
                PlaylistStat { kind: PlaylistKind::Duel, tier: 18, mmr: Some(1500), matches: 40 },
                PlaylistStat { kind: PlaylistKind::Doubles, tier: 17, mmr: None, matches: 10 },
            ],
            saves_per_match: None,
            goals_per_match: Some(0.8),
            shots_per_match: Some(2.5),
            loss_streak: 3,
            recent: Some(RecentForm { matches: 20, wins: 15, mmr_gain: 180 }),
            fetched_at,
        }
    }

    #[test]
    fn roundtrip_preserves_profile() {
        let c = Cache::open_in_memory().unwrap();
        let mut p = profile(1_000);
        c.put(&p).unwrap();
        let mut got = c.get_fresh_at(&p.id, 86_400, 1_100).unwrap().unwrap();
        got.playlists.sort_by_key(|x| x.tier);
        p.playlists.sort_by_key(|x| x.tier);
        assert_eq!(got, p);
    }

    #[test]
    fn ttl_expiry_hides_entry() {
        let c = Cache::open_in_memory().unwrap();
        let p = profile(1_000);
        c.put(&p).unwrap();
        assert!(c.get_fresh_at(&p.id, 100, 1_099).unwrap().is_some());
        assert!(c.get_fresh_at(&p.id, 100, 1_101).unwrap().is_none());
    }

    #[test]
    fn put_replaces_playlists() {
        let c = Cache::open_in_memory().unwrap();
        let mut p = profile(1_000);
        c.put(&p).unwrap();
        p.playlists.truncate(1);
        c.put(&p).unwrap();
        assert_eq!(c.get_fresh_at(&p.id, 10_000, 1_000).unwrap().unwrap().playlists.len(), 1);
        assert_eq!(c.len().unwrap(), 1);
    }

    #[test]
    fn purge_removes_old_rows() {
        let c = Cache::open_in_memory().unwrap();
        c.put(&profile(1)).unwrap();
        assert_eq!(c.purge_expired(60).unwrap(), 1);
        assert!(c.is_empty().unwrap());
    }

    #[test]
    fn file_database_uses_wal() {
        let dir = std::env::temp_dir().join(format!("lt-cache-{}", std::process::id()));
        let path = dir.join("cache.db");
        let c = Cache::open(&path).unwrap();
        let mode: String =
            c.lock().unwrap().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }
}
