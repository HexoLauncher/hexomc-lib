//! FTB packs installed from remote version manifests, without a local pack format.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    download::{download_batch, DownloadTask},
    error::{HexoError, Result},
    install::{loader::ProgressFn, vanilla::LoaderType},
    mods::curseforge::{CfMod, CfModFile, CurseForgeClient},
};

use super::{
    install_pack_loader, instance_game_dir, safe_join, ManualDownload, ModpackInfo,
    ModpackInstallResult,
};

const API_BASE: &str = "https://api.feed-the-beast.com/v1/modpacks/public";

/// Pack metadata and available versions from the FTB API.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FtbPack {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub versions: Vec<FtbVersion>,
}

/// One FTB pack version, including archived versions.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FtbVersion {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub updated: i64,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub targets: Vec<FtbTarget>,
}

/// A game, modloader, or runtime required by a pack version.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FtbTarget {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub r#type: String,
}

/// Installation manifest. `name` is the version name, not the pack name.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FtbVersionManifest {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub targets: Vec<FtbTarget>,
    #[serde(default)]
    pub files: Vec<FtbFile>,
}

/// A file placed in `path`, a directory relative to the instance's `.minecraft`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FtbFile {
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub clientonly: bool,
    #[serde(default)]
    pub serveronly: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub curseforge: Option<FtbCurseForgeRef>,
}

/// CurseForge IDs used when a file has no direct download URL.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FtbCurseForgeRef {
    #[serde(deserialize_with = "deserialize_id")]
    pub project: u64,
    #[serde(deserialize_with = "deserialize_id")]
    pub file: u64,
}

/// Older FTB manifests encode CurseForge IDs as decimal strings.
fn deserialize_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Number(u64),
        Text(String),
    }
    match Id::deserialize(deserializer)? {
        Id::Number(id) => Ok(id),
        Id::Text(id) => id.parse().map_err(serde::de::Error::custom),
    }
}

impl FtbVersionManifest {
    /// Describe the Minecraft version and loader, using the separate pack name.
    pub fn info(&self, pack_name: &str) -> Result<ModpackInfo> {
        let mc = self
            .targets
            .iter()
            .find(|t| t.name == "minecraft")
            .filter(|t| !t.version.is_empty())
            .ok_or_else(|| {
                HexoError::Other("FTB manifest has no Minecraft target version".into())
            })?;
        if self.targets.iter().any(|t| t.name == "quilt") {
            return Err(HexoError::UnsupportedLoader("quilt".into()));
        }
        let mut loader = LoaderType::Vanilla;
        let mut loader_version = None;
        for (name, kind) in [
            ("neoforge", LoaderType::NeoForge),
            ("forge", LoaderType::Forge),
            ("fabric", LoaderType::Fabric),
        ] {
            if let Some(target) = self.targets.iter().find(|t| t.name == name) {
                let version = if kind == LoaderType::Forge {
                    target
                        .version
                        .strip_prefix(&format!("{}-", mc.version))
                        .unwrap_or(&target.version)
                } else {
                    &target.version
                };
                if version.is_empty() {
                    return Err(HexoError::Other(format!(
                        "FTB {name} target has no version"
                    )));
                }
                loader = kind;
                loader_version = Some(version.to_string());
                break;
            }
        }
        ModpackInfo {
            name: pack_name.to_string(),
            version: Some(self.name.clone()),
            mc_version: mc.version.clone(),
            loader,
            loader_version,
        }.validated()
    }

    fn selected_files(&self, include_optional: bool) -> impl Iterator<Item = &FtbFile> {
        self.files
            .iter()
            .filter(move |f| !f.serveronly && (!f.optional || include_optional))
    }
}

fn parse_response<T: DeserializeOwned>(value: serde_json::Value, id: &str) -> Result<T> {
    if value.get("status").and_then(|s| s.as_str()) != Some("success") {
        return Err(HexoError::VersionNotFound(format!("FTB {id}")));
    }
    Ok(serde_json::from_value(value)?)
}

async fn fetch<T: DeserializeOwned>(id: &str) -> Result<T> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("hexomc-lib/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client
        .get(format!("{API_BASE}/modpack/{id}"))
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(HexoError::VersionNotFound(format!("FTB {id}")));
    }
    let http_error = response.error_for_status_ref().err();
    let value = response.json::<serde_json::Value>().await;
    if let Ok(ref value) = value {
        if value.get("status").and_then(|s| s.as_str()) != Some("success") {
            return Err(HexoError::VersionNotFound(format!("FTB {id}")));
        }
    }
    if let Some(error) = http_error {
        return Err(error.into());
    }
    parse_response(value?, id)
}

/// Fetch pack metadata, including its name and available versions.
pub async fn get_ftb_pack(pack_id: u64) -> Result<FtbPack> {
    fetch(&pack_id.to_string()).await
}

/// Fetch the files and targets for one pack version without installing it.
pub async fn fetch_ftb_version_manifest(
    pack_id: u64,
    version_id: u64,
) -> Result<FtbVersionManifest> {
    fetch(&format!("{pack_id}/{version_id}")).await
}

fn download_task(
    file: &FtbFile,
    url: &str,
    dest: &Path,
    fallback_sha1: Option<&str>,
) -> DownloadTask {
    let mut task = DownloadTask::new(url, dest);
    if !file.sha1.is_empty() {
        task = task.with_sha1(&file.sha1);
    } else if let Some(sha1) = fallback_sha1 {
        task = task.with_sha1(sha1);
    }
    task
}

fn resolve_files(
    wanted: Vec<(&FtbFile, PathBuf)>,
    files: &[CfModFile],
    projects: &[CfMod],
    tasks: &mut Vec<DownloadTask>,
    manual_downloads: &mut Vec<ManualDownload>,
) {
    let files: HashMap<_, _> = files.iter().map(|f| (f.id, f)).collect();
    let projects: HashMap<_, _> = projects.iter().map(|p| (p.id, p)).collect();
    for (entry, dest) in wanted {
        let Some(reference) = &entry.curseforge else {
            continue;
        };
        let file = files
            .get(&reference.file)
            .filter(|f| f.mod_id == reference.project);
        let project = projects.get(&reference.project);
        if let Some(url) = file
            .and_then(|f| f.download_url.as_deref())
            .filter(|u| !u.is_empty())
        {
            tasks.push(download_task(
                entry,
                url,
                &dest,
                file.and_then(|f| f.sha1()),
            ));
        } else {
            manual_downloads.push(ManualDownload {
                name: project.map_or_else(|| entry.name.clone(), |p| p.name.clone()),
                file_name: entry.name.clone(),
                website: project
                    .and_then(|p| p.links.as_ref())
                    .and_then(|l| l.website_url.as_ref())
                    .map(|url| format!("{}/files/{}", url.trim_end_matches('/'), reference.file)),
                dest,
            });
        }
    }
}

/// Install an FTB pack version into `instance/{instance_name}`.
///
/// `curseforge` resolves files hosted on CurseForge; `None` returns them as manual
/// downloads. `include_optional` also installs optional files. Server-only files
/// are always excluded. Entries with neither a URL nor CurseForge IDs are skipped.
#[allow(clippy::too_many_arguments)]
pub async fn install_ftb_pack(
    pack_id: u64,
    version_id: u64,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    curseforge: Option<&CurseForgeClient>,
    include_optional: bool,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let pack = get_ftb_pack(pack_id).await?;
    let manifest = fetch_ftb_version_manifest(pack_id, version_id).await?;
    let info = manifest.info(&pack.name)?;
    install_pack_loader(&info, instance_name, base_dir, java_path, progress.clone()).await?;

    install_manifest_files(&manifest, &pack.name, instance_name, base_dir, curseforge, include_optional, progress).await
}

/// Download pack contents without installing Minecraft or its loader.
/// `curseforge` and `include_optional` follow the same rules as [`install_ftb_pack`].
/// No Java is required and `instance_config.json` is not created. Use the returned
/// [`ModpackInfo`] to install Minecraft and the loader before the first launch.
pub async fn install_ftb_pack_files(
    pack_id: u64,
    version_id: u64,
    instance_name: &str,
    base_dir: &Path,
    curseforge: Option<&CurseForgeClient>,
    include_optional: bool,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let pack = get_ftb_pack(pack_id).await?;
    let manifest = fetch_ftb_version_manifest(pack_id, version_id).await?;
    install_manifest_files(&manifest, &pack.name, instance_name, base_dir, curseforge, include_optional, progress).await
}

async fn install_manifest_files(
    manifest: &FtbVersionManifest,
    pack_name: &str,
    instance_name: &str,
    base_dir: &Path,
    curseforge: Option<&CurseForgeClient>,
    include_optional: bool,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let info = manifest.info(pack_name)?;

    let game_dir = instance_game_dir(base_dir, instance_name);
    tokio::fs::create_dir_all(&game_dir).await?;
    let mut tasks = Vec::new();
    let mut wanted = Vec::new();
    let mut manual_downloads = Vec::new();
    let mut skipped = Vec::new();
    for file in manifest.selected_files(include_optional) {
        let dest = safe_join(&safe_join(&game_dir, &file.path)?, &file.name)?;
        if !file.url.is_empty() {
            tasks.push(download_task(file, &file.url, &dest, None));
        } else if file.curseforge.is_some() {
            wanted.push((file, dest));
        } else {
            skipped.push(file.name.clone());
        }
    }

    if !wanted.is_empty() {
        let (files, projects) = if let Some(client) = curseforge {
            progress(0, 0, "Resolving CurseForge files");
            let references: Vec<_> = wanted
                .iter()
                .filter_map(|(f, _)| f.curseforge.as_ref())
                .collect();
            let mut file_ids: Vec<_> = references.iter().map(|r| r.file).collect();
            let mut project_ids: Vec<_> = references.iter().map(|r| r.project).collect();
            file_ids.sort_unstable();
            file_ids.dedup();
            project_ids.sort_unstable();
            project_ids.dedup();
            (
                client.get_files(&file_ids).await?,
                client.get_mods(&project_ids).await?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        resolve_files(wanted, &files, &projects, &mut tasks, &mut manual_downloads);
    }
    download_batch(tasks, 8, move |d, t| {
        progress(d, t, "Downloading pack files")
    })
    .await?;
    Ok(ModpackInstallResult {
        info,
        manual_downloads,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SAMPLE: &str = r#"{
        "status": "success", "id": 100422, "name": "1.0.1",
        "type": "archived", "parent": 134,
        "targets": [
            {"name": "minecraft", "version": "1.21.1", "type": "game"},
            {"name": "neoforge", "version": "21.1.235", "type": "modloader"},
            {"name": "java", "version": "21.0.10+7-LTS", "type": "runtime"}
        ],
        "files": [{
            "id": 3483569778, "path": "./config", "name": "dummmmmmy-common.toml",
            "url": "https://files.feed-the-beast.com/blob/57/example.toml", "mirrors": [],
            "sha1": "208500c1984ff314dde2e5ef1537c648e9e935af",
            "hashes": {"sha1": "208500c1984ff314dde2e5ef1537c648e9e935af", "murmur": 2484968544},
            "size": 1323, "clientonly": false, "serveronly": false, "optional": false,
            "type": "config", "updated": 1784229181
        }]
    }"#;

    fn manifest() -> FtbVersionManifest {
        serde_json::from_str(SAMPLE).unwrap()
    }

    #[tokio::test]
    async fn files_only_creates_game_dir_without_instance_config() {
        let mut manifest = manifest();
        manifest.files.clear();
        let temp = tempfile::tempdir().unwrap();
        let result = install_manifest_files(&manifest, "Test", "test", temp.path(), None, false, crate::no_progress()).await.unwrap();
        assert_eq!(result.info.loader, LoaderType::NeoForge);
        assert!(temp.path().join("instance/test/.minecraft").is_dir());
        assert!(!temp.path().join("instance/test/instance_config.json").exists());
        assert!(!temp.path().join("libraries").exists());
        assert!(!temp.path().join("assets").exists());
    }

    #[test]
    fn parse_manifest_and_pack() {
        let manifest = manifest();
        let info = manifest.info("FTB Skies 2: Aero").unwrap();
        assert_eq!(info.name, "FTB Skies 2: Aero");
        assert_eq!(info.version.as_deref(), Some("1.0.1"));
        assert_eq!(info.mc_version, "1.21.1");
        assert_eq!(info.loader, LoaderType::NeoForge);
        assert_eq!(info.loader_version.as_deref(), Some("21.1.235"));
        let file = &manifest.files[0];
        let root = Path::new("game");
        let dest = safe_join(&safe_join(root, &file.path).unwrap(), &file.name).unwrap();
        assert_eq!(dest, root.join("config/dummmmmmy-common.toml"));
        let task = download_task(file, &file.url, &dest, None);
        assert_eq!(task.sha1.as_deref(), Some(file.sha1.as_str()));
        assert_eq!(task.path, dest);
        for (path, name) in [
            ("../outside", "a.jar"),
            ("./mods", "../../a.jar"),
            ("/outside", "a.jar"),
            ("./mods", "/a.jar"),
        ] {
            assert!(safe_join(root, path)
                .and_then(|dir| safe_join(&dir, name))
                .is_err());
        }
        let pack: FtbPack = parse_response(
            json!({
                "status": "success", "id": 134, "name": "FTB Skies 2: Aero",
                "versions": [{"id": 100422, "name": "1.0.1", "type": "archived"}]
            }),
            "134",
        )
        .unwrap();
        assert!(!pack.private);
        assert_eq!(pack.versions[0].r#type, "archived");
        assert!(pack.versions[0].targets.is_empty());
    }

    #[test]
    fn filters_server_and_optional_files() {
        let mut manifest = manifest();
        manifest.files = (0..8)
            .map(|flags| {
                let mut file = manifest.files[0].clone();
                file.serveronly = flags & 1 != 0;
                file.optional = flags & 2 != 0;
                file.clientonly = flags & 4 != 0;
                file
            })
            .collect();
        assert_eq!(manifest.selected_files(false).count(), 2);
        assert_eq!(manifest.selected_files(true).count(), 4);
        assert!(manifest.selected_files(true).all(|f| !f.serveronly));
        assert!(manifest.selected_files(false).any(|f| f.clientonly));
    }

    #[test]
    fn loader_selection_and_forge_prefix() {
        let mut manifest = manifest();
        manifest.targets.truncate(1);
        assert_eq!(manifest.info("Test").unwrap().loader, LoaderType::Vanilla);
        assert!(manifest.info("Test").unwrap().loader_version.is_none());
        for (name, version, expected, bare) in [
            ("fabric", "0.16.5", LoaderType::Fabric, "0.16.5"),
            ("forge", "1.21.1-52.0.1", LoaderType::Forge, "52.0.1"),
            ("neoforge", "21.1.235", LoaderType::NeoForge, "21.1.235"),
        ] {
            manifest.targets.insert(
                1,
                FtbTarget {
                    name: name.into(),
                    version: version.into(),
                    r#type: "modloader".into(),
                },
            );
            let info = manifest.info("Test").unwrap();
            assert_eq!(info.loader, expected);
            assert_eq!(info.loader_version.as_deref(), Some(bare));
        }
        manifest.targets.push(FtbTarget {
            name: "quilt".into(),
            version: "0.26.0".into(),
            r#type: "modloader".into(),
        });
        assert!(matches!(
            manifest.info("Test"),
            Err(HexoError::UnsupportedLoader(_))
        ));
        manifest.targets.clear();
        assert!(manifest.info("Test").is_err());
    }

    #[tokio::test]
    async fn neoforge_1_20_1_is_rejected_before_installation() {
        let mut manifest = manifest();
        manifest.targets[0].version = "1.20.1".into();
        let temp = tempfile::tempdir().unwrap();
        let result = install_manifest_files(&manifest, "Test", "test", temp.path(), None, false, crate::no_progress()).await;
        assert!(
            matches!(result, Err(HexoError::UnsupportedLoader(message)) if message.contains("NeoForge for 1.20.1"))
        );
        assert!(!temp.path().join("instance").exists());
    }

    #[test]
    fn error_status_does_not_require_manifest_fields() {
        for value in [json!({"status": "error"}), json!({})] {
            assert!(matches!(
                parse_response::<FtbVersionManifest>(value, "134/0"),
                Err(HexoError::VersionNotFound(_))
            ));
        }
    }

    #[test]
    fn curseforge_ids_accept_legacy_strings_and_numbers() {
        for ids in [
            json!({"project": "273430", "file": "2459131"}),
            json!({"project": 273430, "file": 2459131}),
        ] {
            let ids: FtbCurseForgeRef = serde_json::from_value(ids).unwrap();
            assert_eq!(ids.project, 273430);
            assert_eq!(ids.file, 2459131);
        }
        assert!(
            serde_json::from_value::<FtbCurseForgeRef>(json!({"project": -1, "file": 2})).is_err()
        );
    }

    #[test]
    fn curseforge_resolution_keeps_paths_and_missing_files() {
        let entry: FtbFile = serde_json::from_value(json!({
            "path": "./custom", "name": "manifest-name.jar",
            "curseforge": {"project": 123, "file": 456}
        }))
        .unwrap();
        let dest = PathBuf::from("game/custom/manifest-name.jar");
        let mut tasks = Vec::new();
        let mut manual = Vec::new();
        resolve_files(
            vec![(&entry, dest.clone())],
            &[],
            &[],
            &mut tasks,
            &mut manual,
        );
        assert!(tasks.is_empty());
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0].dest, dest);
        assert!(manual[0].website.is_none());

        let project: CfMod = serde_json::from_value(json!({
            "id": 123, "name": "Example", "slug": "example", "summary": "", "downloadCount": 0,
            "links": {"websiteUrl": "https://www.curseforge.com/minecraft/mc-mods/example"}
        }))
        .unwrap();
        let mut file: CfModFile = serde_json::from_value(json!({
            "id": 456, "modId": 123, "displayName": "Example", "fileName": "api-name.jar",
            "downloadUrl": null, "gameVersions": [], "fileDate": "",
            "hashes": [{"algo": 1, "value": "abc"}]
        }))
        .unwrap();
        for url in [
            None,
            Some("".to_string()),
            Some("https://example.com/mod.jar".to_string()),
        ] {
            file.download_url = url;
            tasks.clear();
            manual.clear();
            resolve_files(
                vec![(&entry, dest.clone())],
                std::slice::from_ref(&file),
                std::slice::from_ref(&project),
                &mut tasks,
                &mut manual,
            );
            if file.download_url.as_ref().is_some_and(|u| !u.is_empty()) {
                assert!(manual.is_empty());
                assert_eq!(tasks[0].path, dest);
                assert_eq!(tasks[0].sha1.as_deref(), Some("abc"));
            } else {
                assert!(tasks.is_empty());
                assert_eq!(manual[0].file_name, entry.name);
                assert_eq!(
                    manual[0].website.as_deref(),
                    Some("https://www.curseforge.com/minecraft/mc-mods/example/files/456")
                );
            }
        }
    }
}
