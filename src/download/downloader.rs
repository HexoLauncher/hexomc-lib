use futures::stream::{self, StreamExt};
use sha1::{Digest, Sha1};
use sha2::Sha256;
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
    pub sha256: Option<String>,
}

impl DownloadTask {
    pub fn new(url: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            path: path.into(),
            sha1: None,
            sha256: None,
        }
    }

    pub fn with_sha1(mut self, sha1: impl Into<String>) -> Self {
        self.sha1 = Some(sha1.into());
        self
    }

    /// Require a matching SHA-256 checksum, in addition to any configured SHA1.
    pub fn with_sha256(mut self, sha256: impl Into<String>) -> Self {
        self.sha256 = Some(sha256.into());
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
    result.eq_ignore_ascii_case(expected)
}

/// Verify a local file's SHA-256 checksum.
pub async fn verify_sha256(path: &Path, expected: &str) -> bool {
    let Ok(data) = fs::read(path).await else {
        return false;
    };
    hex::encode(Sha256::digest(&data)).eq_ignore_ascii_case(expected)
}

/// Check every checksum supplied by the caller.
async fn verify_task(task: &DownloadTask) -> bool {
    if let Some(expected) = &task.sha1 {
        if !verify_sha1(&task.path, expected).await {
            return false;
        }
    }
    if let Some(expected) = &task.sha256 {
        if !verify_sha256(&task.path, expected).await {
            return false;
        }
    }
    true
}

/// Download a single file with optional SHA1 and SHA-256 verification, retrying on failure.
pub async fn download_file(task: &DownloadTask) -> Result<()> {
    download_file_with_progress(task, |_, _| {}).await
}

/// Download and verify a file, reporting downloaded bytes and total bytes.
/// Total is zero when the server omits the content length. Each retry resets the
/// downloaded count to zero. A valid cached file reports its size as both counts.
pub async fn download_file_with_progress<F>(task: &DownloadTask, progress: F) -> Result<()>
where
    F: Fn(usize, usize) + Send + Sync,
{
    if task.path.exists() && verify_task(task).await {
        let size = fs::metadata(&task.path).await?.len() as usize;
        progress(size, size);
        return Ok(());
    }

    if let Some(parent) = task.path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let client = reqwest::Client::new();
    let mut last_err = None;

    for _ in 0..MAX_RETRIES {
        match try_download(&client, &task.url, &task.path, &progress).await {
            Ok(()) => {
                if !verify_task(task).await {
                    last_err = Some(HexoError::ChecksumMismatch {
                        path: task.path.display().to_string(),
                    });
                    continue;
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

async fn try_download<F>(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    progress: &F,
) -> Result<()>
where
    F: Fn(usize, usize) + Send + Sync,
{
    let response = client.get(url).send().await?.error_for_status()?;
    let total = response.content_length().unwrap_or(0) as usize;
    let mut downloaded = 0usize;
    progress(0, total);
    let mut file = fs::File::create(path).await?;
    let mut stream = response.bytes_stream();

    use futures::StreamExt as _;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded = downloaded.saturating_add(chunk.len());
        progress(downloaded, total);
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

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[tokio::test]
    async fn byte_progress_reports_retries_and_cached_completion() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archive");
        let (url, server) = serve(vec![(200, "bad"), (200, "abc")]).await;
        let task = DownloadTask::new(url, &path).with_sha256(ABC_SHA256);
        let events = std::sync::Mutex::new(Vec::new());
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            download_file_with_progress(&task, |done, total| {
                events.lock().unwrap().push((done, total))
            }),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        {
            let events = events.lock().unwrap();
            assert_eq!(events.iter().filter(|event| **event == (0, 3)).count(), 2);
            assert_eq!(events.last(), Some(&(3, 3)));
            assert!(events.iter().all(|(done, total)| *total == 3 && *done <= 3));
        }
        events.lock().unwrap().clear();
        download_file_with_progress(&task, |done, total| {
            events.lock().unwrap().push((done, total))
        })
        .await
        .unwrap();
        assert_eq!(*events.lock().unwrap(), vec![(3, 3)]);
    }

    async fn serve(responses: Vec<(u16, &'static str)>) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                }
                let response = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (url, handle)
    }

    #[tokio::test]
    async fn sha256_checks_missing_corrupt_and_uppercase_hashes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archive");
        assert!(!verify_sha256(&path, ABC_SHA256).await);
        fs::write(&path, b"abc").await.unwrap();
        assert!(verify_sha256(&path, ABC_SHA256).await);
        assert!(verify_sha256(&path, &ABC_SHA256.to_uppercase()).await);
        fs::write(&path, b"corrupt").await.unwrap();
        assert!(!verify_sha256(&path, ABC_SHA256).await);
    }

    #[tokio::test]
    async fn sha256_download_retries_and_replaces_invalid_cache() {
        for cached in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("archive");
            if cached {
                fs::write(&path, b"stale").await.unwrap();
            }
            let (url, server) = serve(vec![(200, "corrupt"), (200, "abc")]).await;
            let task = DownloadTask::new(url, &path).with_sha256(ABC_SHA256);
            tokio::time::timeout(std::time::Duration::from_secs(5), download_file(&task))
                .await
                .unwrap()
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fs::read(&path).await.unwrap(), b"abc");
        }
    }

    #[tokio::test]
    async fn sha256_download_rejects_corruption_after_retries() {
        let temp = tempfile::tempdir().unwrap();
        let (url, server) = serve(vec![(200, "corrupt"); MAX_RETRIES as usize]).await;
        let task = DownloadTask::new(url, temp.path().join("archive")).with_sha256(ABC_SHA256);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), download_file(&task))
            .await
            .unwrap();
        assert!(matches!(result, Err(HexoError::ChecksumMismatch { .. })));
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn all_configured_checksums_must_match() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archive");
        fs::write(&path, b"abc").await.unwrap();
        let task = DownloadTask::new("http://127.0.0.1:0/archive", &path)
            .with_sha1(sha1_of(b"abc"))
            .with_sha256(ABC_SHA256);
        download_file(&task).await.unwrap();
        assert!(!verify_task(&task.clone().with_sha1(sha1_of(b"wrong"))).await);
        assert!(!verify_task(&task.with_sha256("0".repeat(64))).await);
    }

    #[tokio::test]
    async fn http_errors_do_not_overwrite_cached_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archive");
        fs::write(&path, b"existing").await.unwrap();
        let (url, server) = serve(vec![(404, "not found"); MAX_RETRIES as usize]).await;
        let task = DownloadTask::new(url, &path).with_sha256(ABC_SHA256);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), download_file(&task))
            .await
            .unwrap();
        assert!(result.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(&path).await.unwrap(), b"existing");
    }

    fn sha1_of(data: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    #[tokio::test]
    async fn verify_sha1_correct() {
        let data = b"hello hexomc-lib";
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        let expected = sha1_of(data);
        assert!(verify_sha1(f.path(), &expected).await);
    }

    #[tokio::test]
    async fn verify_sha1_wrong_hash() {
        let data = b"hello hexomc-lib";
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
        let task = DownloadTask::new("http://127.0.0.1:0/nonexistent", f.path()).with_sha1(sha1);

        assert!(download_file(&task).await.is_ok());
    }
}
