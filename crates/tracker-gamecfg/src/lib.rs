//! Détection des installations de Rocket League (Epic Games, Steam) et réglage de la Stats API
//! officielle (`PacketSendRate` dans `DefaultStatsAPI.ini`).
//!
//! Seul ce fichier de configuration, prévu par le jeu pour cette fonction, est modifié : aucune
//! injection, aucun accès au processus. Une copie `.lobbytracker.bak` de l'original est conservée.

use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

const SECTION: &str = "[TAGame.MatchStatsExporter_TA]";
/// Fréquence maximale acceptée par le jeu.
pub const MAX_RATE: u32 = 120;

#[derive(Debug, thiserror::Error)]
pub enum GameCfgError {
    #[error("ce dossier ne ressemble pas à une installation de Rocket League : {0}")]
    NotAnInstall(String),
    #[error("accès refusé à {0} : relancez LobbyTracker en administrateur")]
    PermissionDenied(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Launcher {
    Epic,
    Steam,
    Custom,
}

impl Launcher {
    pub fn label(self) -> &'static str {
        match self {
            Launcher::Epic => "Epic Games",
            Launcher::Steam => "Steam",
            Launcher::Custom => "Dossier choisi",
        }
    }
}

/// Une installation de Rocket League et l'état de sa Stats API.
#[derive(Debug, Clone, Serialize)]
pub struct Install {
    pub launcher: Launcher,
    pub root: PathBuf,
    pub ini_path: PathBuf,
    /// `PacketSendRate` actuel ; `None` si le fichier est absent ou sans cette clé.
    pub rate: Option<u32>,
}

pub fn ini_path(root: &Path) -> PathBuf {
    root.join("TAGame").join("Config").join("DefaultStatsAPI.ini")
}

pub fn is_install(root: &Path) -> bool {
    root.join("TAGame").is_dir()
}

fn install_at(launcher: Launcher, root: PathBuf) -> Install {
    let ini = ini_path(&root);
    let rate = std::fs::read_to_string(&ini).ok().and_then(|c| parse_rate(&c));
    Install { launcher, root, ini_path: ini, rate }
}

/// Toutes les installations trouvées, plus le dossier choisi par l'utilisateur s'il est valide.
pub fn detect(custom: Option<&Path>) -> Vec<Install> {
    let mut out: Vec<Install> = Vec::new();
    let mut push = |launcher: Launcher, root: PathBuf| {
        if is_install(&root) && !out.iter().any(|i| same_dir(&i.root, &root)) {
            out.push(install_at(launcher, root));
        }
    };
    for r in epic_roots() {
        push(Launcher::Epic, r);
    }
    for r in steam_roots() {
        push(Launcher::Steam, r);
    }
    if let Some(c) = custom {
        push(Launcher::Custom, c.to_path_buf());
    }
    out
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
    norm(a) == norm(b)
}

// --- Epic Games -------------------------------------------------------------------------------

fn epic_roots() -> Vec<PathBuf> {
    let manifest = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("Epic")
        .join("UnrealEngineLauncher")
        .join("LauncherInstalled.dat");
    let mut roots = std::fs::read_to_string(manifest).map(|s| epic_roots_from(&s)).unwrap_or_default();
    roots.push(PathBuf::from(r"C:\Program Files\Epic Games\rocketleague"));
    roots
}

/// Extrait le dossier de Rocket League (code interne « Sugar ») de `LauncherInstalled.dat`.
pub fn epic_roots_from(json: &str) -> Vec<PathBuf> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    v.get("InstallationList")
        .and_then(|l| l.as_array())
        .into_iter()
        .flatten()
        .filter(|e| e.get("AppName").and_then(|n| n.as_str()) == Some("Sugar"))
        .filter_map(|e| e.get("InstallLocation").and_then(|n| n.as_str()))
        .map(PathBuf::from)
        .collect()
}

// --- Steam ------------------------------------------------------------------------------------

fn steam_roots() -> Vec<PathBuf> {
    let mut steam_dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = steam_path_from_registry() {
        steam_dirs.push(p);
    }
    steam_dirs.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));

    let mut libraries: Vec<PathBuf> = Vec::new();
    for dir in &steam_dirs {
        libraries.push(dir.clone());
        let vdf = dir.join("steamapps").join("libraryfolders.vdf");
        if let Ok(s) = std::fs::read_to_string(vdf) {
            libraries.extend(steam_libraries_from(&s));
        }
    }
    libraries.into_iter().map(|l| l.join("steamapps").join("common").join("rocketleague")).collect()
}

fn steam_path_from_registry() -> Option<PathBuf> {
    let mut cmd = Command::new("reg");
    cmd.args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"]).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW : pas de console qui clignote
    }
    let out = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("SteamPath"))?;
    let value = line.split("REG_SZ").nth(1)?.trim();
    (!value.is_empty()).then(|| PathBuf::from(value.replace('/', "\\")))
}

static VDF_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""path"\s+"(?P<p>[^"]+)""#).unwrap());

/// Dossiers de bibliothèque listés par `libraryfolders.vdf`.
pub fn steam_libraries_from(vdf: &str) -> Vec<PathBuf> {
    VDF_PATH.captures_iter(vdf).map(|c| PathBuf::from(c["p"].replace("\\\\", "\\"))).collect()
}

// --- DefaultStatsAPI.ini ----------------------------------------------------------------------

/// Valeur de `PacketSendRate`, hors commentaires.
pub fn parse_rate(content: &str) -> Option<u32> {
    content.lines().find_map(|l| {
        let l = l.trim();
        let (k, v) = l.split_once('=')?;
        k.trim().eq_ignore_ascii_case("PacketSendRate").then(|| v.trim().parse().ok())?
    })
}

/// Renvoie le contenu avec `PacketSendRate=rate`, en conservant le reste (commentaires, ports,
/// fins de ligne). Ajoute la clé, et au besoin la section, si elles manquent.
pub fn with_rate(content: &str, rate: u32) -> String {
    let rate = rate.min(MAX_RATE);
    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let new_line = format!("PacketSendRate={rate}");
    let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();

    let is_rate = |l: &str| l.split_once('=').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case("PacketSendRate"));
    if let Some(i) = lines.iter().position(|l| is_rate(l.trim())) {
        lines[i] = new_line;
    } else if let Some(i) = lines.iter().position(|l| l.trim().eq_ignore_ascii_case(SECTION)) {
        lines.insert(i + 1, new_line);
    } else {
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(SECTION.to_owned());
        lines.push("Port=49123".to_owned());
        lines.push(new_line);
    }
    let mut out = lines.join(eol);
    out.push_str(eol);
    out
}

/// Écrit `PacketSendRate` dans l'installation. Renvoie `true` si le fichier a changé.
pub fn apply_rate(root: &Path, rate: u32) -> Result<bool, GameCfgError> {
    if !is_install(root) {
        return Err(GameCfgError::NotAnInstall(root.display().to_string()));
    }
    let ini = ini_path(root);
    let current = std::fs::read_to_string(&ini).unwrap_or_default();
    let updated = with_rate(&current, rate);
    if updated == current {
        return Ok(false);
    }
    let result = (|| {
        if let Some(dir) = ini.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let backup = ini.with_extension("ini.lobbytracker.bak");
        if ini.exists() && !backup.exists() {
            std::fs::copy(&ini, &backup)?;
        }
        std::fs::write(&ini, updated)
    })();
    match result {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(GameCfgError::PermissionDenied(ini.display().to_string()))
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "[TAGame.MatchStatsExporter_TA]\r\n\r\n; Port the client will listen\r\nPort=49123\r\n\r\n; How many times\r\nPacketSendRate=0\r\n";

    #[test]
    fn reads_rate_ignoring_comments() {
        assert_eq!(parse_rate(REAL), Some(0));
        assert_eq!(parse_rate("; PacketSendRate=30\nPacketSendRate = 10"), Some(10));
        assert_eq!(parse_rate("Port=1"), None);
    }

    #[test]
    fn replaces_only_the_rate_and_keeps_line_endings() {
        let out = with_rate(REAL, 30);
        assert_eq!(parse_rate(&out), Some(30));
        assert!(out.contains("Port=49123\r\n") && out.contains("; How many times\r\n"));
        assert!(!out.contains('\n') || out.matches('\n').count() == out.matches("\r\n").count());
        assert_eq!(with_rate(&out, 30), out, "idempotent");
    }

    #[test]
    fn adds_missing_key_or_section() {
        let no_key = with_rate("[TAGame.MatchStatsExporter_TA]\nPort=49123\n", 10);
        assert_eq!(parse_rate(&no_key), Some(10));
        assert!(no_key.starts_with("[TAGame.MatchStatsExporter_TA]\nPacketSendRate=10"));
        let empty = with_rate("", 10);
        assert!(empty.contains(SECTION) && parse_rate(&empty) == Some(10));
        assert_eq!(parse_rate(&with_rate("", 999)), Some(MAX_RATE));
    }

    #[test]
    fn finds_epic_rocket_league_only() {
        let json = r#"{"InstallationList":[
            {"InstallLocation":"D:\\Games\\Other","AppName":"Fortnite"},
            {"InstallLocation":"E:\\Epic\\rocketleague","AppName":"Sugar"}]}"#;
        assert_eq!(epic_roots_from(json), vec![PathBuf::from(r"E:\Epic\rocketleague")]);
        assert!(epic_roots_from("garbage").is_empty());
    }

    #[test]
    fn parses_steam_library_folders() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}";
        assert_eq!(
            steam_libraries_from(vdf),
            vec![PathBuf::from(r"C:\Program Files (x86)\Steam"), PathBuf::from(r"D:\SteamLibrary")]
        );
    }

    #[test]
    fn applies_to_an_install_and_keeps_a_backup() {
        let dir = std::env::temp_dir().join(format!("lt-gamecfg-{}", std::process::id()));
        let cfg = dir.join("TAGame").join("Config");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("DefaultStatsAPI.ini"), REAL).unwrap();
        assert!(apply_rate(&dir, 10).unwrap());
        assert!(!apply_rate(&dir, 10).unwrap(), "déjà à jour");
        let found = detect(Some(&dir));
        let custom = found.iter().find(|i| i.launcher == Launcher::Custom).unwrap();
        assert_eq!(custom.rate, Some(10));
        let backup = std::fs::read_to_string(cfg.join("DefaultStatsAPI.ini.lobbytracker.bak")).unwrap();
        assert_eq!(parse_rate(&backup), Some(0));
        assert!(matches!(apply_rate(&dir.join("nope"), 10), Err(GameCfgError::NotAnInstall(_))));
        let _ = std::fs::remove_dir_all(dir);
    }
}
