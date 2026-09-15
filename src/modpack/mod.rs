//! Modpack installation: Modrinth (`.mrpack`), CurseForge (`manifest.json` zip) and
//! ATLauncher and FTB (online packs).
//!
//! Every format follows the same flow: parse the pack, install vanilla + the loader via
//! `install_with_loader`, extract overrides/configs into `instance/{name}/.minecraft`,
//! then download the files listed by the pack.
//! The `*_files` entry points only install pack contents, leaving Minecraft and its
//! loader for a later launch. They do not create `instance_config.json`.

pub mod atpack;
pub mod cfpack;
pub mod ftbpack;
pub mod mrpack;

use std::path::{Component, Path, PathBuf};

use crate::{
    error::{HexoError, Result},
    install::{
        forge::get_forge_versions,
        loader::{
            install_with_loader, FabricInstaller, ForgeInstaller, LoaderInstaller,
            NeoForgeInstaller, ProgressFn, VanillaInstaller,
        },
        vanilla::{InstanceConfig, LoaderType},
    },
    java::detector::find_java,
    mods::curseforge::CurseForgeClient,
};

/// Format-independent description of a modpack.
#[derive(Debug, Clone)]
pub struct ModpackInfo {
    pub name: String,
    pub version: Option<String>,
    pub mc_version: String,
    pub loader: LoaderType,
    /// Loader version without the MC prefix (e.g. Forge `47.2.0`, Fabric `0.16.5`).
    /// None uses the latest.
    pub loader_version: Option<String>,
}

impl ModpackInfo {
    /// Normalize Forge versions and reject loaders that cannot be installed later.
    pub(crate) fn validated(mut self) -> Result<Self> {
        if self.loader == LoaderType::NeoForge && self.mc_version == "1.20.1" {
            return Err(HexoError::UnsupportedLoader("NeoForge for 1.20.1".into()));
        }
        if self.loader == LoaderType::Forge {
            if let Some(version) = &mut self.loader_version {
                if let Some(bare) = version.strip_prefix(&format!("{}-", self.mc_version)) {
                    *version = bare.to_string();
                }
            }
        }
        Ok(self)
    }
}

/// Format of a local modpack file. ATLauncher and FTB packs have no file format; use
/// [`atpack::install_atlauncher_pack`] or [`ftbpack::install_ftb_pack`] for those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModpackFormat {
    Modrinth,
    CurseForge,
}

/// A file that could not be downloaded automatically (CurseForge files whose authors
/// disallow third-party distribution, ATLauncher `browser` downloads).
#[derive(Debug, Clone)]
pub struct ManualDownload {
    pub name: String,
    pub file_name: String,
    /// Page the user should download the file from.
    pub website: Option<String>,
    /// Where the downloaded file belongs.
    pub dest: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ModpackInstallResult {
    pub info: ModpackInfo,
    pub manual_downloads: Vec<ManualDownload>,
    /// Names of entries skipped because their type is unsupported or no download source exists.
    pub skipped: Vec<String>,
}

/// Detect a modpack's format from the zip contents.
pub async fn detect_modpack_format(pack_path: &Path) -> Result<ModpackFormat> {
    let pack_path = pack_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let archive = zip::ZipArchive::new(std::fs::File::open(&pack_path)?)?;
        if archive.index_for_name(mrpack::INDEX_FILE).is_some() {
            Ok(ModpackFormat::Modrinth)
        } else if archive.index_for_name(cfpack::MANIFEST_FILE).is_some() {
            Ok(ModpackFormat::CurseForge)
        } else {
            Err(HexoError::Other(format!(
                "unrecognized modpack format: {}",
                pack_path.display()
            )))
        }
    })
    .await
    .map_err(|e| HexoError::Other(e.to_string()))?
}

/// Install a local modpack file, detecting its format.
///
/// `java_path`: needed by the Forge/NeoForge installers; None finds a Java matching the
/// MC version automatically.
/// `curseforge`: required for CurseForge modpacks.
pub async fn install_modpack(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    curseforge: Option<&CurseForgeClient>,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    match detect_modpack_format(pack_path).await? {
        ModpackFormat::Modrinth => {
            mrpack::install_mrpack(pack_path, instance_name, base_dir, java_path, progress).await
        }
        ModpackFormat::CurseForge => {
            let cf = curseforge.ok_or_else(|| {
                HexoError::Other("installing a CurseForge modpack requires a CurseForgeClient".into())
            })?;
            cfpack::install_cfpack(pack_path, instance_name, base_dir, java_path, cf, progress).await
        }
    }
}

/// Extract overrides and download pack files, detecting the local pack format.
/// Minecraft and the mod loader are not installed; install them later with
/// [`install_with_loader`] using the returned [`ModpackInfo`]. No Java is required.
/// A CurseForge client is required for CurseForge archives.
pub async fn install_modpack_files(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    curseforge: Option<&CurseForgeClient>,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    match detect_modpack_format(pack_path).await? {
        ModpackFormat::Modrinth => {
            mrpack::install_mrpack_files(pack_path, instance_name, base_dir, progress).await
        }
        ModpackFormat::CurseForge => {
            let cf = curseforge.ok_or_else(|| {
                HexoError::Other("installing a CurseForge modpack requires a CurseForgeClient".into())
            })?;
            cfpack::install_cfpack_files(pack_path, instance_name, base_dir, cf, progress).await
        }
    }
}

pub(crate) fn instance_game_dir(base_dir: &Path, instance_name: &str) -> PathBuf {
    base_dir.join("instance").join(instance_name).join(".minecraft")
}

/// Install vanilla plus the loader the pack asks for.
///
/// NeoForge for 1.20.1 is rejected: it is published under the old `forge` artifact,
/// which the NeoForge installer does not handle.
pub(crate) async fn install_pack_loader(
    info: &ModpackInfo,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    progress: ProgressFn,
) -> Result<()> {
    let mc = info.mc_version.as_str();

    let loader: Box<dyn LoaderInstaller> = match info.loader {
        LoaderType::Vanilla => Box::new(VanillaInstaller),
        LoaderType::Fabric => Box::new(match &info.loader_version {
            Some(v) => FabricInstaller::new(v),
            None => FabricInstaller::latest(),
        }),
        LoaderType::Forge | LoaderType::NeoForge => {
            if info.loader == LoaderType::NeoForge && mc == "1.20.1" {
                return Err(HexoError::UnsupportedLoader(format!("NeoForge for {}", mc)));
            }
            VanillaInstaller
                .install(mc, instance_name, base_dir, progress.clone())
                .await?;
            let java = match java_path {
                Some(p) => p.to_path_buf(),
                None => {
                    let instance_dir = base_dir.join("instance").join(instance_name);
                    let required = InstanceConfig::load(&instance_dir).await?.java_version;
                    find_java(required)
                        .ok_or(HexoError::JavaNotFound { required })?
                        .path
                }
            };
            if info.loader == LoaderType::Forge {
                let version = match &info.loader_version {
                    Some(v) => Some(resolve_forge_version(mc, v).await?),
                    None => None,
                };
                Box::new(ForgeInstaller { forge_version: version, java_path: java })
            } else {
                Box::new(NeoForgeInstaller {
                    neoforge_version: info.loader_version.clone(),
                    java_path: java,
                })
            }
        }
    };

    install_with_loader(mc, instance_name, base_dir, loader.as_ref(), progress).await
}

/// Map a bare Forge version (`47.2.0`) to the full string used by the Forge version list
/// (`1.20.1-47.2.0`, or legacy forms like `1.7.10-10.13.4.1614-1.7.10`).
pub async fn resolve_forge_version(mc_version: &str, loader_version: &str) -> Result<String> {
    let full = format!("{}-{}", mc_version, loader_version);
    let versions = get_forge_versions(mc_version).await?;
    Ok(versions
        .into_iter()
        .find(|v| *v == full || v.starts_with(&format!("{}-", full)))
        .unwrap_or(full))
}

/// Join a pack-provided relative path onto `root`, rejecting absolute paths and `..`
/// so a pack cannot write outside the instance.
pub(crate) fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let rel = Path::new(relative);
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(HexoError::Other(format!("unsafe path in modpack: {}", relative)));
    }
    Ok(root.join(rel))
}

/// Extract every file under `prefix/` in the zip into `dest`, stripping the prefix.
/// An empty `prefix` extracts everything. Returns the number of files written.
pub(crate) async fn extract_zip_dir(zip_path: &Path, prefix: &str, dest: &Path) -> Result<usize> {
    let zip_path = zip_path.to_path_buf();
    let dest = dest.to_path_buf();
    let prefix = prefix.trim_matches('/').to_string();

    tokio::task::spawn_blocking(move || -> Result<usize> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path)?)?;
        let mut count = 0;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let Some(name) = entry.enclosed_name() else { continue };
            let rel = if prefix.is_empty() {
                name
            } else {
                match name.strip_prefix(&prefix) {
                    Ok(r) => r.to_path_buf(),
                    Err(_) => continue,
                }
            };
            if rel.as_os_str().is_empty() {
                continue;
            }
            let out = dest.join(&rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out)?;
                continue;
            }
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut file)?;
            count += 1;
        }
        Ok(count)
    })
    .await
    .map_err(|e| HexoError::Other(e.to_string()))?
}

/// Read and deserialize a single JSON file from a zip.
pub(crate) async fn read_zip_json<T>(zip_path: &Path, name: &'static str) -> Result<T>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let zip_path = zip_path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<T> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path)?)?;
        let entry = archive.by_name(name)?;
        Ok(serde_json::from_reader(entry)?)
    })
    .await
    .map_err(|e| HexoError::Other(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_pack_files_install_without_minecraft_or_java() {
        use std::io::Write;
        use zip::write::{SimpleFileOptions, ZipWriter};
        use serde_json::json;

        let cases = [
            (mrpack::INDEX_FILE, json!({
                "formatVersion": 1, "game": "minecraft", "versionId": "1.0",
                "name": "Content Pack", "files": [],
                "dependencies": {"minecraft": "1.20.1", "forge": "1.20.1-47.2.0"}
            })),
            (cfpack::MANIFEST_FILE, json!({
                "name": "Content Pack", "version": "1.0", "files": [],
                "minecraft": {"version": "1.20.1", "modLoaders": [{"id": "forge-47.2.0", "primary": true}]}
            })),
        ];
        for (manifest_name, manifest) in cases {
            let temp = tempfile::tempdir().unwrap();
            let pack_path = temp.path().join("pack.zip");
            let base = temp.path().join("launcher");
            {
                let mut zip = ZipWriter::new(std::fs::File::create(&pack_path).unwrap());
                let options = SimpleFileOptions::default();
                zip.start_file(manifest_name, options).unwrap();
                zip.write_all(manifest.to_string().as_bytes()).unwrap();
                zip.start_file("overrides/config/a.toml", options).unwrap();
                zip.write_all(b"enabled = true").unwrap();
                if manifest_name == mrpack::INDEX_FILE {
                    zip.start_file("client-overrides/config/a.toml", options).unwrap();
                    zip.write_all(b"client = true").unwrap();
                }
                zip.finish().unwrap();
            }
            let client = CurseForgeClient::new("");
            let cf = (manifest_name == cfpack::MANIFEST_FILE).then_some(&client);
            let result = install_modpack_files(&pack_path, "content", &base, cf, crate::no_progress()).await.unwrap();
            let instance = base.join("instance/content");
            let expected = if manifest_name == mrpack::INDEX_FILE { "client = true" } else { "enabled = true" };
            assert_eq!(std::fs::read_to_string(instance.join(".minecraft/config/a.toml")).unwrap(), expected);
            assert!(!instance.join("instance_config.json").exists());
            assert!(!base.join("libraries").exists());
            assert!(!base.join("assets").exists());
            assert_eq!(result.info.name, "Content Pack");
            assert_eq!(result.info.version.as_deref(), Some("1.0"));
            assert_eq!(result.info.mc_version, "1.20.1");
            assert_eq!(result.info.loader, LoaderType::Forge);
            assert_eq!(result.info.loader_version.as_deref(), Some("47.2.0"));
            assert!(result.manual_downloads.is_empty());
            assert!(result.skipped.is_empty());
        }
    }

    #[test]
    fn every_format_rejects_neoforge_1_20_1_in_info() {
        use serde_json::json;
        let mr: mrpack::MrpackIndex = serde_json::from_value(json!({
            "formatVersion": 1, "game": "minecraft", "versionId": "1", "name": "Test", "files": [],
            "dependencies": {"minecraft": "1.20.1", "neoforge": "47.1.0"}
        })).unwrap();
        let cf: cfpack::CfManifest = serde_json::from_value(json!({
            "minecraft": {"version": "1.20.1", "modLoaders": [{"id": "neoforge-47.1.0"}]}
        })).unwrap();
        let at: atpack::AtPackConfig = serde_json::from_value(json!({
            "version": "1", "minecraft": "1.20.1", "loader": {"type": "neoforge", "metadata": {"version": "47.1.0"}}
        })).unwrap();
        let ftb: ftbpack::FtbVersionManifest = serde_json::from_value(json!({
            "id": 1, "name": "1", "targets": [
                {"name": "minecraft", "version": "1.20.1"},
                {"name": "neoforge", "version": "47.1.0"}
            ]
        })).unwrap();
        for result in [mr.info(), cf.info(), at.info("Test"), ftb.info("Test")] {
            assert!(matches!(result, Err(HexoError::UnsupportedLoader(message)) if message.contains("NeoForge for 1.20.1")));
        }
    }

    #[test]
    fn safe_join_rejects_escape() {
        let root = Path::new("/game");
        assert!(safe_join(root, "mods/a.jar").is_ok());
        assert!(safe_join(root, "../evil.jar").is_err());
        assert!(safe_join(root, "mods/../../evil.jar").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
    }

    #[tokio::test]
    async fn extract_zip_dir_strips_prefix() {
        use std::io::Write;
        use zip::write::{SimpleFileOptions, ZipWriter};

        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("pack.zip");
        {
            let mut w = ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let opts = SimpleFileOptions::default();
            w.start_file("overrides/config/a.toml", opts).unwrap();
            w.write_all(b"a = 1").unwrap();
            w.start_file("other/b.txt", opts).unwrap();
            w.write_all(b"b").unwrap();
            w.finish().unwrap();
        }

        let dest = tmp.path().join("out");
        let n = extract_zip_dir(&zip_path, "overrides", &dest).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(std::fs::read_to_string(dest.join("config/a.toml")).unwrap(), "a = 1");
        assert!(!dest.join("other").exists());
    }
}
