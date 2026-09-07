use std::path::Path;

use crate::{
    download::{download_file, DownloadTask},
    error::{HexoError, Result},
    mods::{
        curseforge::{CfModFile, CurseForgeClient},
        detector::{detect_mods, ModInfo, ModLoader},
    },
};

#[derive(Debug, Clone)]
pub struct ModUpdate {
    pub current: ModInfo,
    pub new_file: CfModFile,
    pub curseforge_id: u64,
}

/// Scan a mods directory and check CurseForge for updates.
///
/// `mod_id_map` maps a mod_id (fabric/forge) to its CurseForge project ID.
pub async fn check_updates(
    mods_dir: &Path,
    mc_version: &str,
    loader: &ModLoader,
    cf_client: &CurseForgeClient,
    mod_id_map: &std::collections::HashMap<String, u64>,
) -> Result<Vec<ModUpdate>> {
    let mods = detect_mods(mods_dir);
    let mut updates = Vec::new();

    for mod_info in mods {
        let Some(&cf_id) = mod_id_map.get(&mod_info.mod_id) else {
            continue;
        };

        let latest = cf_client
            .get_latest_file(cf_id, mc_version, loader)
            .await?;

        if let Some(latest_file) = latest {
            // Treat a different version name as an update.
            if latest_file.display_name != mod_info.version
                && latest_file.file_name != mod_info.file_name
            {
                updates.push(ModUpdate {
                    current: mod_info,
                    new_file: latest_file,
                    curseforge_id: cf_id,
                });
            }
        }
    }

    Ok(updates)
}

/// Download the update and replace the old mod file.
pub async fn update_mod(update: &ModUpdate, mods_dir: &Path) -> Result<()> {
    let download_url = update
        .new_file
        .download_url
        .as_deref()
        .ok_or_else(|| HexoError::DownloadFailed {
            url: format!("CurseForge mod {} 無下載 URL", update.curseforge_id),
        })?;

    let new_path = mods_dir.join(&update.new_file.file_name);
    let sha1 = update.new_file.sha1().unwrap_or("").to_string();

    let task = if sha1.is_empty() {
        DownloadTask::new(download_url, &new_path)
    } else {
        DownloadTask::new(download_url, &new_path).with_sha1(sha1)
    };

    download_file(&task).await?;

    if update.current.file_path != new_path && update.current.file_path.exists() {
        tokio::fs::remove_file(&update.current.file_path).await?;
    }

    Ok(())
}
