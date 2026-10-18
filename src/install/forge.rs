use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs;

use crate::{
    download::{download_batch, download_file, DownloadTask},
    error::{HexoError, Result},
    install::vanilla::{native_key_for_platform, InstanceConfig, LibEntry, LoaderType, NativeEntry},
    version::manifest::{check_library_rule, Rule},
};

const FORGE_META_URL: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/maven-metadata.json";
const FORGE_MAVEN_BASE: &str =
    "https://maven.minecraftforge.net/net/minecraftforge/forge";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ForgeInstallProfile {
    pub libraries: Vec<ForgeLibrary>,
    pub processors: Vec<ForgeProcessor>,
    pub data: HashMap<String, ForgeDataEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ForgeLibrary {
    pub name: String,
    pub downloads: ForgeLibraryDownloads,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ForgeLibraryDownloads {
    pub artifact: ForgeArtifact,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ForgeArtifact {
    pub path: String,
    pub sha1: String,
    pub url: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ForgeProcessor {
    pub jar: String,
    pub classpath: Vec<String>,
    pub args: Vec<String>,
    pub sides: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ForgeDataEntry {
    pub client: String,
    pub server: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ForgeVersionJson {
    #[serde(rename = "mainClass")]
    pub main_class: String,
    pub libraries: Vec<ForgeLibrary>,
    pub arguments: ForgeVersionArgs,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ForgeVersionArgs {
    pub jvm: Option<Vec<String>>,
    pub game: Option<Vec<String>>,
}

/// Old installer `install_profile.json` (pre-1.13): no processors,
/// made of an `install` and a `versionInfo` block.
#[derive(Debug, Deserialize)]
pub struct LegacyInstallProfile {
    pub install: LegacyInstall,
    #[serde(rename = "versionInfo")]
    pub version_info: LegacyVersionInfo,
}

#[derive(Debug, Deserialize)]
pub struct LegacyInstall {
    /// Maven name of the forge universal jar.
    pub path: String,
    /// Universal jar's filename inside the installer zip.
    #[serde(rename = "filePath")]
    pub file_path: String,
}

#[derive(Debug, Deserialize)]
pub struct LegacyVersionInfo {
    #[serde(rename = "mainClass")]
    pub main_class: String,
    #[serde(rename = "minecraftArguments")]
    pub minecraft_arguments: String,
    pub libraries: Vec<LegacyLibrary>,
}

#[derive(Debug, Deserialize)]
pub struct LegacyLibrary {
    pub name: String,
    /// Download from this maven if present, otherwise from Mojang libraries.
    #[serde(default)]
    pub url: Option<String>,
    /// Whether the client needs it (None = required).
    #[serde(default)]
    pub clientreq: Option<bool>,
    #[serde(default)]
    pub rules: Option<Vec<Rule>>,
    #[serde(default)]
    pub natives: Option<HashMap<String, String>>,
}

/// Available Forge versions for the given MC version.
pub async fn get_forge_versions(mc_version: &str) -> Result<Vec<String>> {
    let client = reqwest::Client::new();
    let data: HashMap<String, Vec<String>> =
        client.get(FORGE_META_URL).send().await?.json().await?;

    Ok(data.get(mc_version).cloned().unwrap_or_default())
}

/// Install Forge. `forge_version` = None uses the newest version.
pub async fn install_forge(
    mc_version: &str,
    forge_version: Option<&str>,
    instance_name: &str,
    java_path: &Path,
    base_dir: &Path,
) -> Result<()> {
    let versions = get_forge_versions(mc_version).await?;
    let forge_ver = if let Some(v) = forge_version {
        v.to_string()
    } else {
        // List is ordered oldest -> newest.
        versions
            .into_iter()
            .last()
            .ok_or_else(|| HexoError::VersionNotFound(format!("Forge for {}", mc_version)))?
    };

    // Version strings already include the mc prefix (e.g. "1.7.10-10.13.4.1614-1.7.10").
    let installer_url = format!(
        "{}/{}/forge-{}-installer.jar",
        FORGE_MAVEN_BASE, forge_ver, forge_ver
    );

    install_forge_like(
        mc_version,
        &forge_ver,
        &installer_url,
        instance_name,
        java_path,
        base_dir,
        LoaderType::Forge,
    )
    .await
}


pub async fn install_forge_like(
    mc_version: &str,
    loader_version: &str,
    installer_url: &str,
    instance_name: &str,
    java_path: &Path,
    base_dir: &Path,
    loader_type: LoaderType,
) -> Result<()> {
    let instance_dir = base_dir.join("instance").join(instance_name);
    let lib_dir = base_dir.join("libraries");
    let temp_dir = base_dir.join("temp").join(loader_version);
    fs::create_dir_all(&temp_dir).await?;

    let installer_path = temp_dir.join("forge-installer.jar");
    download_file(&DownloadTask::new(installer_url, &installer_path)).await?;

    // Detect installer format: pre-1.13 has no processors and uses install + versionInfo.
    let profile_value: serde_json::Value =
        read_json_from_zip(&installer_path, "install_profile.json").await?;
    if profile_value.get("versionInfo").is_some() {
        let legacy: LegacyInstallProfile = serde_json::from_value(profile_value)?;
        let result =
            install_forge_legacy(&installer_path, &legacy, instance_name, base_dir).await;
        let _ = fs::remove_dir_all(&temp_dir).await;
        return result;
    }
    let install_profile: ForgeInstallProfile = serde_json::from_value(profile_value)?;

    let mut lib_tasks: Vec<DownloadTask> = Vec::new();
    for lib in &install_profile.libraries {
        let art = &lib.downloads.artifact;
        if art.url.is_empty() {
            continue;
        }
        let path = lib_dir.join(&art.path);
        lib_tasks.push(DownloadTask::new(&art.url, &path).with_sha1(art.sha1.clone()));
    }
    download_batch(lib_tasks, 8, |_, _| {}).await?;

    let lzma_path = temp_dir.join("client.lzma");
    extract_file_from_zip(&installer_path, "data/client.lzma", &lzma_path).await?;

    let mc_jar = instance_dir.join(format!("{}.jar", mc_version));
    run_processors(
        &install_profile.processors,
        &install_profile.data,
        &installer_path,
        &lzma_path,
        &mc_jar,
        &lib_dir,
        java_path,
        &temp_dir,
    )
    .await?;

    let version_json: ForgeVersionJson =
        read_json_from_zip(&installer_path, "version.json").await?;

    let mut new_lib_entries: Vec<LibEntry> = Vec::new();
    let mut ver_lib_tasks: Vec<DownloadTask> = Vec::new();
    for lib in &version_json.libraries {
        let art = &lib.downloads.artifact;
        if art.url.is_empty() {
            continue;
        }
        let path = lib_dir.join(&art.path);
        ver_lib_tasks.push(DownloadTask::new(&art.url, &path).with_sha1(art.sha1.clone()));
        new_lib_entries.push(LibEntry {
            name: lib.name.clone(),
            path: path.clone(),
            sha1: art.sha1.clone(),
        });
    }
    download_batch(ver_lib_tasks, 8, |_, _| {}).await?;

    let mut config = InstanceConfig::load(&instance_dir).await?;

    // Insert Forge JVM args at the -cp position.
    if let Some(jvm_args) = &version_json.arguments.jvm {
        let cp_pos = config
            .start_args
            .iter()
            .position(|a| a == "-cp")
            .unwrap_or(0);
        for (i, arg) in jvm_args.iter().enumerate() {
            config.start_args.insert(cp_pos + i, arg.clone());
        }
    }

    // Insert Forge game args right after the main class.
    if let Some(game_args) = &version_json.arguments.game {
        if let Some(mc_pos) = config.start_args.iter().position(|a| a == "${mainClass}") {
            for (i, arg) in game_args.iter().enumerate() {
                config.start_args.insert(mc_pos + 1 + i, arg.clone());
            }
        }
    }

    config.main_class = version_json.main_class.clone();
    config.loader_type = loader_type;

    // Prepend the new libs so Forge takes precedence over vanilla.
    new_lib_entries.append(&mut config.lib_list);
    config.lib_list = new_lib_entries;

    config.save(&instance_dir).await?;

    fs::remove_dir_all(&temp_dir).await?;

    Ok(())
}

/// Legacy Forge install (pre-1.13): no processors. Extract the universal jar,
/// download the libraries, then overlay mainClass / args / libs onto the
/// already-installed vanilla instance config.
async fn install_forge_legacy(
    installer_path: &Path,
    profile: &LegacyInstallProfile,
    instance_name: &str,
    base_dir: &Path,
) -> Result<()> {
    let instance_dir = base_dir.join("instance").join(instance_name);
    let lib_dir = base_dir.join("libraries");

    // Extract the forge universal jar from the installer into its maven path.
    let universal_dest = resolve_maven_path(&lib_dir, &profile.install.path);
    if let Some(parent) = universal_dest.parent() {
        fs::create_dir_all(parent).await?;
    }
    extract_file_from_zip(installer_path, &profile.install.file_path, &universal_dest).await?;

    let (lib_tasks, mut new_lib_entries, new_native_entries) = legacy_library_downloads(
        profile,
        &lib_dir,
        native_key_for_platform(),
    );
    download_batch(lib_tasks, 8, |_, _| {}).await?;

    let mut config = InstanceConfig::load(&instance_dir).await?;
    config.main_class = profile.version_info.main_class.clone();
    config.loader_type = LoaderType::Forge;
    // Old game args are a single string; replace vanilla's game args outright.
    config.start_args = profile
        .version_info
        .minecraft_arguments
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    // Prepend new libs (launchwrapper / forge must load before vanilla libs).
    new_lib_entries.append(&mut config.lib_list);
    config.lib_list = new_lib_entries;
    config.natives.extend(new_native_entries);
    config.save(&instance_dir).await?;

    Ok(())
}

fn legacy_library_downloads(
    profile: &LegacyInstallProfile,
    lib_dir: &Path,
    native_os: &str,
) -> (Vec<DownloadTask>, Vec<LibEntry>, Vec<NativeEntry>) {
    let mut tasks = Vec::new();
    let mut libraries = Vec::new();
    let mut natives = Vec::new();
    for lib in &profile.version_info.libraries {
        if lib.clientreq == Some(false)
            || lib.rules.as_ref().is_some_and(|rules| !check_library_rule(rules))
        {
            continue;
        }
        let coordinate = if let Some(classifiers) = &lib.natives {
            let Some(classifier) = classifiers.get(native_os) else {
                continue;
            };
            let arch = if cfg!(target_pointer_width = "64") { "64" } else { "32" };
            format!("{}:{}", lib.name, classifier.replace("${arch}", arch))
        } else {
            lib.name.clone()
        };
        let path = resolve_maven_path(lib_dir, &coordinate);
        if lib.natives.is_some() {
            natives.push(NativeEntry {
                name: lib.name.clone(),
                path: path.clone(),
                sha1: String::new(),
            });
        } else {
            libraries.push(LibEntry {
                name: lib.name.clone(),
                path: path.clone(),
                sha1: String::new(),
            });
            if lib.name == profile.install.path {
                continue;
            }
        }
        let base = lib
            .url
            .as_deref()
            .unwrap_or("https://libraries.minecraft.net/");
        let sep = if base.ends_with('/') { "" } else { "/" };
        let url = format!("{}{}{}", base, sep, maven_relative_path(&coordinate));
        tasks.push(DownloadTask::new(url, path));
    }
    (tasks, libraries, natives)
}

#[allow(clippy::too_many_arguments)]
async fn run_processors(
    processors: &[ForgeProcessor],
    data: &HashMap<String, ForgeDataEntry>,
    installer_path: &Path,
    lzma_path: &Path,
    mc_jar: &Path,
    lib_dir: &Path,
    java_path: &Path,
    temp_dir: &Path,
) -> Result<()> {
    let mut rargs: HashMap<String, String> = HashMap::new();

    for (key, entry) in data {
        let value = &entry.client;
        let resolved = if value.starts_with('[') && value.ends_with(']') {
            path_str(&resolve_maven_path(lib_dir, &value[1..value.len() - 1]))
        } else {
            value.clone()
        };
        rargs.insert(format!("{{{}}}", key), resolved);
    }

    // Insert hardcoded values after the data loop so they aren't overwritten by a
    // same-named key in data (e.g. NeoForge's BINPATCH is "/data/client.lzma" in data).
    rargs.insert("{INSTALLER}".to_string(), path_str(installer_path));
    rargs.insert("{ROOT}".to_string(), path_str(temp_dir));
    rargs.insert("{SIDE}".to_string(), "client".to_string());
    rargs.insert("{MINECRAFT_JAR}".to_string(), path_str(mc_jar));
    rargs.insert("{BINPATCH}".to_string(), path_str(lzma_path));

    for proc in processors {
        if let Some(sides) = &proc.sides {
            if !sides.contains(&"client".to_string()) {
                continue;
            }
        }

        let proc_jar = resolve_maven_path_from_name(lib_dir, &proc.jar)?;
        let main_class = get_jar_main_class(&proc_jar)?;

        let mut classpath = proc
            .classpath
            .iter()
            .map(|n| resolve_maven_path_from_name(lib_dir, n).map(|p| path_str(&p)))
            .collect::<Result<Vec<_>>>()?;
        classpath.push(path_str(&proc_jar));

        let cp_sep = classpath_separator();
        let cp = classpath.join(cp_sep);

        let resolved_args: Vec<String> = proc
            .args
            .iter()
            .map(|arg| {
                if arg.starts_with('[') && arg.ends_with(']') {
                    let name = &arg[1..arg.len() - 1];
                    path_str(&resolve_maven_path(lib_dir, name))
                } else {
                    let mut s = arg.clone();
                    for (k, v) in &rargs {
                        s = s.replace(k.as_str(), v.as_str());
                    }
                    s
                }
            })
            .collect();

        let status = std::process::Command::new(java_path)
            .args(["-cp", &cp, &main_class])
            .args(&resolved_args)
            .status()
            .map_err(|e| HexoError::ProcessorFailed(e.to_string()))?;

        if !status.success() {
            return Err(HexoError::ProcessorFailed(format!(
                "processor {} exited with a non-zero status",
                proc.jar
            )));
        }
    }

    Ok(())
}

async fn read_json_from_zip<T: serde::de::DeserializeOwned + Send + 'static>(
    zip_path: &Path,
    entry_name: &str,
) -> Result<T> {
    let zip_path = zip_path.to_owned();
    let entry_name = entry_name.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&zip_path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let entry = archive.by_name(&entry_name)?;
        let value: T = serde_json::from_reader(entry)?;
        Ok(value)
    })
    .await
    .map_err(|e| HexoError::Other(e.to_string()))?
}

async fn extract_file_from_zip(
    zip_path: &Path,
    entry_name: &str,
    out_path: &Path,
) -> Result<()> {
    let zip_path = zip_path.to_owned();
    let entry_name = entry_name.to_owned();
    let out_path = out_path.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&zip_path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let mut entry = archive.by_name(&entry_name)?;
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out)?;
        Ok(())
    })
    .await
    .map_err(|e| HexoError::Other(e.to_string()))?
}

/// Resolve `group:artifact:version[:classifier][@ext]` to a path under `lib_dir`.
fn resolve_maven_path(lib_dir: &Path, name: &str) -> PathBuf {
    let (base, ext) = if let Some((b, e)) = name.split_once('@') {
        (b, e)
    } else {
        (name, "jar")
    };

    let parts: Vec<&str> = base.split(':').collect();
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).copied().unwrap_or("");

    let file_name = if classifier.is_empty() {
        format!("{}-{}.{}", artifact, version, ext)
    } else {
        format!("{}-{}-{}.{}", artifact, version, classifier, ext)
    };

    lib_dir.join(group).join(artifact).join(version).join(file_name)
}

/// Like `resolve_maven_path` but returns a `/`-separated relative path for URLs.
fn maven_relative_path(name: &str) -> String {
    let (base, ext) = if let Some((b, e)) = name.split_once('@') {
        (b, e)
    } else {
        (name, "jar")
    };

    let parts: Vec<&str> = base.split(':').collect();
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).copied().unwrap_or("");

    let file_name = if classifier.is_empty() {
        format!("{}-{}.{}", artifact, version, ext)
    } else {
        format!("{}-{}-{}.{}", artifact, version, classifier, ext)
    };

    format!("{}/{}/{}/{}", group, artifact, version, file_name)
}

fn resolve_maven_path_from_name(lib_dir: &Path, name: &str) -> Result<PathBuf> {
    let path = resolve_maven_path(lib_dir, name);
    if !path.exists() {
        return Err(HexoError::Other(format!(
            "jar does not exist: {}",
            path.display()
        )));
    }
    Ok(path)
}

fn get_jar_main_class(jar_path: &Path) -> Result<String> {
    let file = std::fs::File::open(jar_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| HexoError::Other(e.to_string()))?;
    let manifest = archive
        .by_name("META-INF/MANIFEST.MF")
        .map_err(|e| HexoError::Other(e.to_string()))?;
    use std::io::Read;
    let mut content = String::new();
    let mut r = manifest;
    r.read_to_string(&mut content)?;
    for line in content.lines() {
        if line.starts_with("Main-Class:") {
            return Ok(line["Main-Class:".len()..].trim().to_string());
        }
    }
    Err(HexoError::Other(format!(
        "no Main-Class found in {}",
        jar_path.display()
    )))
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn classpath_separator() -> &'static str {
    if cfg!(target_os = "windows") { ";" } else { ":" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn legacy_profile(libraries: serde_json::Value) -> LegacyInstallProfile {
        serde_json::from_value(json!({
            "install": {
                "path": "net.minecraftforge:minecraftforge:9.11.1.1345",
                "filePath": "minecraftforge-universal-1.6.4-9.11.1.1345.jar"
            },
            "versionInfo": {
                "mainClass": "net.minecraft.launchwrapper.Launch",
                "minecraftArguments": "--tweakClass cpw.mods.fml.common.launcher.FMLTweaker",
                "libraries": libraries
            }
        })).unwrap()
    }

    #[test]
    fn legacy_native_archives_use_classifiers_and_stay_off_the_classpath() {
        let profile = legacy_profile(json!([
            {"name": "net.minecraftforge:minecraftforge:9.11.1.1345"},
            {"name": "net.minecraft:launchwrapper:1.8"},
            {
                "name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.0",
                "natives": {
                    "linux": "natives-linux",
                    "windows": "natives-windows",
                    "osx": "natives-osx"
                }
            },
            {
                "name": "net.java.jinput:jinput-platform:2.0.5",
                "natives": {"linux": "natives-linux", "windows": "natives-windows"}
            }
        ]));
        for platform in ["linux", "windows", "osx"] {
            let (tasks, libraries, natives) = legacy_library_downloads(
                &profile, Path::new("libraries"), platform,
            );
            assert_eq!(libraries.len(), 2);
            assert_eq!(libraries[0].name, profile.install.path);
            assert_eq!(libraries[1].name, "net.minecraft:launchwrapper:1.8");
            assert_eq!(natives.len(), if platform == "osx" { 1 } else { 2 });
            assert_eq!(tasks.len(), natives.len() + 1);
            let expected = format!(
                "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-{platform}.jar"
            );
            assert!(tasks.iter().any(|task| {
                task.url == format!("https://libraries.minecraft.net/{expected}")
                    && task.path == Path::new("libraries").join(&expected)
            }));
            assert!(tasks.iter().all(|task| !task.url.ends_with("lwjgl-platform-2.9.0.jar")));
        }
    }

    #[test]
    fn legacy_library_rules_and_client_requirements_are_respected() {
        let other_os = if cfg!(target_os = "windows") { "linux" } else { "windows" };
        let profile = legacy_profile(json!([
            {"name": "test:server-only:1", "clientreq": false},
            {"name": "test:other-os:1", "rules": [{"action": "allow", "os": {"name": other_os}}]},
            {"name": "test:disallowed:1", "rules": [{"action": "disallow"}]},
            {"name": "test:no-linux-native:1", "natives": {"windows": "natives-windows"}},
            {"name": "test:required:1", "clientreq": true, "url": "https://maven.minecraftforge.net"}
        ]));
        let (tasks, libraries, natives) = legacy_library_downloads(
            &profile, Path::new("libraries"), "linux",
        );
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].url, "https://maven.minecraftforge.net/test/required/1/required-1.jar");
        assert_eq!(libraries.len(), 1);
        assert!(natives.is_empty());
    }

    #[test]
    fn legacy_native_classifiers_expand_architecture() {
        let profile = legacy_profile(json!([{
            "name": "test:native:1",
            "natives": {"windows": "natives-windows-${arch}"}
        }]));
        let (tasks, libraries, natives) = legacy_library_downloads(
            &profile, Path::new("libraries"), "windows",
        );
        let arch = if cfg!(target_pointer_width = "64") { "64" } else { "32" };
        assert!(tasks[0].url.ends_with(&format!("native-1-natives-windows-{arch}.jar")));
        assert!(libraries.is_empty());
        assert_eq!(natives.len(), 1);
    }

    #[tokio::test]
    #[ignore = "requires network"]
    async fn legacy_1_6_4_installer_download_urls_exist() {
        let root = tempfile::tempdir().unwrap();
        let installer = root.path().join("installer.jar");
        download_file(&DownloadTask::new(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.6.4-9.11.1.1345/forge-1.6.4-9.11.1.1345-installer.jar",
            &installer,
        )).await.unwrap();
        let profile: LegacyInstallProfile = read_json_from_zip(&installer, "install_profile.json")
            .await.unwrap();
        let (tasks, libraries, natives) = legacy_library_downloads(
            &profile, &root.path().join("libraries"), "linux",
        );
        assert_eq!(natives.len(), 2);
        assert!(libraries.iter().all(|library| !library.name.contains("-platform:")));
        let client = reqwest::Client::new();
        for task in tasks {
            let response = client.head(&task.url).send().await.unwrap();
            assert!(response.status().is_success(), "{}: {}", task.url, response.status());
        }
    }
}
