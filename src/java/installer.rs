use std::path::{Path, PathBuf};
use tokio::fs;

use crate::{
    download::{download_file, DownloadTask},
    error::{HexoError, Result},
};

const ADOPTIUM_API: &str = "https://api.adoptium.net/v3/assets/latest";

/// Download an Adoptium Temurin JRE to `{base_dir}/java/{version}/`.
/// Returns the full path to the java executable.
pub async fn download_java(version: u32, base_dir: &Path) -> Result<PathBuf> {
    let java_dir = base_dir.join("java").join(version.to_string());
    fs::create_dir_all(&java_dir).await?;

    let (os, arch, archive_type) = platform_info();
    let url = format!(
        "{}/{}/hotspot?image_type=jre&os={}&architecture={}&vendor=eclipse",
        ADOPTIUM_API, version, os, arch
    );

    let client = reqwest::Client::new();
    let releases: Vec<AdoptiumRelease> = client.get(&url).send().await?.json().await?;

    let release = releases
        .into_iter()
        .next()
        .ok_or_else(|| HexoError::JavaNotFound { required: version })?;

    let binary = &release.binary;
    let pkg = &binary.package;

    let archive_path = java_dir.join(&pkg.name);
    download_file(&DownloadTask::new(&pkg.link, &archive_path).with_sha1(pkg.checksum.clone()))
        .await?;

    let extract_dir = java_dir.join("jre");
    fs::create_dir_all(&extract_dir).await?;

    if archive_type == "zip" {
        extract_zip(&archive_path, &extract_dir).await?;
    } else {
        extract_tar_gz(&archive_path, &extract_dir).await?;
    }

    let java_bin =
        find_java_bin_in(&extract_dir).ok_or(HexoError::JavaNotFound { required: version })?;
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

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("bin").join(java_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    let direct = dir.join("bin").join(java_name);
    if direct.exists() {
        return Some(direct);
    }
    None
}
