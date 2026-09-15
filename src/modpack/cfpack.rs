//! CurseForge modpacks (a zip holding `manifest.json` and `overrides/`).
//!
//! The manifest only lists project/file IDs, so download URLs are resolved through the
//! CurseForge API (API key required). Files whose authors disallow third-party downloads
//! have no `downloadUrl` and are returned as [`ManualDownload`]s.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    download::{download_batch, DownloadTask},
    error::{HexoError, Result},
    install::{loader::ProgressFn, vanilla::LoaderType},
    modpack::{
        extract_zip_dir, install_pack_loader, instance_game_dir, read_zip_json, safe_join,
        ManualDownload, ModpackInfo, ModpackInstallResult,
    },
    mods::curseforge::CurseForgeClient,
};

pub const MANIFEST_FILE: &str = "manifest.json";

/// CurseForge `classId` of resource packs.
const CLASS_RESOURCE_PACKS: u32 = 12;
/// CurseForge `classId` of shader packs.
const CLASS_SHADERS: u32 = 6552;

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CfManifest {
    pub minecraft: CfManifestMinecraft,
    #[serde(default)]
    pub manifest_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub files: Vec<CfManifestFile>,
    #[serde(default = "default_overrides")]
    pub overrides: String,
}

fn default_overrides() -> String {
    "overrides".into()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CfManifestMinecraft {
    pub version: String,
    #[serde(default)]
    pub mod_loaders: Vec<CfManifestLoader>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CfManifestLoader {
    /// e.g. `forge-47.2.0`, `neoforge-21.1.77`, `fabric-0.16.5`.
    pub id: String,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CfManifestFile {
    #[serde(rename = "projectID")]
    pub project_id: u64,
    #[serde(rename = "fileID")]
    pub file_id: u64,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

impl CfManifest {
    pub fn info(&self) -> Result<ModpackInfo> {
        let loader = self
            .minecraft
            .mod_loaders
            .iter()
            .find(|l| l.primary)
            .or_else(|| self.minecraft.mod_loaders.first());

        let (loader, loader_version) = match loader {
            None => (LoaderType::Vanilla, None),
            Some(l) => {
                let (kind, ver) = l
                    .id
                    .split_once('-')
                    .ok_or_else(|| HexoError::UnsupportedLoader(l.id.clone()))?;
                let kind = match kind {
                    "forge" => LoaderType::Forge,
                    "neoforge" => LoaderType::NeoForge,
                    "fabric" => LoaderType::Fabric,
                    _ => return Err(HexoError::UnsupportedLoader(l.id.clone())),
                };
                (kind, Some(ver.to_string()))
            }
        };

        Ok(ModpackInfo {
            name: self.name.clone(),
            version: self.version.clone(),
            mc_version: self.minecraft.version.clone(),
            loader,
            loader_version,
        })
    }
}

/// Read a CurseForge modpack's manifest without installing it.
pub async fn read_cf_manifest(pack_path: &Path) -> Result<CfManifest> {
    read_zip_json(pack_path, MANIFEST_FILE).await
}

/// Install a CurseForge modpack into `instance/{instance_name}`. Only `required` files
/// are downloaded.
pub async fn install_cfpack(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    curseforge: &CurseForgeClient,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let manifest = read_cf_manifest(pack_path).await?;
    let info = manifest.info()?;

    install_pack_loader(&info, instance_name, base_dir, java_path, progress.clone()).await?;

    let game_dir = instance_game_dir(base_dir, instance_name);

    progress(0, 0, "Extracting overrides");
    extract_zip_dir(pack_path, &manifest.overrides, &game_dir).await?;

    let wanted: Vec<&CfManifestFile> = manifest.files.iter().filter(|f| f.required).collect();
    if wanted.is_empty() {
        return Ok(ModpackInstallResult { info, manual_downloads: Vec::new(), skipped: Vec::new() });
    }

    progress(0, 0, "Resolving CurseForge files");
    let file_ids: Vec<u64> = wanted.iter().map(|f| f.file_id).collect();
    let project_ids: Vec<u64> = wanted.iter().map(|f| f.project_id).collect();
    let files = curseforge.get_files(&file_ids).await?;
    let projects: HashMap<u64, _> = curseforge
        .get_mods(&project_ids)
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();

    let mut tasks = Vec::new();
    let mut manual_downloads = Vec::new();
    for file in files {
        let project = projects.get(&file.mod_id);
        let subdir = match project.and_then(|p| p.class_id) {
            Some(CLASS_RESOURCE_PACKS) => "resourcepacks",
            Some(CLASS_SHADERS) => "shaderpacks",
            _ => "mods",
        };
        let dest = safe_join(&game_dir.join(subdir), &file.file_name)?;

        match &file.download_url {
            Some(url) => {
                let mut task = DownloadTask::new(url, dest);
                if let Some(sha1) = file.sha1() {
                    task = task.with_sha1(sha1);
                }
                tasks.push(task);
            }
            None => manual_downloads.push(ManualDownload {
                name: project.map_or_else(|| file.display_name.clone(), |p| p.name.clone()),
                file_name: file.file_name.clone(),
                website: project
                    .and_then(|p| p.links.as_ref())
                    .and_then(|l| l.website_url.as_ref())
                    .map(|u| format!("{}/files/{}", u, file.id)),
                dest,
            }),
        }
    }

    let p = progress.clone();
    download_batch(tasks, 8, move |d, t| p(d, t, "Downloading modpack files")).await?;

    Ok(ModpackInstallResult { info, manual_downloads, skipped: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manifest() {
        let json = r#"{
            "minecraft": {
                "version": "1.20.1",
                "modLoaders": [{ "id": "forge-47.2.0", "primary": true }]
            },
            "manifestType": "minecraftModpack",
            "manifestVersion": 1,
            "name": "Test",
            "version": "1.0",
            "author": "someone",
            "files": [
                { "projectID": 1, "fileID": 2, "required": true },
                { "projectID": 3, "fileID": 4, "required": false }
            ],
            "overrides": "overrides"
        }"#;
        let manifest: CfManifest = serde_json::from_str(json).unwrap();
        let info = manifest.info().unwrap();
        assert_eq!(info.mc_version, "1.20.1");
        assert_eq!(info.loader, LoaderType::Forge);
        assert_eq!(info.loader_version.as_deref(), Some("47.2.0"));
        assert_eq!(manifest.files.iter().filter(|f| f.required).count(), 1);
    }
}
