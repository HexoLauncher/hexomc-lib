//! Modrinth modpacks (`.mrpack`).
//!
//! A zip holding `modrinth.index.json` plus `overrides/` and `client-overrides/`.
//! Spec: <https://support.modrinth.com/en/articles/8802351-modrinth-modpack-format-mrpack>

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    download::{download_batch, DownloadTask},
    error::{HexoError, Result},
    install::{loader::ProgressFn, vanilla::LoaderType},
    modpack::{
        extract_zip_dir, install_pack_loader, instance_game_dir, read_zip_json, safe_join,
        ModpackInfo, ModpackInstallResult,
    },
};

pub const INDEX_FILE: &str = "modrinth.index.json";

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MrpackIndex {
    pub format_version: u32,
    pub game: String,
    pub version_id: String,
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    pub files: Vec<MrpackFile>,
    /// `minecraft`, `forge`, `neoforge`, `fabric-loader` or `quilt-loader` -> version.
    pub dependencies: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MrpackFile {
    /// Path relative to the game directory, e.g. `mods/sodium.jar`.
    pub path: String,
    pub hashes: HashMap<String, String>,
    #[serde(default)]
    pub env: Option<MrpackEnv>,
    pub downloads: Vec<String>,
    #[serde(default)]
    pub file_size: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MrpackEnv {
    pub client: EnvSupport,
    pub server: EnvSupport,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EnvSupport {
    Required,
    Optional,
    Unsupported,
}

impl MrpackFile {
    pub fn is_client_supported(&self) -> bool {
        self.env
            .as_ref()
            .is_none_or(|e| e.client != EnvSupport::Unsupported)
    }
}

impl MrpackIndex {
    pub fn info(&self) -> Result<ModpackInfo> {
        let mc_version = self
            .dependencies
            .get("minecraft")
            .cloned()
            .ok_or_else(|| HexoError::Other("mrpack has no minecraft dependency".into()))?;

        let deps = &self.dependencies;
        let (loader, loader_version) = if let Some(v) = deps.get("neoforge") {
            (LoaderType::NeoForge, Some(v.clone()))
        } else if let Some(v) = deps.get("forge") {
            (LoaderType::Forge, Some(v.clone()))
        } else if let Some(v) = deps.get("fabric-loader") {
            (LoaderType::Fabric, Some(v.clone()))
        } else if let Some(v) = deps.get("quilt-loader") {
            return Err(HexoError::UnsupportedLoader(format!("quilt-loader {}", v)));
        } else {
            (LoaderType::Vanilla, None)
        };

        Ok(ModpackInfo {
            name: self.name.clone(),
            version: Some(self.version_id.clone()),
            mc_version,
            loader,
            loader_version,
        })
    }
}

/// Read a `.mrpack`'s index without installing it.
pub async fn read_mrpack_index(pack_path: &Path) -> Result<MrpackIndex> {
    read_zip_json(pack_path, INDEX_FILE).await
}

/// Install a `.mrpack` into `instance/{instance_name}`.
///
/// Files whose client support is `required` or `optional` are installed; `unsupported`
/// ones are skipped. `client-overrides/` is applied after `overrides/`.
pub async fn install_mrpack(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let index = read_mrpack_index(pack_path).await?;
    if index.game != "minecraft" {
        return Err(HexoError::Other(format!("unsupported game: {}", index.game)));
    }
    let info = index.info()?;

    install_pack_loader(&info, instance_name, base_dir, java_path, progress.clone()).await?;

    let game_dir = instance_game_dir(base_dir, instance_name);

    progress(0, 0, "Extracting overrides");
    extract_zip_dir(pack_path, "overrides", &game_dir).await?;
    extract_zip_dir(pack_path, "client-overrides", &game_dir).await?;

    let mut tasks = Vec::new();
    for file in index.files.iter().filter(|f| f.is_client_supported()) {
        let url = file.downloads.first().ok_or_else(|| HexoError::DownloadFailed {
            url: format!("no download URL for {}", file.path),
        })?;
        let mut task = DownloadTask::new(url, safe_join(&game_dir, &file.path)?);
        if let Some(sha1) = file.hashes.get("sha1") {
            task = task.with_sha1(sha1);
        }
        tasks.push(task);
    }

    let p = progress.clone();
    download_batch(tasks, 8, move |d, t| p(d, t, "Downloading modpack files")).await?;

    Ok(ModpackInstallResult { info, manual_downloads: Vec::new(), skipped: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "1.0.0",
        "name": "Test Pack",
        "files": [
            {
                "path": "mods/sodium.jar",
                "hashes": { "sha1": "abc", "sha512": "def" },
                "env": { "client": "required", "server": "unsupported" },
                "downloads": ["https://cdn.modrinth.com/data/x/sodium.jar"],
                "fileSize": 123
            },
            {
                "path": "mods/server-only.jar",
                "hashes": { "sha1": "123" },
                "env": { "client": "unsupported", "server": "required" },
                "downloads": ["https://cdn.modrinth.com/data/y/server.jar"],
                "fileSize": 1
            }
        ],
        "dependencies": { "minecraft": "1.21.1", "fabric-loader": "0.16.5" }
    }"#;

    #[test]
    fn parse_index() {
        let index: MrpackIndex = serde_json::from_str(SAMPLE).unwrap();
        let info = index.info().unwrap();
        assert_eq!(info.mc_version, "1.21.1");
        assert_eq!(info.loader, LoaderType::Fabric);
        assert_eq!(info.loader_version.as_deref(), Some("0.16.5"));

        let client: Vec<_> = index.files.iter().filter(|f| f.is_client_supported()).collect();
        assert_eq!(client.len(), 1);
        assert_eq!(client[0].path, "mods/sodium.jar");
    }

    #[test]
    fn quilt_is_unsupported() {
        let mut index: MrpackIndex = serde_json::from_str(SAMPLE).unwrap();
        index.dependencies.remove("fabric-loader");
        index.dependencies.insert("quilt-loader".into(), "0.26.0".into());
        assert!(matches!(index.info(), Err(HexoError::UnsupportedLoader(_))));
    }
}
