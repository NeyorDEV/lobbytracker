//! Récupération des profils publics (Tracker Network) : WebView2 invisible pour passer
//! Cloudflare, jeton API officiel en repli, file d'attente prioritaire.

pub mod fetcher;
pub mod mapping;
pub mod queue;
pub mod webview;

pub use fetcher::{Fetcher, FetchOptions, HybridFetcher, JsonSource, SourceFetcher, TrnApiSource};
pub use queue::{FetchOutcome, Priority, TrackerQueue};
pub use webview::{ingest_script, WebviewDriver, WebviewSource};

#[derive(Debug, thiserror::Error)]
pub enum ScrapeError {
    #[error("requête bloquée (403) : Cloudflare ou jeton invalide")]
    Blocked,
    #[error("profil introuvable ou privé")]
    NotFound,
    #[error("limite de requêtes atteinte (429)")]
    RateLimited,
    #[error("erreur réseau : {0}")]
    Network(String),
    #[error("délai dépassé")]
    Timeout,
    #[error("réponse inattendue : {0}")]
    BadPayload(String),
    #[error("aucune source de données configurée")]
    NoSource,
    #[error("fenêtre WebView : {0}")]
    Webview(String),
}
