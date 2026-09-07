use std::path::{Path, PathBuf};
use tokio::fs;
use serde::{Deserialize, Serialize};

use crate::{
    download::{download_batch, download_file, DownloadTask},
    error::{HexoError, Result},
    version::manifest::{
        fetch_asset_index, fetch_version_manifest, fetch_version_json,
        check_library_rule, check_jvm_rule,
        Argument, ArgumentValue, VersionJson,
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

    progress(0, 4, "取得版本資訊");
    let version_json = fetch_version_json(&entry.url).await?;

    let instance_dir = base_dir.join("instance").join(instance_name);
    let lib_dir = base_dir.join("libraries");
    let assets_dir = base_dir.join("assets");

    init_instance_dirs(&instance_dir, &assets_dir, &lib_dir).await?;

    progress(1, 4, "下載 libraries");
    let (lib_list, natives) = download_libraries(&version_json, &lib_dir, progress.clone()).await?;

    progress(2, 4, "下載 assets");
    download_assets(&version_json, &assets_dir, progress.clone()).await?;

    progress(3, 4, "下載 client jar");
    let client_jar = instance_dir.join(format!("{}.jar", version_id));
    download_file(
        &DownloadTask::new(
            version_json.downloads.client.url.clone(),
            &client_jar,
        )
        .with_sha1(version_json.downloads.client.sha1.clone()),
    )
    .await?;

    progress(4, 4, "解析啟動參數");
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
        p(done, total, "下載 libraries");
    })
    .await?;

    download_batch(native_download_tasks, 8, move |done, _| {
        progress(done, total, "下載 native libraries");
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
) -> Result<()> {
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
        progress(done, total, "下載 assets");
    })
    .await
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
