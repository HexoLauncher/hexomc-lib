use std::path::{Path, PathBuf};
use tokio::fs;

use crate::{
    download::{download_file_with_progress, DownloadTask},
    error::{HexoError, Result},
    install::loader::{no_progress, ProgressFn},
};

const ADOPTIUM_API: &str = "https://api.adoptium.net/v3/assets/latest";
const ADOPTIUM_RELEASES_API: &str = "https://api.adoptium.net/v3/info/available_releases";
const ZULU_API: &str = "https://api.azul.com/metadata/v1/zulu/packages";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JavaVendor {
    Temurin,
    Zulu,
}

impl JavaVendor {
    pub const ALL: [JavaVendor; 2] = [JavaVendor::Temurin, JavaVendor::Zulu];

    pub fn id(self) -> &'static str {
        match self {
            JavaVendor::Temurin => "temurin",
            JavaVendor::Zulu => "zulu",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            JavaVendor::Temurin => "Eclipse Temurin",
            JavaVendor::Zulu => "Azul Zulu",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JavaRelease {
    pub version: u32,
    pub lts: bool,
}

/// Download a JRE to `{base_dir}/java/{version}/`: Adoptium Temurin, or Azul Zulu
/// when Temurin has no build for this platform (e.g. Java 8 on Apple Silicon).
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
    install_java_into(&java_dir, version, None, progress).await
}

pub async fn download_vendor_java_with_progress(
    vendor: JavaVendor,
    version: u32,
    base_dir: &Path,
    progress: ProgressFn,
) -> Result<PathBuf> {
    let java_dir = base_dir
        .join("java")
        .join(format!("{}-{}", vendor.id(), version));
    install_java_into(&java_dir, version, Some(vendor), progress).await
}

pub async fn list_java_releases(vendor: JavaVendor) -> Result<Vec<JavaRelease>> {
    let client = reqwest::Client::new();
    let mut versions = match vendor {
        JavaVendor::Temurin => {
            let info: AdoptiumAvailableReleases = client
                .get(ADOPTIUM_RELEASES_API)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            info.available_releases
        }
        JavaVendor::Zulu => {
            let listed: Vec<ZuluListedPackage> = client
                .get(zulu_packages_url(None))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            listed
                .into_iter()
                .filter(|p| !p.name.contains("musl"))
                .filter_map(|p| p.java_version.first().copied())
                .collect()
        }
    };
    versions.sort_unstable_by(|a, b| b.cmp(a));
    versions.dedup();
    Ok(versions
        .into_iter()
        .map(|version| JavaRelease {
            version,
            lts: is_lts(version),
        })
        .collect())
}

fn is_lts(version: u32) -> bool {
    version == 8 || version == 11 || (version >= 17 && (version - 17) % 4 == 0)
}

async fn fetch_package(
    client: &reqwest::Client,
    version: u32,
    vendor: Option<JavaVendor>,
) -> Result<Option<JavaPackage>> {
    match vendor {
        Some(JavaVendor::Temurin) => fetch_adoptium_package(client, version).await,
        Some(JavaVendor::Zulu) => fetch_zulu_package(client, version).await,
        None => match fetch_adoptium_package(client, version).await? {
            Some(pkg) => Ok(Some(pkg)),
            None => fetch_zulu_package(client, version).await,
        },
    }
}

async fn install_java_into(
    java_dir: &Path,
    version: u32,
    vendor: Option<JavaVendor>,
    progress: ProgressFn,
) -> Result<PathBuf> {
    let extract_dir = java_dir.join("jre");
    if let Some(java_bin) = find_java_bin_in(&extract_dir) {
        progress(0, 0, "Using installed Java");
        return Ok(java_bin);
    }
    fs::create_dir_all(java_dir).await?;

    progress(0, 0, "Fetching Java release");
    let (_, _, archive_type) = platform_info();
    let client = reqwest::Client::new();
    let pkg = fetch_package(&client, version, vendor)
        .await?
        .ok_or(HexoError::JavaNotFound { required: version })?;

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

/// A downloadable JRE archive, whichever vendor it came from.
struct JavaPackage {
    name: String,
    url: String,
    sha256: String,
    size: u64,
}

impl JavaPackage {
    /// Both vendors publish SHA-256 checksums, not SHA1.
    fn download_task(&self, path: &Path) -> DownloadTask {
        DownloadTask::new(&self.url, path).with_sha256(&self.sha256)
    }
}

async fn fetch_adoptium_package(client: &reqwest::Client, version: u32) -> Result<Option<JavaPackage>> {
    let (os, arch, _) = platform_info();
    let url = format!(
        "{}/{}/hotspot?image_type=jre&os={}&architecture={}&vendor=eclipse",
        ADOPTIUM_API, version, os, arch
    );
    let releases: Vec<AdoptiumRelease> = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(releases.into_iter().next().map(|r| r.binary.package.into()))
}

/// Azul's package list has no checksum, so the chosen package's details are fetched too.
async fn fetch_zulu_package(client: &reqwest::Client, version: u32) -> Result<Option<JavaPackage>> {
    let listed: Vec<ZuluListedPackage> = client
        .get(zulu_packages_url(Some(version)))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let Some(listed) = listed.into_iter().find(|p| !p.name.contains("musl")) else {
        return Ok(None);
    };
    let details: ZuluPackage = client
        .get(format!("{}/{}", ZULU_API, listed.package_uuid))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(Some(details.into()))
}

fn zulu_packages_url(version: Option<u32>) -> String {
    let (os, arch, archive_type) = zulu_platform_info();
    let (version, page_size) = match version {
        Some(v) => (format!("java_version={}&", v), 10),
        None => (String::new(), 1000),
    };
    format!(
        "{}/?{}os={}&arch={}&archive_type={}&java_package_type=jre\
         &javafx_bundled=false&crac_supported=false&latest=true&release_status=ga\
         &availability_types=CA&page_size={}",
        ZULU_API, version, os, arch, archive_type, page_size
    )
}

fn zulu_platform_info() ->(&'static str, &'static str, &'static str) {
    let (os, _, archive) = platform_info();
    let os = if os == "mac" { "macos" } else { os };
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86"
    };
    (os, arch, archive)
}

#[derive(serde::Deserialize)]
struct ZuluListedPackage {
    name: String,
    package_uuid: String,
    #[serde(default)]
    java_version: Vec<u32>,
}

#[derive(serde::Deserialize)]
struct AdoptiumAvailableReleases {
    available_releases: Vec<u32>,
}

#[derive(serde::Deserialize)]
struct ZuluPackage {
    name: String,
    download_url: String,
    sha256_hash: String,
    #[serde(default)]
    size: u64,
}

impl From<ZuluPackage> for JavaPackage {
    fn from(p: ZuluPackage) -> Self {
        Self { name: p.name, url: p.download_url, sha256: p.sha256_hash, size: p.size }
    }
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

impl From<AdoptiumPackage> for JavaPackage {
    fn from(p: AdoptiumPackage) -> Self {
        Self { name: p.name, url: p.link, sha256: p.checksum, size: p.size }
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

    #[test]
    fn lts_versions() {
        let lts: Vec<u32> = (8..=30).filter(|&v| is_lts(v)).collect();
        assert_eq!(lts, [8, 11, 17, 21, 25, 29]);
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn vendors_list_current_lts_releases() {
        for vendor in JavaVendor::ALL {
            let releases = list_java_releases(vendor).await.unwrap();
            assert!(releases.windows(2).all(|w| w[0].version > w[1].version));
            for lts in [8, 17, 21] {
                let release = JavaRelease { version: lts, lts: true };
                assert!(releases.contains(&release), "{vendor:?} {lts}");
            }
        }
    }

    #[test]
    fn zulu_listing_url_has_optional_version_filter() {
        assert!(!zulu_packages_url(None).contains("java_version="));
        assert!(zulu_packages_url(Some(21)).contains("java_version=21&"));
    }

    #[tokio::test]
    async fn vendor_java_is_reused_from_vendor_folder() {
        let temp = tempfile::tempdir().unwrap();
        let bin_dir = temp.path().join("java/zulu-17/jre/zulu17-jre/bin");
        fs::create_dir_all(&bin_dir).await.unwrap();
        let java_bin = bin_dir.join(if cfg!(windows) { "java.exe" } else { "java" });
        fs::write(&java_bin, b"existing runtime").await.unwrap();
        let actual =
            download_vendor_java_with_progress(JavaVendor::Zulu, 17, temp.path(), no_progress())
                .await
                .unwrap();
        assert_eq!(actual, java_bin);
    }

    #[test]
    fn zulu_package_details_parse() {
        let package: ZuluPackage = serde_json::from_str(
            r#"{
            "name": "zulu8-jre.tar.gz", "download_url": "https://cdn.azul.com/zulu8-jre.tar.gz",
            "sha256_hash": "2bf6", "size": 40158500, "md5_hash": "a530"
        }"#,
        )
        .unwrap();
        let package = JavaPackage::from(package);
        assert_eq!(package.url, "https://cdn.azul.com/zulu8-jre.tar.gz");
        assert_eq!(package.sha256, "2bf6");
        assert_eq!(package.size, 40158500);
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
        let package = JavaPackage::from(package);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(&package.name);
        fs::write(&path, b"abc").await.unwrap();
        let task = package.download_task(&path);
        assert!(task.sha1.is_none());
        assert_eq!(task.sha256.as_deref(), Some(package.sha256.as_str()));
        crate::download::download_file(&task).await.unwrap();
    }
}
