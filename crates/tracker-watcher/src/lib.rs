//! Surveillance 100 % passive : tail asynchrone de `Launch.log` (lecture seule) et écoute
//! optionnelle de la Stats API locale (socket TCP 127.0.0.1).

use std::io::SeekFrom;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    LogLine(String),
    /// `Launch.log` a été recréé ou tronqué : Rocket League vient d'être (re)lancé.
    LogReset,
    StatsMessage(String),
}

const POLL_INTERVAL: Duration = Duration::from_millis(150);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Découpe un flux d'octets en lignes complètes, en conservant la ligne partielle.
#[derive(Debug, Default)]
pub struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.pending.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            lines.push(line.trim_end_matches(['\r', '\n']).to_string());
        }
        lines
    }

    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

/// Extrait des objets JSON de premier niveau d'un flux TCP sans séparateur.
#[derive(Debug, Default)]
pub struct JsonFramer {
    buf: Vec<u8>,
    depth: usize,
    in_string: bool,
    escaped: bool,
    start: Option<usize>,
    scanned: usize,
}

impl JsonFramer {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while self.scanned < self.buf.len() {
            let b = self.buf[self.scanned];
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if b == b'\\' {
                    self.escaped = true;
                } else if b == b'"' {
                    self.in_string = false;
                }
            } else {
                match b {
                    b'"' if self.depth > 0 => self.in_string = true,
                    b'{' => {
                        if self.depth == 0 {
                            self.start = Some(self.scanned);
                        }
                        self.depth += 1;
                    }
                    b'}' if self.depth > 0 => {
                        self.depth -= 1;
                        if self.depth == 0 {
                            if let Some(s) = self.start.take() {
                                out.push(String::from_utf8_lossy(&self.buf[s..=self.scanned]).into_owned());
                            }
                        }
                    }
                    _ => {}
                }
            }
            self.scanned += 1;
        }
        // On libère ce qui a été consommé (hors objet en cours).
        let keep_from = self.start.unwrap_or(self.buf.len());
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.scanned -= keep_from;
            if let Some(s) = self.start.as_mut() {
                *s = 0;
            }
        }
        out
    }
}

/// Suit `Launch.log` : démarre en fin de fichier, repart de zéro si le jeu le recrée/tronque.
pub fn spawn_log_tailer(path: PathBuf, tx: mpsc::Sender<WatchEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut first_open = true;
        loop {
            match tail_once(&path, &tx, first_open).await {
                Ok(()) => return, // récepteur fermé
                Err(e) => tracing::debug!("tail de {} interrompu : {e}", path.display()),
            }
            first_open = false;
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    })
}

async fn tail_once(
    path: &PathBuf,
    tx: &mpsc::Sender<WatchEvent>,
    start_at_end: bool,
) -> std::io::Result<()> {
    // Rust ouvre les fichiers avec FILE_SHARE_READ|WRITE|DELETE : le jeu n'est jamais gêné.
    let mut file = tokio::fs::File::open(path).await?;
    let meta = file.metadata().await?;
    let mut created = meta.created().ok();
    let mut pos = if start_at_end { meta.len() } else { 0 };
    if !start_at_end && tx.send(WatchEvent::LogReset).await.is_err() {
        return Ok(()); // le fichier réapparaît : nouveau lancement du jeu
    }
    let mut splitter = LineSplitter::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let meta = tokio::fs::metadata(path).await?;
        let len = meta.len();
        let now_created = meta.created().ok();
        if len < pos || now_created != created {
            // Fichier recréé ou tronqué par un nouveau lancement du jeu.
            pos = 0;
            created = now_created;
            splitter.reset();
            file = tokio::fs::File::open(path).await?;
            if tx.send(WatchEvent::LogReset).await.is_err() {
                return Ok(());
            }
        }
        if len > pos {
            file.seek(SeekFrom::Start(pos)).await?;
            while pos < len {
                let want = ((len - pos) as usize).min(buf.len());
                let n = file.read(&mut buf[..want]).await?;
                if n == 0 {
                    break;
                }
                pos += n as u64;
                for line in splitter.push(&buf[..n]) {
                    if tx.send(WatchEvent::LogLine(line)).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Se connecte à la Stats API (`TAStatsAPI`) et reconnecte automatiquement.
pub fn spawn_stats_listener(port: u16, tx: mpsc::Sender<WatchEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
                tracing::info!("Stats API connectée sur 127.0.0.1:{port}");
                let mut framer = JsonFramer::default();
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            for msg in framer.push(&buf[..n]) {
                                if tx.send(WatchEvent::StatsMessage(msg)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn splitter_keeps_partial_line() {
        let mut s = LineSplitter::default();
        assert_eq!(s.push(b"abc\r\nde"), vec!["abc"]);
        assert_eq!(s.push(b"f\nghi\n"), vec!["def", "ghi"]);
        assert!(s.push(b"tail").is_empty());
    }

    #[test]
    fn framer_handles_split_and_concatenated_objects() {
        let mut f = JsonFramer::default();
        assert!(f.push(br#"{"a":{"b":"x}y"},"#).is_empty());
        let out = f.push(br#""c":1}{"d":"q\"}"}"#);
        assert_eq!(out, vec![r#"{"a":{"b":"x}y"},"c":1}"#, r#"{"d":"q\"}"}"#]);
    }

    #[test]
    fn framer_ignores_noise_between_objects() {
        let mut f = JsonFramer::default();
        assert_eq!(f.push(b"\n  {\"k\":1}\n"), vec!["{\"k\":1}"]);
    }

    #[tokio::test]
    async fn tailer_reads_only_new_lines_and_survives_truncation() {
        let path = std::env::temp_dir().join(format!("lt-tail-{}.log", std::process::id()));
        std::fs::write(&path, "old line\n").unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let handle = spawn_log_tailer(path.clone(), tx);
        tokio::time::sleep(Duration::from_millis(400)).await;

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "new line").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
        assert_eq!(got, WatchEvent::LogLine("new line".into()));

        std::fs::write(&path, "x\n").unwrap(); // troncature = nouveau lancement
        let got = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
        assert_eq!(got, WatchEvent::LogReset);
        let got = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
        assert_eq!(got, WatchEvent::LogLine("x".into()));

        handle.abort();
        let _ = std::fs::remove_file(path);
    }
}
