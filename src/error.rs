use thiserror::Error;

#[derive(Error, Debug)]
pub enum HexoError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("SHA1 checksum mismatch: {path}")]
    ChecksumMismatch { path: String },

    #[error("version not found: {0}")]
    VersionNotFound(String),

    #[error("Java not found (requires version {required})")]
    JavaNotFound { required: u32 },

    #[error("authentication failed: {0}")]
    AuthError(String),

    #[error("unsupported mod loader: {0}")]
    UnsupportedLoader(String),

    #[error("instance not found: {0}")]
    InstanceNotFound(String),

    #[error("download failed after retries: {url}")]
    DownloadFailed { url: String },

    #[error("Forge processor failed: {0}")]
    ProcessorFailed(String),

    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),

    #[error("invalid project or version ID: {0:?}")]
    InvalidId(String),

    #[error("CurseForge API key is not set")]
    MissingApiKey,

    #[error("the author of {project} disallows third-party downloads")]
    DownloadBlocked {
        project: String,
        /// Page the file can be downloaded from manually.
        website: Option<String>,
    },

    #[error("dependency resolution failed: {0}")]
    Dependency(String),

    #[error("error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, HexoError>;
