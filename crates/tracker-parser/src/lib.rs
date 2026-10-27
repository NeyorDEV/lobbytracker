//! Extraction passive des identifiants de joueurs depuis `Launch.log` et la Stats API.
//!
//! Le format exact du log n'est pas documenté publiquement : le parseur se base sur la forme
//! canonique des identifiants Rocket League (`Plateforme|id|splitscreen`) et, en repli, sur des
//! identifiants « nus » (Epic 32 hex, SteamID64) uniquement sur des lignes de connexion.

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use tracker_core::{Platform, PlayerId, Team};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("message Stats API invalide : {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeenContext {
    /// Ligne de connexion (`BeaconJoin`, `LogNet … Join`, …).
    Connection,
    /// Identifiant typé rencontré sur une autre ligne.
    Other,
    /// Stats API.
    StatsApi,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedEvent {
    PlayerSeen { id: PlayerId, name: Option<String>, team: Option<Team>, context: SeenContext },
    /// Nouveau match / retour au menu : la liste des joueurs doit être vidée.
    LobbyReset,
    /// Joueur local (compte connecté sur cette machine), lu dans la ligne de connexion du jeu.
    LocalPlayer { id: PlayerId, name: Option<String> },
    /// Fin du décompte 3-2-1 : le coup d'envoi vient d'être donné (événement `RoundStarted`).
    Kickoff,
}

static TYPED_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?P<plat>EpicGames|Epic|Steam|XboxOne|Xbox|XBL|PlayStation|PS4|PS5|PSN)\|(?P<id>[0-9A-Za-z_\-]{4,64})\|\d+",
    )
    .unwrap()
});
static BARE_EPIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?P<id>[0-9a-f]{32})\b").unwrap());
static BARE_STEAM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?P<id>7656119\d{10})\b").unwrap());
static LOCAL_PLAYER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"HandleLocalPlayerLoginStatusChanged\s+PlayerName=(?P<n>.+?)\s+PlayerID=(?P<id>\S+)\s+LoginStatus=LS_LoggedIn\s+IsPrimary=True",
    )
    .unwrap()
});
static CONNECTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)BeaconJoin|LogNet.*(join|login|connect)|PlayerJoin").unwrap());
static NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:player ?name|name)\s*[=:]\s*"?(?P<n>[^"|,\r\n?&]+?)"?\s*(?:[,|;)\]?&]|\s\w+=|$)"#).unwrap()
});
static RESET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bLoadMap\b|MatchEnded|Match_Ended|NewMatch|LeaveMatch|Lobby ?Reset").unwrap()
});

fn platform_from_tag(tag: &str) -> Option<Platform> {
    match tag.to_ascii_lowercase().as_str() {
        "epic" | "epicgames" => Some(Platform::Epic),
        "steam" => Some(Platform::Steam),
        "xboxone" | "xbox" | "xbl" => Some(Platform::Xbox),
        "ps4" | "ps5" | "psn" | "playstation" => Some(Platform::Psn),
        _ => None,
    }
}

/// Valide le format de l'identifiant selon la plateforme.
fn valid_id(platform: Platform, id: &str) -> bool {
    match platform {
        Platform::Epic => id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        Platform::Steam => id.len() == 17 && id.starts_with("7656") && id.bytes().all(|b| b.is_ascii_digit()),
        Platform::Xbox => (8..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit()),
        Platform::Psn => id.len() >= 4,
    }
}

/// Extrait tous les identifiants typés (`Epic|abc…|0`) d'un texte.
pub fn typed_ids(text: &str) -> Vec<PlayerId> {
    let mut out = Vec::new();
    for c in TYPED_ID.captures_iter(text) {
        if let Some(p) = platform_from_tag(&c["plat"]) {
            if valid_id(p, &c["id"]) {
                let id = PlayerId::new(p, &c["id"]);
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct LogParser;

impl LogParser {
    pub fn new() -> Self {
        Self
    }

    /// Détecte uniquement la connexion du joueur local (utilisé aussi sur l'historique du log).
    pub fn parse_local_player(&self, line: &str) -> Option<ParsedEvent> {
        let c = LOCAL_PLAYER.captures(line)?;
        let id = typed_ids(&c["id"]).into_iter().next()?;
        let name = Some(c["n"].trim().to_string()).filter(|n| !n.is_empty());
        Some(ParsedEvent::LocalPlayer { id, name })
    }

    /// Analyse une ligne de `Launch.log`.
    pub fn parse_line(&self, line: &str) -> Vec<ParsedEvent> {
        if let Some(local) = self.parse_local_player(line) {
            return vec![local];
        }
        let mut events = Vec::new();
        if RESET.is_match(line) {
            events.push(ParsedEvent::LobbyReset);
        }
        let is_conn = CONNECTION.is_match(line);
        let name = NAME.captures(line).map(|c| c["n"].trim().to_string()).filter(|n| !n.is_empty());

        let mut ids = typed_ids(line);
        if is_conn {
            for c in BARE_STEAM.captures_iter(line) {
                ids.push(PlayerId::new(Platform::Steam, &c["id"]));
            }
            for c in BARE_EPIC.captures_iter(line) {
                ids.push(PlayerId::new(Platform::Epic, &c["id"]));
            }
        }
        let mut seen: Vec<PlayerId> = Vec::new();
        for id in ids {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id.clone());
            events.push(ParsedEvent::PlayerSeen {
                id,
                name: name.clone(),
                team: None,
                context: if is_conn { SeenContext::Connection } else { SeenContext::Other },
            });
        }
        events
    }
}

/// Analyse un message JSON de la Stats API (`UpdateState`, `MatchCreated`, …).
pub fn parse_stats_message(raw: &str) -> Result<Vec<ParsedEvent>, ParseError> {
    let v: Value = serde_json::from_str(raw)?;
    let event = v.get("Event").and_then(Value::as_str).unwrap_or("");
    let mut out = Vec::new();

    if matches!(event, "MatchCreated" | "MatchInitialized" | "MatchDestroyed") {
        out.push(ParsedEvent::LobbyReset);
    }
    if event == "RoundStarted" {
        out.push(ParsedEvent::Kickoff);
    }
    // `Data` est un objet (récent) ou une chaîne JSON (ancien).
    let data = match v.get("Data") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    };
    if let Some(players) = data.get("Players").and_then(Value::as_array) {
        for p in players {
            let Some(pid) = p.get("PrimaryId").and_then(Value::as_str) else { continue };
            let Some(id) = typed_ids(pid).into_iter().next() else { continue };
            out.push(ParsedEvent::PlayerSeen {
                id,
                name: p.get("Name").and_then(Value::as_str).map(str::to_owned),
                team: p.get("TeamNum").and_then(Value::as_i64).map(Team::from_num),
                context: SeenContext::StatsApi,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPIC: &str = "0123456789abcdef0123456789ABCDEF";

    fn seen(events: &[ParsedEvent]) -> Vec<(&PlayerId, Option<&str>)> {
        events
            .iter()
            .filter_map(|e| match e {
                ParsedEvent::PlayerSeen { id, name, .. } => Some((id, name.as_deref())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn typed_epic_id_with_name() {
        let line = format!("[0123.45] LogNet: Join succeeded: PlayerName=Zen, UniqueId=Epic|{EPIC}|0");
        let ev = LogParser::new().parse_line(&line);
        let s = seen(&ev);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0.platform, Platform::Epic);
        assert_eq!(s[0].0.id, EPIC.to_ascii_lowercase());
        assert_eq!(s[0].1, Some("Zen"));
    }

    #[test]
    fn steam_xbox_psn_typed() {
        let p = LogParser::new();
        let st = p.parse_line("BeaconJoin Steam|76561198012345678|0");
        assert_eq!(seen(&st)[0].0, &PlayerId::new(Platform::Steam, "76561198012345678"));
        let xb = p.parse_line("Player XboxOne|2535412345678901|0 connected");
        assert_eq!(seen(&xb)[0].0.platform, Platform::Xbox);
        let ps = p.parse_line("Player PS4|abc12345def|0 connected");
        assert_eq!(seen(&ps)[0].0.platform, Platform::Psn);
    }

    #[test]
    fn invalid_ids_are_rejected() {
        let p = LogParser::new();
        assert!(seen(&p.parse_line("Epic|tooshort|0")).is_empty());
        assert!(seen(&p.parse_line("Steam|12345678901234567|0")).is_empty());
        assert!(seen(&p.parse_line("Xbox|notdigits12345|0")).is_empty());
    }

    #[test]
    fn bare_ids_only_on_connection_lines() {
        let p = LogParser::new();
        let conn = p.parse_line(&format!("LogNet: BeaconJoin {EPIC}"));
        assert_eq!(seen(&conn).len(), 1);
        // Un hash de 32 hex dans une ligne anodine (shader, GUID…) ne doit rien produire.
        let noise = p.parse_line(&format!("LogShaderCompilers: cache key {EPIC}"));
        assert!(seen(&noise).is_empty());
    }

    #[test]
    fn bare_steam_id_on_connection_line() {
        let ev = LogParser::new().parse_line("LogNet: PlayerJoin 76561198012345678");
        assert_eq!(seen(&ev)[0].0.platform, Platform::Steam);
    }

    #[test]
    fn duplicate_ids_on_same_line_are_merged() {
        let line = format!("BeaconJoin Epic|{EPIC}|0 again Epic|{EPIC}|0");
        assert_eq!(seen(&LogParser::new().parse_line(&line)).len(), 1);
    }

    #[test]
    fn local_player_login_line_from_real_log() {
        let line = "[0023.09] Party: HandleLocalPlayerLoginStatusChanged PlayerName=neyor. PlayerID=Epic|6897812333f244858dddff1fbb06554f|0 LoginStatus=LS_LoggedIn IsPrimary=True IsInParty=False";
        let ev = LogParser::new().parse_line(line);
        assert_eq!(
            ev,
            vec![ParsedEvent::LocalPlayer {
                id: PlayerId::new(Platform::Epic, "6897812333f244858dddff1fbb06554f"),
                name: Some("neyor.".into())
            }]
        );
    }

    #[test]
    fn logged_out_or_secondary_player_is_not_local() {
        let p = LogParser::new();
        let out = "Party: HandleLocalPlayerLoginStatusChanged PlayerName=x PlayerID=Epic|6897812333f244858dddff1fbb06554f|0 LoginStatus=LS_NotLoggedIn IsPrimary=True";
        assert!(p.parse_local_player(out).is_none());
        let sec = "Party: HandleLocalPlayerLoginStatusChanged PlayerName=x PlayerID=Epic|6897812333f244858dddff1fbb06554f|0 LoginStatus=LS_LoggedIn IsPrimary=False";
        assert!(p.parse_local_player(sec).is_none());
    }

    #[test]
    fn real_party_and_uncached_lines_yield_ids() {
        let p = LogParser::new();
        let a = p.parse_line("[1192.04] ScriptLog: Uncached PlatformId for Epic|07375e4935f4407fb2c92312adc1e14f|0");
        assert_eq!(seen(&a).len(), 1);
        let b = p.parse_line("[1191.85] Party: HandleLobbyMemberStatusUpdate LobbyUID=PsyNet-FdPLXBbD Members=2 MemberIndex=1 MemberID=Epic|07375e4935f4407fb2c92312adc1e14f|0");
        assert_eq!(seen(&b).len(), 1);
    }

    #[test]
    fn name_stops_before_next_key_value() {
        let ev = LogParser::new().parse_line(&format!("Join PlayerName=Zen Pro PlayerUID=Epic|{EPIC}|0"));
        assert_eq!(seen(&ev)[0].1, Some("Zen Pro"));
    }

    #[test]
    fn reset_lines_emit_lobby_reset() {
        let ev = LogParser::new().parse_line("[1.0] LogLoad: LoadMap: /Game/Maps/Stadium_P");
        assert!(ev.contains(&ParsedEvent::LobbyReset));
    }

    #[test]
    fn unrelated_lines_produce_nothing() {
        assert!(LogParser::new().parse_line("LogAudio: Display: Sound mixer initialised").is_empty());
    }

    #[test]
    fn stats_api_update_state_with_object_data() {
        let msg = format!(
            r#"{{"Event":"UpdateState","Data":{{"Players":[
                {{"Name":"Alpha","PrimaryId":"Epic|{EPIC}|0","TeamNum":0}},
                {{"Name":"Beta","PrimaryId":"Steam|76561198012345678|0","TeamNum":1}}]}}}}"#
        );
        let ev = parse_stats_message(&msg).unwrap();
        assert_eq!(ev.len(), 2);
        match &ev[1] {
            ParsedEvent::PlayerSeen { team, name, .. } => {
                assert_eq!(*team, Some(Team::Orange));
                assert_eq!(name.as_deref(), Some("Beta"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn stats_api_data_as_string_and_reset() {
        let inner = format!(r#"{{"Players":[{{"Name":"A","PrimaryId":"Epic|{EPIC}|0","TeamNum":0}}]}}"#);
        let msg = serde_json::json!({"Event":"UpdateState","Data":inner}).to_string();
        assert_eq!(parse_stats_message(&msg).unwrap().len(), 1);
        let reset = parse_stats_message(r#"{"Event":"MatchCreated","Data":{}}"#).unwrap();
        assert_eq!(reset, vec![ParsedEvent::LobbyReset]);
    }

    #[test]
    fn round_started_is_a_kickoff() {
        let ev = parse_stats_message(r#"{"Event":"RoundStarted","Data":{"MatchGuid":"abc"}}"#).unwrap();
        assert_eq!(ev, vec![ParsedEvent::Kickoff]);
        // Le début du décompte n'est pas encore le coup d'envoi.
        assert!(parse_stats_message(r#"{"Event":"CountdownBegin","Data":{}}"#).unwrap().is_empty());
    }

    #[test]
    fn stats_api_rejects_garbage() {
        assert!(parse_stats_message("not json").is_err());
    }
}
