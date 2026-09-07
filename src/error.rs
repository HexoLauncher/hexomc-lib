use thiserror::Error;

#[derive(Error, Debug)]
pub enum HexoError {
    #[error("網路錯誤: {0}")]
    Network(#[from] reqwest::Error),

    #[error("IO 錯誤: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 解析錯誤: {0}")]
    Json(#[from] serde_json::Error),

    #[error("ZIP 錯誤: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("SHA1 校驗失敗: {path}")]
    ChecksumMismatch { path: String },

    #[error("版本不存在: {0}")]
    VersionNotFound(String),

    #[error("Java 未找到（需要版本 {required}）")]
    JavaNotFound { required: u32 },

    #[error("驗證失敗: {0}")]
    AuthError(String),

    #[error("模組 loader 類型不支援: {0}")]
    UnsupportedLoader(String),

    #[error("Instance 不存在: {0}")]
    InstanceNotFound(String),

    #[error("下載失敗（已重試）: {url}")]
    DownloadFailed { url: String },

    #[error("Forge processor 執行失敗: {0}")]
    ProcessorFailed(String),

    #[error("其他錯誤: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, HexoError>;
