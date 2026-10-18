use std::path::{Path, PathBuf};
use tokio::fs;
use serde::{Deserialize, Serialize};

use crate::{
    download::{download_batch, download_file, DownloadTask},
    error::{HexoError, Result},
    version::manifest::{
        fetch_asset_index, fetch_version_manifest, fetch_version_json,
        check_library_rule, check_jvm_rule,
        Argument, ArgumentValue, AssetIndexData, VersionJson,
    },
};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LibEntry {
    pub name: String,
    pub path: PathBuf,
    pub sha1: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct NativeEntry {
    pub name: String,
    pub path: PathBuf,
    pub sha1: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LoaderType {
    Vanilla,
    Fabric,
    Forge,
    NeoForge,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct InstanceConfig {
    pub version_id: String,
    pub loader_type: LoaderType,
    pub main_class: String,
    /// JVM + game launch args (with `${placeholder}` tokens).
    pub start_args: Vec<String>,
    pub lib_list: Vec<LibEntry>,
    pub natives: Vec<NativeEntry>,
    pub assets_id: String,
    pub java_version: u32,
}

impl InstanceConfig {
    pub fn save_path(instance_dir: &Path) -> PathBuf {
        instance_dir.join("instance_config.json")
    }

    pub async fn save(&self, instance_dir: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(Self::save_path(instance_dir), json).await?;
        Ok(())
    }

    pub async fn load(instance_dir: &Path) -> Result<Self> {
        let data = fs::read_to_string(Self::save_path(instance_dir)).await?;
        let config: Self = serde_json::from_str(&data)?;
        Ok(config)
    }
}

/// Install vanilla Minecraft.
///
/// `version_id`: e.g. `"1.21.4"`
/// `instance_name`: instance directory name (chosen by the caller to avoid collisions)
/// `base_dir`: launcher root (holds assets/ libraries/ instance/)
/// `progress`: callback (done, total, description)
pub async fn install_vanilla<F>(
    version_id: &str,
    instance_name: &str,
    base_dir: &Path,
    progress: F,
) -> Result<InstanceConfig>
where
    F: Fn(usize, usize, &str) + Send + Sync + Clone + 'static,
{
    let manifest = fetch_version_manifest().await?;
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == version_id)
        .ok_or_else(|| HexoError::VersionNotFound(version_id.to_string()))?;

    progress(0, 4, "fetching version info");
    let version_json = fetch_version_json(&entry.url).await?;

    let instance_dir = base_dir.join("instance").join(instance_name);
    let lib_dir = base_dir.join("libraries");
    let assets_dir = base_dir.join("assets");

    init_instance_dirs(&instance_dir, &assets_dir, &lib_dir).await?;

    progress(1, 4, "downloading libraries");
    let (lib_list, natives) = download_libraries(&version_json, &lib_dir, progress.clone()).await?;

    progress(2, 4, "downloading assets");
    let asset_data = download_assets(&version_json, &assets_dir, progress.clone()).await?;
    materialize_legacy_assets(
        &asset_data,
        &assets_dir,
        &instance_dir.join(".minecraft"),
        &version_json.asset_index.id,
    )
    .await?;

    progress(3, 4, "downloading client jar");
    let client_jar = instance_dir.join(format!("{}.jar", version_id));
    download_file(
        &DownloadTask::new(
            version_json.downloads.client.url.clone(),
            &client_jar,
        )
        .with_sha1(version_json.downloads.client.sha1.clone()),
    )
    .await?;

    progress(4, 4, "resolving launch arguments");
    let start_args = parse_start_args(&version_json);

    let config = InstanceConfig {
        version_id: version_id.to_string(),
        loader_type: LoaderType::Vanilla,
        main_class: version_json.main_class.clone(),
        start_args,
        lib_list,
        natives,
        assets_id: version_json.asset_index.id.clone(),
        java_version: version_json.java_version.major_version,
    };

    config.save(&instance_dir).await?;

    Ok(config)
}

async fn init_instance_dirs(
    instance_dir: &Path,
    assets_dir: &Path,
    lib_dir: &Path,
) -> Result<()> {
    let minecraft_dir = instance_dir.join(".minecraft");
    for dir in &[
        instance_dir.to_path_buf(),
        minecraft_dir.join("mods"),
        minecraft_dir.join("config"),
        minecraft_dir.join("resourcepacks"),
        minecraft_dir.join("saves"),
        minecraft_dir.join("screenshots"),
        minecraft_dir.join("shaderpacks"),
        minecraft_dir.join("logs"),
        assets_dir.join("objects"),
        assets_dir.join("indexes"),
        lib_dir.to_path_buf(),
    ] {
        fs::create_dir_all(dir).await?;
    }
    Ok(())
}

async fn download_libraries(
    version_json: &VersionJson,
    lib_dir: &Path,
    progress: impl Fn(usize, usize, &str) + Send + Sync + Clone + 'static,
) -> Result<(Vec<LibEntry>, Vec<NativeEntry>)> {
    let mut lib_tasks: Vec<(DownloadTask, LibEntry)> = Vec::new();
    let mut native_tasks: Vec<(DownloadTask, NativeEntry)> = Vec::new();

    for lib in &version_json.libraries {
        if let Some(rules) = &lib.rules {
            if !check_library_rule(rules) {
                continue;
            }
        }

        if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let path = lib_dir.join(&artifact.path);
                lib_tasks.push((
                    DownloadTask::new(&artifact.url, &path)
                        .with_sha1(artifact.sha1.clone()),
                    LibEntry {
                        name: lib.name.clone(),
                        path: path.clone(),
                        sha1: artifact.sha1.clone(),
                    },
                ));
            }

            if let Some(natives_map) = &lib.natives {
                let native_key = native_key_for_platform();
                if let Some(classifier_key) = natives_map.get(native_key) {
                    if let Some(classifiers) = &downloads.classifiers {
                        if let Some(native_artifact) = classifiers.get(classifier_key) {
                            let path = lib_dir.join(&native_artifact.path);
                            native_tasks.push((
                                DownloadTask::new(&native_artifact.url, &path)
                                    .with_sha1(native_artifact.sha1.clone()),
                                NativeEntry {
                                    name: lib.name.clone(),
                                    path: path.clone(),
                                    sha1: native_artifact.sha1.clone(),
                                },
                            ));
                        }
                    }
                }
            }
        }
    }

    let total = lib_tasks.len() + native_tasks.len();
    let p = progress.clone();

    let lib_download_tasks: Vec<DownloadTask> =
        lib_tasks.iter().map(|(t, _)| t.clone()).collect();
    let native_download_tasks: Vec<DownloadTask> =
        native_tasks.iter().map(|(t, _)| t.clone()).collect();

    download_batch(lib_download_tasks, 16, move |done, _| {
        p(done, total, "downloading libraries");
    })
    .await?;

    download_batch(native_download_tasks, 8, move |done, _| {
        progress(done, total, "downloading native libraries");
    })
    .await?;

    Ok((
        lib_tasks.into_iter().map(|(_, e)| e).collect(),
        native_tasks.into_iter().map(|(_, e)| e).collect(),
    ))
}

async fn download_assets(
    version_json: &VersionJson,
    assets_dir: &Path,
    progress: impl Fn(usize, usize, &str) + Send + Sync + Clone + 'static,
) -> Result<AssetIndexData> {
    let index = &version_json.asset_index;

    let index_path = assets_dir
        .join("indexes")
        .join(format!("{}.json", index.id));

    let asset_data = fetch_asset_index(&index.url).await?;
    let json = serde_json::to_string_pretty(&asset_data)?;
    fs::write(&index_path, json).await?;

    let tasks: Vec<DownloadTask> = asset_data
        .objects
        .values()
        .map(|obj| DownloadTask::asset(&obj.hash, assets_dir))
        .collect();

    let total = tasks.len();
    download_batch(tasks, 32, move |done, _| {
        progress(done, total, "downloading assets");
    })
    .await?;
    Ok(asset_data)
}

async fn materialize_legacy_assets(
    index: &AssetIndexData,
    assets_dir: &Path,
    minecraft_dir: &Path,
    asset_id: &str,
) -> Result<()> {
    let mut roots = Vec::new();
    if index.virtual_assets {
        roots.push(assets_dir.join("virtual").join(asset_id));
    }
    if index.map_to_resources {
        roots.push(minecraft_dir.join("resources"));
    }
    if roots.is_empty() {
        return Ok(());
    }
    for (name, object) in &index.objects {
        if Path::new(name)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(HexoError::Other(format!("Unsafe asset path: {name}")));
        }
        let prefix = object.hash.get(..2)
            .ok_or_else(|| HexoError::Other(format!("Invalid asset hash: {}", object.hash)))?;
        let source = assets_dir.join("objects").join(prefix).join(&object.hash);
        for root in &roots {
            let target = root.join(name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).await?;
            }
            fs::copy(&source, &target).await?;
        }
    }
    Ok(())
}

/// Parse JVM + game args, expanding conditional args by rule.
pub fn parse_start_args(version_json: &VersionJson) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    if let Some(arguments) = &version_json.arguments {
        for arg in &arguments.jvm {
            collect_arg(arg, &mut args, true);
        }
        // Main class goes between JVM args and game args.
        args.push("${mainClass}".to_string());
        for arg in &arguments.game {
            collect_arg(arg, &mut args, false);
        }
    } else if let Some(mc_args) = &version_json.minecraft_arguments {
        // Old format: a single whitespace-separated string.
        args.extend(mc_args.split_whitespace().map(|s| s.to_string()));
    }

    args
}

fn collect_arg(arg: &Argument, out: &mut Vec<String>, is_jvm: bool) {
    match arg {
        Argument::Simple(s) => out.push(s.clone()),
        Argument::Conditional { rules, value } => {
            let allowed = if is_jvm {
                check_jvm_rule(rules)
            } else {
                check_library_rule(rules)
            };
            if allowed {
                match value {
                    ArgumentValue::Single(s) => out.push(s.clone()),
                    ArgumentValue::Multiple(vs) => out.extend(vs.iter().cloned()),
                }
            }
        }
    }
}

fn native_key_for_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn legacy_assets_are_copied_to_named_paths_and_resources() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        for (virtual_assets, resources) in [(true, false), (false, true), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let assets = root.path().join("assets");
            let minecraft = root.path().join("instance/.minecraft");
            init_instance_dirs(
                &root.path().join("instance"),
                &assets,
                &root.path().join("libraries"),
            )
            .await.unwrap();
            assert!(minecraft.join("config").is_dir());
            fs::create_dir_all(assets.join("objects/01")).await.unwrap();
            fs::write(assets.join("objects/01").join(hash), b"asset bytes").await.unwrap();
            let index = serde_json::from_value(json!({
                "virtual": virtual_assets,
                "map_to_resources": resources,
                "objects": {"sound/test.ogg": {"hash": hash, "size": 11}}
            })).unwrap();
            materialize_legacy_assets(&index, &assets, &minecraft, "legacy").await.unwrap();
            assert_eq!(assets.join("virtual/legacy").exists(), virtual_assets);
            assert_eq!(minecraft.join("resources").exists(), resources);
            if virtual_assets {
                assert_eq!(fs::read(assets.join("virtual/legacy/sound/test.ogg")).await.unwrap(), b"asset bytes");
            }
            if resources {
                assert_eq!(fs::read(minecraft.join("resources/sound/test.ogg")).await.unwrap(), b"asset bytes");
            }
        }
    }

    #[tokio::test]
    async fn legacy_asset_paths_cannot_escape_the_mapping_directory() {
        let root = tempfile::tempdir().unwrap();
        let index = serde_json::from_value(json!({
            "virtual": true,
            "objects": {"../escape": {"hash": "0123456789abcdef", "size": 1}}
        })).unwrap();
        assert!(materialize_legacy_assets(&index, root.path(), root.path(), "legacy")
            .await.is_err());
    }

    #[tokio::test]
    async fn modern_asset_indexes_need_no_named_mapping() {
        let root = tempfile::tempdir().unwrap();
        let index = serde_json::from_value(json!({
            "objects": {"sound/test.ogg": {"hash": "0123456789abcdef", "size": 1}}
        })).unwrap();
        materialize_legacy_assets(&index, root.path(), root.path(), "modern").await.unwrap();
        assert!(!root.path().join("virtual").exists());
        assert!(!root.path().join("resources").exists());
    }
}
