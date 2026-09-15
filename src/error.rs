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

    #[error("error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, HexoError>;
