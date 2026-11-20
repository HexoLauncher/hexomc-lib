use std::path::{Path, PathBuf};

use super::{atpack, cfpack, detect_modpack_format, ftbpack, hmclpack, mrpack, state};
use super::{ModpackFormat, ModpackInfo};
use crate::browse::{Browser, Platform, ProjectKind, ProjectVersion, VersionFilter};
use crate::download::{download_file, DownloadTask};
use crate::error::{HexoError, Result};
use crate::mods::curseforge::{file_curseforge_fingerprint, CurseForgeClient};
use crate::mods::modrinth::ModrinthClient;

#[derive(Debug, Clone)]
pub struct ModpackPreview {
    pub format: ModpackFormat,
    pub info: ModpackInfo,
    pub file_count: usize,
}

/// Inspect an archive without downloading or installing its contents.
pub async fn inspect_modpack(path: &Path) -> Result<ModpackPreview> {
    let format = detect_modpack_format(path).await?;
    let (info, file_count) = match format {
        ModpackFormat::Modrinth => {
            let index = mrpack::read_mrpack_index(path).await?;
            (
                index.info()?,
                index
                    .files
                    .iter()
                    .filter(|file| file.is_client_supported())
                    .count(),
            )
        }
        ModpackFormat::CurseForge => {
            let manifest = cfpack::read_cf_manifest(path).await?;
            (
                manifest.info()?,
                manifest.files.iter().filter(|file| file.required).count(),
            )
        }
        ModpackFormat::Hmcl => (
            hmclpack::read_hmcl_pack(path).await?.info()?,
            count_zip_files(path, "minecraft/", Some("minecraft/pack.json")).await?,
        ),
        ModpackFormat::Mcbbs => (
            hmclpack::read_mcbbs_manifest(path).await?.info()?,
            count_zip_files(path, "overrides/", None).await?,
        ),
    };
    Ok(ModpackPreview {
        format,
        info,
        file_count,
    })
}

async fn count_zip_files(
    path: &Path,
    prefix: &'static str,
    skip: Option<&'static str>,
) -> Result<usize> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let mut count = 0;
        for name in archive.file_names() {
            let name = name?;
            if name.starts_with(prefix) && !name.ends_with('/') && Some(name.as_ref()) != skip {
                count += 1;
            }
        }
        Ok(count)
    })
    .await
    .map_err(|error| HexoError::Other(error.to_string()))?
}

#[derive(Debug, Clone)]
pub struct IdentifiedModpack {
    pub platform: Platform,
    pub project_id: String,
    pub version_id: String,
    pub version_name: String,
}

/// Calculate a file's SHA-1 without loading the whole archive into memory.
pub async fn modpack_file_sha1(path: &Path) -> Result<String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || state::sha1_file(&path))
        .await
        .map_err(|error| HexoError::Other(error.to_string()))?
}

/// Identify a local archive by its published hash or fingerprint.
pub async fn identify_modpack(
    path: &Path,
    format: ModpackFormat,
    curseforge: Option<&CurseForgeClient>,
) -> Result<Option<IdentifiedModpack>> {
    match format {
        ModpackFormat::Modrinth => {
            let hash = modpack_file_sha1(path).await?;
            Ok(ModrinthClient::new()
                .get_version_by_hash(&hash)
                .await?
                .map(|version| IdentifiedModpack {
                    platform: Platform::Modrinth,
                    project_id: version.project_id,
                    version_id: version.id,
                    version_name: version.name,
                }))
        }
        ModpackFormat::CurseForge => {
            let Some(client) = curseforge else {
                return Ok(None);
            };
            let fingerprint = file_curseforge_fingerprint(path).await?;
            Ok(client
                .match_fingerprints(&[fingerprint])
                .await?
                .into_iter()
                .next()
                .map(|entry| IdentifiedModpack {
                    platform: Platform::CurseForge,
                    project_id: entry.mod_id.to_string(),
                    version_id: entry.file.id.to_string(),
                    version_name: entry.file.display_name,
                }))
        }
        _ => Ok(None),
    }
}

#[derive(Debug, Clone)]
pub struct ModpackUpdateCandidates {
    pub current_version_found: bool,
    pub versions: Vec<ProjectVersion>,
}

/// Find downloadable releases before the installed version in the platform's newest-first list.
/// When the installed version cannot be found, the list contains all downloadable releases.
pub async fn modpack_update_candidates(
    browser: &Browser,
    platform: Platform,
    project_id: &str,
    current_id: &str,
) -> Result<ModpackUpdateCandidates> {
    let mut versions = browser
        .versions(
            platform,
            ProjectKind::Modpack,
            project_id,
            &VersionFilter::default(),
        )
        .await?;
    let current = versions.iter().position(|version| version.id == current_id);
    if let Some(index) = current {
        versions.truncate(index);
    }
    versions.retain(|version| version.downloadable);
    Ok(ModpackUpdateCandidates {
        current_version_found: current.is_some(),
        versions,
    })
}

/// A downloaded archive whose directory is removed when the handle is dropped.
pub struct DownloadedModpack {
    _temp: tempfile::TempDir,
    path: PathBuf,
}

impl DownloadedModpack {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Fetch the Modrinth or CurseForge archive and verify its published checksum when available.
pub async fn download_modpack_archive(
    browser: &Browser,
    platform: Platform,
    project_id: &str,
    version_id: &str,
    temp_root: &Path,
) -> Result<DownloadedModpack> {
    let file = browser
        .file(platform, ProjectKind::Modpack, project_id, version_id)
        .await?;
    tokio::fs::create_dir_all(temp_root).await?;
    let temp = tempfile::Builder::new()
        .prefix("modpack-")
        .tempdir_in(temp_root)?;
    let filename = if platform == Platform::Modrinth {
        "pack.mrpack"
    } else {
        "pack.zip"
    };
    let path = temp.path().join(filename);
    let mut task = DownloadTask::new(file.url, &path);
    if let Some(sha1) = file.sha1 {
        task = task.with_sha1(sha1);
    }
    download_file(&task).await?;
    Ok(DownloadedModpack { _temp: temp, path })
}

/// An online modpack with a version manifest instead of a single archive.
pub enum OnlineModpack {
    AtLauncher {
        name: String,
        version: String,
        config: atpack::AtPackConfig,
    },
    Ftb {
        name: String,
        manifest: ftbpack::FtbVersionManifest,
    },
}

impl OnlineModpack {
    pub async fn fetch(platform: Platform, pack_id: &str, version_id: &str) -> Result<Self> {
        fn numeric_id(id: &str) -> Result<u64> {
            id.parse()
                .map_err(|_| HexoError::Other(format!("invalid numeric modpack ID: {id}")))
        }

        let pack_id = pack_id.trim();
        let version_id = version_id.trim();
        match platform {
            Platform::AtLauncher => {
                if pack_id.is_empty()
                    || !pack_id.chars().all(|ch| ch.is_ascii_alphanumeric())
                    || version_id.is_empty()
                    || version_id.contains("..")
                    || !version_id
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ' '))
                {
                    return Err(HexoError::Other(
                        "invalid ATLauncher pack or version ID".into(),
                    ));
                }
                let config = atpack::fetch_atlauncher_pack_config(pack_id, version_id).await?;
                Ok(Self::AtLauncher {
                    name: pack_id.into(),
                    version: version_id.into(),
                    config,
                })
            }
            Platform::Ftb => {
                let pack_id = numeric_id(pack_id)?;
                let version_id = numeric_id(version_id)?;
                let (pack, manifest) = tokio::try_join!(
                    ftbpack::get_ftb_pack(pack_id),
                    ftbpack::fetch_ftb_version_manifest(pack_id, version_id),
                )?;
                Ok(Self::Ftb {
                    name: pack.name,
                    manifest,
                })
            }
            _ => Err(HexoError::UnsupportedPlatform(format!(
                "{platform:?} uses an archive"
            ))),
        }
    }

    pub fn info(&self) -> Result<ModpackInfo> {
        match self {
            Self::AtLauncher { name, config, .. } => config.info(name),
            Self::Ftb { name, manifest } => manifest.info(name),
        }
    }

    pub fn file_count(&self, include_optional: bool) -> usize {
        match self {
            Self::AtLauncher { config, .. } => config
                .mods
                .iter()
                .filter(|file| {
                    file.client && (!file.optional || (include_optional && file.recommended))
                })
                .count(),
            Self::Ftb { manifest, .. } => manifest
                .files
                .iter()
                .filter(|file| !file.serveronly && (!file.optional || include_optional))
                .count(),
        }
    }
}
