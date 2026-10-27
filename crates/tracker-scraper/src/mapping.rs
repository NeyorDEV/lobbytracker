//! Conversion des JSON Tracker Network (API interne et API publique, même schéma) en `PlayerProfile`.
//!
//! Les champs sont lus de façon tolérante : un champ absent donne `None` plutôt qu'une erreur,
//! car le schéma n'est pas garanti stable.

use crate::ScrapeError;
use serde_json::Value;
use tracker_core::{unix_now, PlayerId, PlayerProfile, PlaylistKind, PlaylistStat, RecentForm};

fn num(v: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for k in path {
        cur = cur.get(k)?;
    }
    cur.as_f64()
}

fn text<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for k in path {
        cur = cur.get(k)?;
    }
    cur.as_str()
}

/// Rang approximatif (tier 1-22) déduit d'un MMR de pointe, pour les segments qui ne
/// fournissent que `peakRating`. Seuils 2v2/3v3 approximatifs (à base de la saison courante).
pub fn mmr_to_tier(mmr: f64) -> u32 {
    const FLOORS: [(f64, u32); 15] = [
        (1860.0, 22),
        (1715.0, 21),
        (1575.0, 20),
        (1435.0, 19),
        (1315.0, 18),
        (1195.0, 17),
        (1075.0, 16),
        (995.0, 15),
        (915.0, 14),
        (835.0, 13),
        (755.0, 12),
        (675.0, 11),
        (595.0, 10),
        (515.0, 9),
        (435.0, 8),
    ];
    FLOORS.iter().find(|(m, _)| mmr >= *m).map(|(_, t)| *t).unwrap_or(5)
}

/// Parse la réponse `.../standard/profile/{plateforme}/{id}`.
pub fn parse_profile(json: &Value, id: &PlayerId) -> Result<PlayerProfile, ScrapeError> {
    let data = json.get("data").ok_or_else(|| {
        let msg = json
            .pointer("/errors/0/message")
            .and_then(Value::as_str)
            .unwrap_or("champ `data` absent");
        ScrapeError::BadPayload(msg.to_string())
    })?;
    let segments = data
        .get("segments")
        .and_then(Value::as_array)
        .ok_or_else(|| ScrapeError::BadPayload("champ `segments` absent".into()))?;

    let display_name = text(data, &["platformInfo", "platformUserHandle"]).unwrap_or("").to_string();

    let mut playlists = Vec::new();
    let mut peak_tier: Option<u32> = None;
    let mut wins = 0u32;
    let (mut goals, mut saves, mut shots, mut matches_total) = (None, None, None, None);
    // Moyennes par match fournies par Tracker Network (une entrée `playlistAverage` par mode).
    let (mut avg_matches, mut avg_goals, mut avg_saves, mut avg_shots) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);

    for seg in segments {
        let kind = text(seg, &["type"]).unwrap_or("");
        let stats = seg.get("stats").cloned().unwrap_or(Value::Null);
        match kind {
            "overview" => {
                wins = num(&stats, &["wins", "value"]).unwrap_or(0.0) as u32;
                goals = num(&stats, &["goals", "value"]);
                saves = num(&stats, &["saves", "value"]);
                shots = num(&stats, &["shots", "value"]);
                matches_total = num(&stats, &["matchesPlayed", "value"]);
            }
            "playlist" => {
                let name = text(seg, &["metadata", "name"]).unwrap_or("");
                let tier = num(&stats, &["tier", "value"]).unwrap_or(0.0) as u32;
                if let Some(t) = num(&stats, &["peakTier", "value"]).map(|v| v as u32).filter(|t| *t > 0) {
                    peak_tier = Some(peak_tier.map_or(t, |p| p.max(t)));
                }
                playlists.push(PlaylistStat {
                    kind: PlaylistKind::from_name(name),
                    tier,
                    mmr: num(&stats, &["rating", "value"]).map(|v| v as u32),
                    matches: num(&stats, &["matchesPlayed", "value"]).unwrap_or(0.0) as u32,
                });
            }
            "playlistAverage" => {
                let m = num(&stats, &["matches", "value"]).or_else(|| num(&stats, &["matches"])).unwrap_or(0.0);
                let get = |k: &str| num(&stats, &[k, "value"]).or_else(|| num(&stats, &[k])).unwrap_or(0.0);
                avg_matches += m;
                avg_goals += get("avgGoalsPerGame") * m;
                avg_saves += get("avgSavesPerGame") * m;
                avg_shots += get("avgShotsPerGame") * m;
            }
            "peak-rating" => {
                // Tier de pointe direct si fourni, sinon estimation depuis le MMR de pointe.
                let t = num(&stats, &["peakTier", "value"])
                    .or_else(|| num(&stats, &["tier", "value"]))
                    .map(|v| v as u32)
                    .or_else(|| num(&stats, &["peakRating", "value"]).map(mmr_to_tier));
                if let Some(t) = t {
                    peak_tier = Some(peak_tier.map_or(t, |p| p.max(t)));
                }
            }
            _ => {}
        }
    }

    // Les statistiques « par match » exigent un nombre de matchs ; à défaut on l'estime
    // à partir des victoires (≈ 50 % de winrate).
    let matches = matches_total
        .filter(|m| *m > 0.0)
        .or_else(|| (wins > 0).then(|| wins as f64 * 2.0));
    let per = |total: Option<f64>| match (total, matches) {
        (Some(t), Some(m)) => Some((t / m) as f32),
        _ => None,
    };
    // Les moyennes officielles par mode sont plus fiables que le total estimé.
    let from_avg = |sum: f64| (avg_matches > 0.0).then(|| (sum / avg_matches) as f32);

    Ok(PlayerProfile {
        id: id.clone(),
        display_name,
        total_wins: wins,
        peak_tier,
        playlists,
        saves_per_match: from_avg(avg_saves).or_else(|| per(saves)),
        goals_per_match: from_avg(avg_goals).or_else(|| per(goals)),
        shots_per_match: from_avg(avg_shots).or_else(|| per(shots)),
        loss_streak: 0,
        recent: None,
        fetched_at: unix_now(),
    })
}

/// Parse `.../profile/{plateforme}/{id}/sessions` et renvoie la série de défaites en cours
/// (matchs les plus récents d'abord). Renvoie 0 si le format n'est pas reconnu.
pub fn parse_loss_streak(json: &Value) -> u32 {
    let Some(items) = json.pointer("/data/items").and_then(Value::as_array) else { return 0 };
    let mut streak = 0;
    for session in items {
        let Some(matches) = session.get("matches").and_then(Value::as_array) else { continue };
        for m in matches {
            match match_result(m) {
                Some(false) => streak += 1,
                Some(true) => return streak,
                None => {}
            }
        }
    }
    streak
}

/// Forme récente en classé (duel, 2v2, 3v3) : les `RECENT_MATCHES` derniers matchs, du plus récent
/// au plus ancien. Les lignes regroupées (« 3 wins ») comptent pour `matchesPlayed` matchs.
pub fn parse_recent_form(json: &Value) -> Option<RecentForm> {
    const RECENT_MATCHES: u32 = 30;
    let items = json.pointer("/data/items")?.as_array()?;
    let mut form = RecentForm { matches: 0, wins: 0, mmr_gain: 0 };
    'outer: for session in items {
        let Some(matches) = session.get("matches").and_then(Value::as_array) else { continue };
        for m in matches {
            let playlist = text(m, &["metadata", "playlist"]).unwrap_or("");
            if !PlaylistKind::from_name(playlist).is_core_ranked() {
                continue;
            }
            let n = num(m, &["stats", "matchesPlayed", "value"]).unwrap_or(1.0).max(1.0) as u32;
            let wins = match match_result(m) {
                Some(true) => num(m, &["stats", "wins", "value"]).map_or(n, |w| w as u32).min(n),
                Some(false) => 0,
                None => continue,
            };
            form.matches += n;
            form.wins += wins;
            form.mmr_gain += num(m, &["stats", "rating", "metadata", "ratingDelta"]).unwrap_or(0.0) as i32;
            if form.matches >= RECENT_MATCHES {
                break 'outer;
            }
        }
    }
    (form.matches > 0).then_some(form)
}

/// `Some(true)` = victoire, `Some(false)` = défaite.
fn match_result(m: &Value) -> Option<bool> {
    if let Some(r) = text(m, &["metadata", "result"]) {
        let r = r.to_ascii_lowercase();
        return match r.as_str() {
            "win" | "victory" => Some(true),
            "loss" | "defeat" | "lose" => Some(false),
            // Matchs regroupés par Tracker Network : « 3 wins », « 2 losses ».
            g if g.ends_with("wins") => Some(true),
            g if g.ends_with("losses") => Some(false),
            _ => None,
        };
    }
    num(m, &["stats", "result", "value"]).map(|v| v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tracker_core::Platform;

    fn pid() -> PlayerId {
        PlayerId::new(Platform::Steam, "76561198012345678")
    }

    #[test]
    fn parses_overview_playlists_and_peak() {
        let j = json!({"data": {
            "platformInfo": {"platformUserHandle": "Smurfy"},
            "segments": [
                {"type":"overview","stats":{
                    "wins":{"value":100.0},"goals":{"value":150.0},"saves":{"value":260.0},
                    "shots":{"value":500.0}}},
                {"type":"playlist","metadata":{"name":"Ranked Duel 1v1"},
                 "stats":{"tier":{"value":18.0},"rating":{"value":1350.0},"matchesPlayed":{"value":40.0}}},
                {"type":"playlist","metadata":{"name":"Ranked Doubles 2v2"},
                 "stats":{"tier":{"value":17.0},"matchesPlayed":{"value":12.0}}},
                {"type":"peak-rating","stats":{"peakRating":{"value":1600.0}}}
            ]}});
        let p = parse_profile(&j, &pid()).unwrap();
        assert_eq!(p.display_name, "Smurfy");
        assert_eq!(p.total_wins, 100);
        assert_eq!(p.playlists.len(), 2);
        assert_eq!(p.tier_of(PlaylistKind::Duel), Some(18));
        assert_eq!(p.peak_tier, Some(20));
        // 200 matchs estimés (2 × victoires)
        assert!((p.saves_per_match.unwrap() - 1.3).abs() < 1e-5);
    }

    #[test]
    fn explicit_matches_played_wins_over_estimate() {
        let j = json!({"data":{"segments":[
            {"type":"overview","stats":{"wins":{"value":10.0},"saves":{"value":100.0},"matchesPlayed":{"value":50.0}}}
        ]}});
        let p = parse_profile(&j, &pid()).unwrap();
        assert!((p.saves_per_match.unwrap() - 2.0).abs() < 1e-5);
    }

    #[test]
    fn missing_data_is_an_error_with_api_message() {
        let j = json!({"errors":[{"code":"CollectorResultStatus::NotFound","message":"profil introuvable"}]});
        match parse_profile(&j, &pid()) {
            Err(ScrapeError::BadPayload(m)) => assert!(m.contains("introuvable")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn loss_streak_counts_until_first_win() {
        let j = json!({"data":{"items":[{"matches":[
            {"metadata":{"result":"loss"}},{"metadata":{"result":"loss"}},
            {"metadata":{"result":"loss"}},{"metadata":{"result":"loss"}},
            {"metadata":{"result":"win"}},{"metadata":{"result":"loss"}}]}]}});
        assert_eq!(parse_loss_streak(&j), 4);
        assert_eq!(parse_loss_streak(&json!({})), 0);
    }

    #[test]
    fn recent_form_sums_ranked_matches_and_mmr() {
        let m = |res: &str, pl: &str, delta: i64| json!({"metadata":{"result":res,"playlist":pl},
            "stats":{"rating":{"metadata":{"ratingDelta":delta}}}});
        let j = json!({"data":{"items":[{"matches":[
            m("victory", "Ranked Doubles 2v2", 10),
            m("defeat", "Ranked Doubles 2v2", -9),
            m("victory", "Casual", 0),
            {"metadata":{"result":"3 wins","playlist":"Ranked Duel 1v1"},
             "stats":{"matchesPlayed":{"value":4.0},"wins":{"value":3.0},"rating":{"metadata":{"ratingDelta":20}}}}
        ]}]}});
        let f = parse_recent_form(&j).unwrap();
        assert_eq!((f.matches, f.wins, f.mmr_gain), (6, 4, 21));
        assert!(parse_recent_form(&json!({})).is_none());
    }

    #[test]
    fn grouped_wins_end_a_loss_streak() {
        let j = json!({"data":{"items":[{"matches":[
            {"metadata":{"result":"defeat"}},{"metadata":{"result":"defeat"}},
            {"metadata":{"result":"3 wins"}},{"metadata":{"result":"defeat"}}]}]}});
        assert_eq!(parse_loss_streak(&j), 2);
    }

    #[test]
    fn real_playlist_peak_tier_and_averages_are_used() {
        let j = json!({"data":{"segments":[
            {"type":"overview","stats":{"wins":{"value":100.0},"saves":{"value":999.0}}},
            {"type":"playlist","metadata":{"name":"Ranked Doubles 2v2"},
             "stats":{"tier":{"value":17.0},"peakTier":{"value":21.0},"matchesPlayed":{"value":50.0}}},
            {"type":"playlistAverage","stats":{"matches":{"value":10.0},"avgSavesPerGame":{"value":2.0},
             "avgGoalsPerGame":{"value":1.0},"avgShotsPerGame":{"value":3.0}}},
            {"type":"playlistAverage","stats":{"matches":{"value":30.0},"avgSavesPerGame":{"value":1.0},
             "avgGoalsPerGame":{"value":2.0},"avgShotsPerGame":{"value":3.0}}}
        ]}});
        let p = parse_profile(&j, &pid()).unwrap();
        assert_eq!(p.peak_tier, Some(21));
        assert!((p.saves_per_match.unwrap() - 1.25).abs() < 1e-5);
        assert!((p.goals_per_match.unwrap() - 1.75).abs() < 1e-5);
    }

    #[test]
    fn mmr_tier_estimation_is_monotonic() {
        assert_eq!(mmr_to_tier(1600.0), 20);
        assert!(mmr_to_tier(1300.0) < mmr_to_tier(1400.0));
        assert_eq!(mmr_to_tier(100.0), 5);
    }
}
