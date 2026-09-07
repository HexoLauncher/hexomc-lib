use futures::stream::{self, StreamExt};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::error::{HexoError, Result};

const MAX_RETRIES: u32 = 3;
const ASSETS_URL: &str = "https://resources.download.minecraft.net";

#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub url: String,
    pub path: PathBuf,
    pub sha1: Option<String>,
}

impl DownloadTask {
    pub fn new(url: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            path: path.into(),
            sha1: None,
        }
    }

    pub fn with_sha1(mut self, sha1: impl Into<String>) -> Self {
        self.sha1 = Some(sha1.into());
        self
    }

    /// Build a download task for a Minecraft asset.
    pub fn asset(hash: &str, assets_dir: &Path) -> Self {
        let prefix = &hash[..2];
        let url = format!("{}/{}/{}", ASSETS_URL, prefix, hash);
        let path = assets_dir.join("objects").join(prefix).join(hash);
        Self::new(url, path).with_sha1(hash.to_string())
    }
}

/// Verify a local file's SHA1.
pub async fn verify_sha1(path: &Path, expected: &str) -> bool {
    let Ok(data) = fs::read(path).await else {
        return false;
    };
    let mut hasher = Sha1::new();
    hasher.update(&data);
    let result = hex::encode(hasher.finalize());
    result == expected
}

/// Download a single file (with SHA1 verification), retrying on failure.
pub async fn download_file(task: &DownloadTask) -> Result<()> {
    // Skip if it already exists with the correct sha1.
    if task.path.exists() {
        if let Some(expected) = &task.sha1 {
            if verify_sha1(&task.path, expected).await {
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }

    if let Some(parent) = task.path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let client = reqwest::Client::new();
    let mut last_err = None;

    for attempt in 0..MAX_RETRIES {
        match try_download(&client, &task.url, &task.path).await {
            Ok(()) => {
                if let Some(expected) = &task.sha1 {
                    if !verify_sha1(&task.path, expected).await {
                        if attempt + 1 < MAX_RETRIES {
                            last_err = Some(HexoError::ChecksumMismatch {
                                path: task.path.display().to_string(),
                            });
                            continue;
                        } else {
                            return Err(HexoError::ChecksumMismatch {
                                path: task.path.display().to_string(),
                            });
                        }
                    }
                }
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or(HexoError::DownloadFailed {
        url: task.url.clone(),
    }))
}

async fn try_download(client: &reqwest::Client, url: &str, path: &Path) -> Result<()> {
    let response = client.get(url).send().await?;
    let mut file = fs::File::create(path).await?;
    let mut stream = response.bytes_stream();

    use futures::StreamExt as _;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(())
}

/// Download a batch of files concurrently.
///
/// `concurrency`: max simultaneous downloads
/// `progress`: callback (done, total)
pub async fn download_batch<F>(
    tasks: Vec<DownloadTask>,
    concurrency: usize,
    progress: F,
) -> Result<()>
where
    F: Fn(usize, usize) + Send + Sync + 'static,
{
    let total = tasks.len();
    let progress = std::sync::Arc::new(progress);
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let results: Vec<Result<()>> = stream::iter(tasks)
        .map(|task| {
            let progress = progress.clone();
            let counter = counter.clone();
            async move {
                let result = download_file(&task).await;
                let done = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                progress(done, total);
                result
            }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    for r in results {
        r?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha1::{Digest, Sha1};
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn sha1_of(data: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    #[tokio::test]
    async fn verify_sha1_correct() {
        let data = b"hello hexo-mc-lib";
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        let expected = sha1_of(data);
        assert!(verify_sha1(f.path(), &expected).await);
    }

    #[tokio::test]
    async fn verify_sha1_wrong_hash() {
        let data = b"hello hexo-mc-lib";
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        assert!(!verify_sha1(f.path(), "0000000000000000000000000000000000000000").await);
    }

    #[tokio::test]
    async fn verify_sha1_missing_file() {
        assert!(!verify_sha1(std::path::Path::new("/nonexistent/file.bin"), "abc").await);
    }

    #[tokio::test]
    async fn download_skips_existing_valid_file() {
        let data = b"cached content";
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        let sha1 = sha1_of(data);

        // Unreachable URL, but the matching sha1 means the download is skipped.
        let task = DownloadTask::new("http://127.0.0.1:0/nonexistent", f.path())
            .with_sha1(sha1);

        assert!(download_file(&task).await.is_ok());
    }
}
