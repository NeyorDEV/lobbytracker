//! File d'attente prioritaire : adversaires d'abord, puis inconnus, puis coéquipiers.

use crate::fetcher::{FetchOptions, Fetcher};
use crate::ScrapeError;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tracker_cache::Cache;
use tracker_core::{unix_now, PlayerId, PlayerProfile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Teammate = 0,
    Unknown = 1,
    Opponent = 2,
}

#[derive(Debug, PartialEq, Eq)]
struct Job {
    priority: Priority,
    seq: u64,
    id: PlayerId,
}

impl Ord for Job {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap = max-heap : priorité haute d'abord, puis le plus ancien (seq le plus petit).
        self.priority.cmp(&other.priority).then(other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Job {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
pub enum FetchOutcome {
    Fetched(Box<PlayerProfile>),
    Failed { id: PlayerId, error: String },
}

#[derive(Default)]
struct State {
    heap: BinaryHeap<Job>,
    queued: HashSet<PlayerId>,
    in_flight: Option<PlayerId>,
    /// Pseudo à utiliser pour la requête de chaque joueur en file.
    lookups: std::collections::HashMap<PlayerId, String>,
    seq: u64,
    /// Incrémenté par `clear` pour ignorer les résultats d'un lobby précédent.
    generation: u64,
}

#[derive(Default)]
pub struct TrackerQueue {
    state: Mutex<State>,
    notify: Notify,
}

impl TrackerQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Ajoute un joueur ; renvoie `false` s'il est déjà en file ou en cours.
    pub fn enqueue(&self, id: PlayerId, priority: Priority) -> bool {
        self.enqueue_named(id, priority, None)
    }

    /// Comme `enqueue`, avec le pseudo requis par Tracker Network pour Epic, Xbox et PSN.
    pub fn enqueue_named(&self, id: PlayerId, priority: Priority, lookup: Option<String>) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.queued.contains(&id) || s.in_flight.as_ref() == Some(&id) {
            return false;
        }
        if let Some(name) = lookup.filter(|n| !n.is_empty()) {
            s.lookups.insert(id.clone(), name);
        }
        s.seq += 1;
        let seq = s.seq;
        s.queued.insert(id.clone());
        s.heap.push(Job { priority, seq, id });
        drop(s);
        self.notify.notify_one();
        true
    }

    pub fn pop_next(&self) -> Option<PlayerId> {
        let mut s = self.state.lock().unwrap();
        let job = s.heap.pop()?;
        s.queued.remove(&job.id);
        s.in_flight = Some(job.id.clone());
        Some(job.id)
    }

    /// Nombre de joueurs en attente ou en cours de résolution.
    pub fn pending(&self) -> usize {
        let s = self.state.lock().unwrap();
        s.heap.len() + usize::from(s.in_flight.is_some())
    }

    /// Vide la file (nouveau lobby).
    pub fn clear(&self) {
        let mut s = self.state.lock().unwrap();
        s.heap.clear();
        s.queued.clear();
        s.lookups.clear();
        s.generation += 1;
    }

    fn take_lookup(&self, id: &PlayerId) -> Option<String> {
        self.state.lock().unwrap().lookups.remove(id)
    }

    fn generation(&self) -> u64 {
        self.state.lock().unwrap().generation
    }

    fn finish(&self) {
        self.state.lock().unwrap().in_flight = None;
    }

    /// Lance le worker : un joueur à la fois, `min_interval` entre deux requêtes réseau.
    /// Les profils déjà en cache valide sont renvoyés immédiatement, sans pause.
    pub fn spawn_worker(
        self: &Arc<Self>,
        fetcher: Arc<dyn Fetcher>,
        cache: Arc<Cache>,
        ttl_secs: i64,
        options: Arc<dyn Fn() -> FetchOptions + Send + Sync>,
        min_interval: Duration,
        out: mpsc::UnboundedSender<FetchOutcome>,
    ) -> JoinHandle<()> {
        let q = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let Some(id) = q.pop_next() else {
                    q.notify.notified().await;
                    continue;
                };
                let gen = q.generation();
                let lookup = q.take_lookup(&id);

                if let Ok(Some(p)) = cache.get_fresh(&id, ttl_secs) {
                    q.finish();
                    let _ = out.send(FetchOutcome::Fetched(Box::new(p)));
                    continue;
                }

                let result = fetcher.fetch(&id, FetchOptions { lookup, ..options() }).await;
                q.finish();
                if q.generation() != gen {
                    continue; // lobby remplacé pendant la requête
                }
                let outcome = match result {
                    Ok(mut p) => {
                        p.fetched_at = unix_now();
                        if let Err(e) = cache.put(&p) {
                            tracing::warn!("écriture du cache impossible : {e}");
                        }
                        FetchOutcome::Fetched(Box::new(p))
                    }
                    Err(e) => FetchOutcome::Failed { id, error: describe(&e) },
                };
                if out.send(outcome).is_err() {
                    return;
                }
                tokio::time::sleep(min_interval).await;
            }
        })
    }
}

fn describe(e: &ScrapeError) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tracker_core::Platform;

    fn pid(n: u32) -> PlayerId {
        PlayerId::new(Platform::Steam, format!("7656119800000{n:04}"))
    }

    #[test]
    fn opponents_first_then_fifo_within_priority() {
        let q = TrackerQueue::new();
        q.enqueue(pid(1), Priority::Teammate);
        q.enqueue(pid(2), Priority::Opponent);
        q.enqueue(pid(3), Priority::Unknown);
        q.enqueue(pid(4), Priority::Opponent);
        assert_eq!(q.pop_next(), Some(pid(2)));
        assert_eq!(q.pop_next(), Some(pid(4)));
        assert_eq!(q.pop_next(), Some(pid(3)));
        assert_eq!(q.pop_next(), Some(pid(1)));
        assert_eq!(q.pop_next(), None);
    }

    #[test]
    fn duplicates_are_ignored_and_pending_counts_in_flight() {
        let q = TrackerQueue::new();
        assert!(q.enqueue(pid(1), Priority::Opponent));
        assert!(!q.enqueue(pid(1), Priority::Opponent));
        q.pop_next();
        assert!(!q.enqueue(pid(1), Priority::Opponent)); // en cours
        assert_eq!(q.pending(), 1);
        q.finish();
        assert_eq!(q.pending(), 0);
    }

    struct Fixed;
    #[async_trait]
    impl Fetcher for Fixed {
        async fn fetch(&self, id: &PlayerId, _: FetchOptions) -> Result<PlayerProfile, ScrapeError> {
            if id == &pid(9) {
                return Err(ScrapeError::NotFound);
            }
            Ok(PlayerProfile {
                id: id.clone(),
                display_name: "net".into(),
                total_wins: 1,
                peak_tier: None,
                playlists: vec![],
                saves_per_match: None,
                goals_per_match: None,
                shots_per_match: None,
                loss_streak: 0,
                recent: None,
                fetched_at: 0,
            })
        }
    }

    #[tokio::test]
    async fn worker_uses_cache_then_network_and_reports_failures() {
        let cache = Arc::new(Cache::open_in_memory().unwrap());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let q = TrackerQueue::new();
        let h = q.spawn_worker(
            Arc::new(Fixed),
            cache.clone(),
            3600,
            Arc::new(FetchOptions::default),
            Duration::from_millis(1),
            tx,
        );

        q.enqueue(pid(1), Priority::Opponent);
        match rx.recv().await.unwrap() {
            FetchOutcome::Fetched(p) => assert_eq!(p.display_name, "net"),
            o => panic!("{o:?}"),
        }
        assert_eq!(cache.len().unwrap(), 1, "le résultat réseau est mis en cache");

        let mut cached = cache.get_fresh(&pid(1), 3600).unwrap().unwrap();
        cached.display_name = "cache".into();
        cache.put(&cached).unwrap();
        q.enqueue(pid(1), Priority::Opponent);
        match rx.recv().await.unwrap() {
            FetchOutcome::Fetched(p) => assert_eq!(p.display_name, "cache"),
            o => panic!("{o:?}"),
        }

        q.enqueue(pid(9), Priority::Opponent);
        assert!(matches!(rx.recv().await.unwrap(), FetchOutcome::Failed { .. }));
        h.abort();
    }
}
