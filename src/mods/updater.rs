use std::collections::HashMap;
use std::path::Path;

use sha1::{Digest, Sha1};

use crate::{
    download::{download_file, DownloadTask},
    error::{HexoError, Result},
    mods::{
        curseforge::{CfModFile, CurseForgeClient},
        detector::{detect_mods, ModInfo, ModLoader},
        modrinth::{ModrinthClient, MrVersion},
    },
};

/// Where an available update came from.
#[derive(Debug, Clone)]
pub enum UpdateSource {
    Modrinth(MrVersion),
    CurseForge { file: CfModFile, project_id: u64 },
}

#[derive(Debug, Clone)]
pub struct ModUpdate {
    pub current: ModInfo,
    pub source: UpdateSource,
}

impl ModUpdate {
    pub fn version_name(&self) -> &str {
        match &self.source {
            UpdateSource::Modrinth(v) => &v.version_number,
            UpdateSource::CurseForge { file, .. } => &file.display_name,
        }
    }

    pub fn file_name(&self) -> Option<&str> {
        match &self.source {
            UpdateSource::Modrinth(v) => v.file_name(),
            UpdateSource::CurseForge { file, .. } => Some(&file.file_name),
        }
    }

    pub fn download_url(&self) -> Option<&str> {
        match &self.source {
            UpdateSource::Modrinth(v) => v.download_url(),
            UpdateSource::CurseForge { file, .. } => file.download_url.as_deref(),
        }
    }

    pub fn sha1(&self) -> Option<&str> {
        match &self.source {
            UpdateSource::Modrinth(v) => v.sha1(),
            UpdateSource::CurseForge { file, .. } => file.sha1(),
        }
    }
}

/// Scan a mods directory and check for updates, preferring Modrinth.
///
/// Modrinth is queried first by file hash, so no ID map is needed for mods it
/// knows. `mod_id_map` (mod_id -> CurseForge project ID) and `curseforge` are
/// only a fallback for mods Modrinth doesn't recognise; pass `None` and an empty
/// map to use Modrinth exclusively.
pub async fn check_updates(
    mods_dir: &Path,
    mc_version: &str,
    loader: &ModLoader,
    modrinth: &ModrinthClient,
    curseforge: Option<&CurseForgeClient>,
    mod_id_map: &HashMap<String, u64>,
) -> Result<Vec<ModUpdate>> {
    let mods = detect_mods(mods_dir);
    let mut updates = Vec::new();

    for mod_info in mods {
        let sha1 = file_sha1(&mod_info.file_path);

        if let Some(hash) = &sha1 {
            if let Some(latest) = modrinth
                .get_latest_by_hash(hash, mc_version, loader)
                .await?
            {
                // A different primary-file hash means a newer file.
                if latest.sha1() != Some(hash.as_str()) {
                    updates.push(ModUpdate {
                        current: mod_info,
                        source: UpdateSource::Modrinth(latest),
                    });
                }
                continue;
            }
        }

        let (Some(cf), Some(&cf_id)) = (curseforge, mod_id_map.get(&mod_info.mod_id)) else {
            continue;
        };

        if let Some(latest_file) = cf.get_latest_file(cf_id, mc_version, loader).await? {
            if latest_file.display_name != mod_info.version
                && latest_file.file_name != mod_info.file_name
            {
                updates.push(ModUpdate {
                    current: mod_info,
                    source: UpdateSource::CurseForge {
                        file: latest_file,
                        project_id: cf_id,
                    },
                });
            }
        }
    }

    Ok(updates)
}

/// Download the update and replace the old mod file.
pub async fn update_mod(update: &ModUpdate, mods_dir: &Path) -> Result<()> {
    let url = update
        .download_url()
        .ok_or_else(|| HexoError::DownloadFailed {
            url: format!("模組 {} 無下載 URL", update.current.name),
        })?;
    let file_name = update
        .file_name()
        .ok_or_else(|| HexoError::DownloadFailed {
            url: format!("模組 {} 無檔名", update.current.name),
        })?;

    let new_path = mods_dir.join(file_name);

    let task = match update.sha1() {
        Some(sha1) if !sha1.is_empty() => {
            DownloadTask::new(url, &new_path).with_sha1(sha1.to_string())
        }
        _ => DownloadTask::new(url, &new_path),
    };

    download_file(&task).await?;

    if update.current.file_path != new_path && update.current.file_path.exists() {
        tokio::fs::remove_file(&update.current.file_path).await?;
    }

    Ok(())
}

fn file_sha1(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let mut hasher = Sha1::new();
    hasher.update(&data);
    Some(hex::encode(hasher.finalize()))
}
