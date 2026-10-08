use std::path::{Path, PathBuf};
use tokio::fs;

use crate::{
    download::{download_file_with_progress, DownloadTask},
    error::{HexoError, Result},
    install::loader::{no_progress, ProgressFn},
};

const ADOPTIUM_API: &str = "https://api.adoptium.net/v3/assets/latest";

/// Download an Adoptium Temurin JRE to `{base_dir}/java/{version}/`.
/// Returns the full path to the java executable.
pub async fn download_java(version: u32, base_dir: &Path) -> Result<PathBuf> {
    download_java_with_progress(version, base_dir, no_progress()).await
}

/// Reuse an extracted JRE or download one, reporting progress through the callback.
/// During `Downloading Java`, counts are downloaded and total bytes, with zero total
/// meaning unknown size. Counts reset on retries. Other stages report `(0, 0)`.
/// The existing runtime is checked before any network request or archive extraction.
pub async fn download_java_with_progress(
    version: u32,
    base_dir: &Path,
    progress: ProgressFn,
) -> Result<PathBuf> {
    let java_dir = base_dir.join("java").join(version.to_string());
    let extract_dir = java_dir.join("jre");
    if let Some(java_bin) = find_java_bin_in(&extract_dir) {
        progress(0, 0, "Using installed Java");
        return Ok(java_bin);
    }
    fs::create_dir_all(&java_dir).await?;

    progress(0, 0, "Fetching Java release");
    let (os, arch, archive_type) = platform_info();
    let url = format!(
        "{}/{}/hotspot?image_type=jre&os={}&architecture={}&vendor=eclipse",
        ADOPTIUM_API, version, os, arch
    );

    let client = reqwest::Client::new();
    let releases: Vec<AdoptiumRelease> = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let release = releases
        .into_iter()
        .next()
        .ok_or_else(|| HexoError::JavaNotFound { required: version })?;

    let binary = &release.binary;
    let pkg = &binary.package;

    let archive_path = java_dir.join(&pkg.name);
    download_file_with_progress(&pkg.download_task(&archive_path), |downloaded, total| {
        let total = if total == 0 { pkg.size as usize } else { total };
        progress(downloaded, total, "Downloading Java");
    })
    .await?;

    progress(0, 0, "Extracting Java");
    fs::create_dir_all(&extract_dir).await?;

    if archive_type == "zip" {
        extract_zip(&archive_path, &extract_dir).await?;
    } else {
        extract_tar_gz(&archive_path, &extract_dir).await?;
    }

    let java_bin =
        find_java_bin_in(&extract_dir).ok_or(HexoError::JavaNotFound { required: version })?;
    progress(0, 0, "Java ready");
    Ok(java_bin)
}

fn platform_info() -> (&'static str, &'static str, &'static str) {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x32"
    };

    let archive = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    };

    (os, arch, archive)
}

#[derive(serde::Deserialize)]
struct AdoptiumRelease {
    binary: AdoptiumBinary,
}

#[derive(serde::Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
}

#[derive(serde::Deserialize)]
struct AdoptiumPackage {
    name: String,
    link: String,
    checksum: String,
    #[serde(default)]
    size: u64,
}

impl AdoptiumPackage {
    /// Adoptium's package checksum is SHA-256, not SHA1.
    fn download_task(&self, path: &Path) -> DownloadTask {
        DownloadTask::new(&self.link, path).with_sha256(&self.checksum)
    }
}

async fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let archive = archive.to_owned();
    let dest = dest.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive)?;
        let mut zip = zip::ZipArchive::new(file)?;
        zip.extract(&dest)?;
        Ok(())
    })
    .await
    .map_err(|e| HexoError::Other(e.to_string()))?
}

async fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let archive = archive.to_owned();
    let dest = dest.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive)?;
        let gz = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(gz);
        tar.unpack(&dest)?;
        Ok(())
    })
    .await
    .map_err(|e| HexoError::Other(e.to_string()))?
}

fn find_java_bin_in(dir: &Path) -> Option<PathBuf> {
    let java_name = if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    };

    let find_in = |root: &Path| {
        [
            root.join("bin").join(java_name),
            root.join("Contents/Home/bin").join(java_name),
        ]
        .into_iter()
        .find(|candidate| candidate.is_file())
    };
    find_in(dir).or_else(|| {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .find_map(|entry| find_in(&entry.path()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn extracted_java_is_reused_without_network_or_extraction() {
        for layout in ["bin", "jdk-21/bin", "jdk-21/Contents/Home/bin"] {
            let temp = tempfile::tempdir().unwrap();
            let java_dir = temp.path().join("java/21");
            let bin_dir = java_dir.join("jre").join(layout);
            fs::create_dir_all(&bin_dir).await.unwrap();
            let java_bin = bin_dir.join(if cfg!(windows) { "java.exe" } else { "java" });
            fs::write(&java_bin, b"existing runtime").await.unwrap();
            let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let collected = events.clone();
            let progress: ProgressFn = std::sync::Arc::new(move |done, total, stage| {
                collected
                    .lock()
                    .unwrap()
                    .push((done, total, stage.to_string()));
            });
            let actual = download_java_with_progress(21, temp.path(), progress)
                .await
                .unwrap();
            assert_eq!(actual, java_bin);
            assert_eq!(download_java(21, temp.path()).await.unwrap(), java_bin);
            assert_eq!(fs::read(&java_bin).await.unwrap(), b"existing runtime");
            assert_eq!(
                *events.lock().unwrap(),
                vec![(0, 0, "Using installed Java".into())]
            );
            assert_eq!(std::fs::read_dir(&java_dir).unwrap().count(), 1);
        }
    }

    #[test]
    fn java_directory_is_not_mistaken_for_executable() {
        let temp = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "java.exe" } else { "java" };
        std::fs::create_dir_all(temp.path().join("bin").join(name)).unwrap();
        assert!(find_java_bin_in(temp.path()).is_none());
    }

    #[tokio::test]
    async fn adoptium_package_validates_cached_archive_with_sha256() {
        let package: AdoptiumPackage = serde_json::from_str(
            r#"{
            "name": "jre.zip", "link": "http://127.0.0.1:0/jre.zip",
            "checksum": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        }"#,
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(&package.name);
        fs::write(&path, b"abc").await.unwrap();
        let task = package.download_task(&path);
        assert!(task.sha1.is_none());
        assert_eq!(task.sha256.as_deref(), Some(package.checksum.as_str()));
        crate::download::download_file(&task).await.unwrap();
    }
}
