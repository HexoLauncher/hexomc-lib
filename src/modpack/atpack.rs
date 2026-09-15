//! ATLauncher packs, installed from ATLauncher's online API.
//!
//! ATLauncher has no pack file format: a pack version is described by a remote
//! `Configs.json` (loader and file list) plus an optional `Configs.zip` of config files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::{
    download::{download_batch, DownloadTask},
    error::{HexoError, Result},
    install::{loader::ProgressFn, vanilla::LoaderType},
    modpack::{
        extract_zip_dir, install_pack_loader, instance_game_dir, safe_join, ManualDownload,
        ModpackInfo, ModpackInstallResult,
    },
};

const API_BASE: &str = "https://api.atlauncher.com/v1";
const DOWNLOAD_SERVER: &str = "https://download.nodecdn.net/containers/atl";
const USER_AGENT: &str = concat!("hexomc-lib/", env!("CARGO_PKG_VERSION"));

/// ATLauncher's API sits behind Cloudflare, which rejects requests without a User-Agent.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .unwrap_or_default()
}

/// One published version of an ATLauncher pack.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AtPackVersion {
    pub version: String,
    pub minecraft: String,
    #[serde(default)]
    pub published: i64,
}

/// A pack version's `Configs.json`.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AtPackConfig {
    pub version: String,
    pub minecraft: String,
    #[serde(default)]
    pub no_configs: bool,
    #[serde(default)]
    pub loader: Option<AtLoader>,
    #[serde(default)]
    pub mods: Vec<AtMod>,
    #[serde(default)]
    pub configs: Option<AtConfigs>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AtLoader {
    #[serde(rename = "type", default)]
    pub loader_type: String,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub class_name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AtConfigs {
    #[serde(default)]
    pub filesize: i64,
    #[serde(default)]
    pub sha1: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AtMod {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub url: String,
    pub file: String,
    /// Overrides the target directory for `mods` entries.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub download: AtDownloadType,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(rename = "type")]
    pub mod_type: AtModType,
    #[serde(default)]
    pub extract_to: Option<AtExtractTo>,
    #[serde(default)]
    pub extract_folder: Option<String>,
    #[serde(default = "default_true")]
    pub client: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default = "default_true")]
    pub recommended: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AtDownloadType {
    /// Relative URL on ATLauncher's download server.
    #[default]
    Server,
    /// Absolute URL.
    Direct,
    /// Must be downloaded by the user in a browser.
    Browser,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AtModType {
    Jar,
    Dependency,
    Depandency,
    Forge,
    Mcpc,
    Mods,
    Plugins,
    Ic2lib,
    Denlib,
    Flan,
    Coremods,
    Extract,
    Decomp,
    Millenaire,
    Texturepack,
    Resourcepack,
    Texturepackextract,
    Resourcepackextract,
    Shaderpack,
    Datapack,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AtExtractTo {
    Root,
    Mods,
    Coremods,
    #[serde(other)]
    Unknown,
}

/// How a pack entry is placed into the game directory.
enum Placement {
    /// Copy the file into this directory (relative to the game dir).
    Copy(String),
    /// Unzip `folder` inside the archive into this directory.
    Unzip { dir: String, folder: String },
    /// Server-only entry, ignored on the client.
    Ignore,
    Unsupported,
}

impl AtMod {
    pub fn download_url(&self) -> String {
        match self.download {
            AtDownloadType::Server => format!("{}/{}", DOWNLOAD_SERVER, self.url.trim_start_matches('/')),
            AtDownloadType::Direct | AtDownloadType::Browser => self.url.clone(),
        }
    }

    fn placement(&self, mc_version: &str) -> Placement {
        let copy = |dir: &str| Placement::Copy(dir.to_string());
        match self.mod_type {
            AtModType::Mods => copy(self.path.as_deref().unwrap_or("mods")),
            AtModType::Resourcepack => copy("resourcepacks"),
            AtModType::Texturepack => copy("texturepacks"),
            AtModType::Datapack => copy("datapacks"),
            AtModType::Shaderpack => copy("shaderpacks"),
            AtModType::Coremods => copy("coremods"),
            AtModType::Plugins => copy("plugins"),
            AtModType::Ic2lib => copy("mods/ic2"),
            AtModType::Denlib => copy("mods/denlib"),
            AtModType::Flan => copy("Flan"),
            AtModType::Dependency | AtModType::Depandency => copy(&format!("mods/{}", mc_version)),
            AtModType::Jar | AtModType::Forge => copy("jarmods"),
            AtModType::Texturepackextract => Placement::Unzip {
                dir: "texturepacks/extracted".into(),
                folder: String::new(),
            },
            AtModType::Resourcepackextract => Placement::Unzip {
                dir: "resourcepacks/extracted".into(),
                folder: String::new(),
            },
            AtModType::Extract => {
                let dir = match self.extract_to {
                    Some(AtExtractTo::Root) => "",
                    Some(AtExtractTo::Mods) => "mods",
                    Some(AtExtractTo::Coremods) => "coremods",
                    _ => return Placement::Unsupported,
                };
                Placement::Unzip {
                    dir: dir.into(),
                    folder: self.extract_folder.clone().unwrap_or_default(),
                }
            }
            AtModType::Mcpc => Placement::Ignore,
            AtModType::Decomp | AtModType::Millenaire | AtModType::Unknown => Placement::Unsupported,
        }
    }
}

impl AtLoader {
    fn meta_str(&self, key: &str) -> Option<String> {
        self.metadata.get(key).and_then(|v| v.as_str()).map(str::to_string)
    }
}

impl AtPackConfig {
    pub fn info(&self, pack_name: &str) -> Result<ModpackInfo> {
        let mc = &self.minecraft;
        let (loader, loader_version) = match &self.loader {
            None => (LoaderType::Vanilla, None),
            Some(l) => {
                let kind = if l.loader_type.is_empty() {
                    l.class_name.clone().unwrap_or_default()
                } else {
                    l.loader_type.clone()
                }
                .to_lowercase();

                if kind.contains("neoforge") {
                    (LoaderType::NeoForge, l.meta_str("version"))
                } else if kind.contains("forge") {
                    let version = l.meta_str("version").or_else(|| {
                        l.meta_str("rawVersion")
                            .map(|raw| raw.trim_start_matches(&format!("{}-", mc)).to_string())
                    });
                    (LoaderType::Forge, version)
                } else if kind.contains("fabric") && !kind.contains("legacy") {
                    (LoaderType::Fabric, l.meta_str("loader"))
                } else {
                    return Err(HexoError::UnsupportedLoader(kind));
                }
            }
        };

        Ok(ModpackInfo {
            name: pack_name.to_string(),
            version: Some(self.version.clone()),
            mc_version: mc.clone(),
            loader,
            loader_version,
        })
    }
}

/// List the published versions of an ATLauncher pack. `safe_name` is the pack's
/// URL-safe name, e.g. `SkyFactory4`.
pub async fn get_atlauncher_pack_versions(safe_name: &str) -> Result<Vec<AtPackVersion>> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        error: bool,
        data: Option<Data>,
    }
    #[derive(Deserialize)]
    struct Data {
        versions: Vec<AtPackVersion>,
    }

    let resp = http_client()
        .get(format!("{}/pack/{}", API_BASE, safe_name))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(HexoError::VersionNotFound(safe_name.to_string()));
    }
    let resp: Resp = resp.error_for_status()?.json().await?;
    match resp.data {
        Some(data) if !resp.error => Ok(data.versions),
        _ => Err(HexoError::VersionNotFound(safe_name.to_string())),
    }
}

/// Fetch a pack version's `Configs.json`.
pub async fn fetch_atlauncher_pack_config(safe_name: &str, version: &str) -> Result<AtPackConfig> {
    let resp = http_client()
        .get(format!("{}/packs/{}/versions/{}/Configs.json", DOWNLOAD_SERVER, safe_name, version))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(HexoError::VersionNotFound(format!("{} {}", safe_name, version)));
    }
    Ok(resp.error_for_status()?.json().await?)
}

/// Install an ATLauncher pack version into `instance/{instance_name}`.
///
/// Client-side, non-optional entries are always installed; optional entries are
/// installed only when `include_optional` is set and the pack marks them recommended.
/// `browser` downloads are returned as [`ManualDownload`]s, and entry types that need
/// ATLauncher-specific handling (`decomp`, `millenaire`) are returned in `skipped`.
pub async fn install_atlauncher_pack(
    safe_name: &str,
    version: &str,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    include_optional: bool,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let config = fetch_atlauncher_pack_config(safe_name, version).await?;
    let info = config.info(safe_name)?;

    install_pack_loader(&info, instance_name, base_dir, java_path, progress.clone()).await?;

    let game_dir = instance_game_dir(base_dir, instance_name);
    let temp_dir = safe_join(&base_dir.join("temp").join("atlauncher"), &format!("{}-{}", safe_name, version))?;
    tokio::fs::create_dir_all(&temp_dir).await?;

    let selected = config
        .mods
        .iter()
        .filter(|m| m.client && (!m.optional || (include_optional && m.recommended)));

    let mut tasks = Vec::new();
    let mut md5_checks: Vec<(PathBuf, String)> = Vec::new();
    let mut unzips: Vec<(PathBuf, PathBuf, String)> = Vec::new();
    let mut manual_downloads = Vec::new();
    let mut skipped = Vec::new();

    for m in selected {
        let dest = match m.placement(&config.minecraft) {
            Placement::Copy(dir) => safe_join(&safe_join(&game_dir, &dir)?, &m.file)?,
            Placement::Unzip { dir, folder } => {
                let archive = safe_join(&temp_dir, &m.file)?;
                unzips.push((archive.clone(), safe_join(&game_dir, &dir)?, folder));
                archive
            }
            Placement::Ignore => continue,
            Placement::Unsupported => {
                skipped.push(m.name.clone());
                continue;
            }
        };

        if m.download == AtDownloadType::Browser {
            manual_downloads.push(ManualDownload {
                name: m.name.clone(),
                file_name: m.file.clone(),
                website: m.website.clone().or_else(|| Some(m.url.clone())),
                dest,
            });
            continue;
        }

        let mut task = DownloadTask::new(m.download_url(), &dest);
        match (&m.sha1, &m.md5) {
            (Some(sha1), _) => task = task.with_sha1(sha1),
            (None, Some(md5)) => md5_checks.push((dest.clone(), md5.to_lowercase())),
            (None, None) => {}
        }
        tasks.push(task);
    }

    let p = progress.clone();
    download_batch(tasks, 8, move |d, t| p(d, t, "Downloading pack files")).await?;

    for (path, expected) in &md5_checks {
        if file_md5(path).await? != *expected {
            let _ = tokio::fs::remove_file(path).await;
            return Err(HexoError::ChecksumMismatch { path: path.display().to_string() });
        }
    }

    progress(0, 0, "Extracting pack files");
    let unzipped: Vec<PathBuf> = unzips
        .iter()
        .map(|(archive, _, _)| archive.clone())
        .collect();
    for (archive, dest, folder) in unzips {
        if manual_downloads.iter().any(|d| d.dest == archive) {
            continue;
        }
        extract_zip_dir(&archive, &folder, &dest).await?;
    }

    if let Some(configs) = config.configs.as_ref().filter(|c| !config.no_configs && c.filesize != 0) {
        progress(0, 0, "Downloading configs");
        let zip_path = temp_dir.join("Configs.zip");
        let mut task = DownloadTask::new(
            format!("{}/packs/{}/versions/{}/Configs.zip", DOWNLOAD_SERVER, safe_name, version),
            &zip_path,
        );
        if let Some(sha1) = &configs.sha1 {
            task = task.with_sha1(sha1);
        }
        crate::download::download_file(&task).await?;
        extract_zip_dir(&zip_path, "", &game_dir).await?;
    }

    let has_manual_extract = manual_downloads.iter().any(|d| unzipped.contains(&d.dest));
    if !has_manual_extract {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    Ok(ModpackInstallResult { info, manual_downloads, skipped })
}

async fn file_md5(path: &Path) -> Result<String> {
    let data = tokio::fs::read(path).await?;
    Ok(hex::encode(Md5::digest(&data)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "enableCurseIntegration": true,
        "version": "4.2.4",
        "minecraft": "1.12.2",
        "loader": {
            "type": "forge",
            "choose": false,
            "metadata": {
                "minecraft": "1.12.2",
                "version": "14.23.5.2860",
                "rawVersion": "1.12.2-14.23.5.2860",
                "installerSize": 4598857
            },
            "className": "com.atlauncher.data.loaders.forge.ForgeLoader"
        },
        "mods": [
            {
                "name": "SF4 Server Configs",
                "url": "packs/SkyFactory4/files/4.2.4/SF4-Server-Config.zip",
                "file": "SF4-Server-Config.zip",
                "download": "server",
                "md5": "b2588953d8825703579f9c0ee45285d2",
                "type": "extract",
                "extractTo": "root",
                "extractFolder": "/",
                "client": false,
                "optional": false
            },
            {
                "name": "Bed Patch",
                "url": "packs/SkyFactory4/files/4.2.4/bedpatch-2.2-1.12.2.jar",
                "file": "bedpatch-2.2-1.12.2.jar",
                "download": "server",
                "type": "mods",
                "client": true,
                "optional": false
            },
            {
                "name": "Weird",
                "url": "x",
                "file": "x.jar",
                "type": "somethingnew"
            }
        ],
        "configs": { "filesize": 23061665, "sha1": "73dbececeb20f59a96843fdb3b9b4d2560ab8fa6" }
    }"#;

    #[test]
    fn parse_config() {
        let config: AtPackConfig = serde_json::from_str(SAMPLE).unwrap();
        let info = config.info("SkyFactory4").unwrap();
        assert_eq!(info.mc_version, "1.12.2");
        assert_eq!(info.loader, LoaderType::Forge);
        assert_eq!(info.loader_version.as_deref(), Some("14.23.5.2860"));

        assert_eq!(config.mods.len(), 3);
        assert_eq!(config.mods[2].mod_type, AtModType::Unknown);
        assert!(!config.mods[0].client);
        assert_eq!(
            config.mods[1].download_url(),
            "https://download.nodecdn.net/containers/atl/packs/SkyFactory4/files/4.2.4/bedpatch-2.2-1.12.2.jar"
        );
        assert!(matches!(config.mods[1].placement("1.12.2"), Placement::Copy(d) if d == "mods"));
    }
}
