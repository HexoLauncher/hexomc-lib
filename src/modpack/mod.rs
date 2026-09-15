//! Modpack installation: Modrinth (`.mrpack`), CurseForge (`manifest.json` zip) and
//! ATLauncher (online packs).
//!
//! Every format follows the same flow: parse the pack, install vanilla + the loader via
//! `install_with_loader`, extract overrides/configs into `instance/{name}/.minecraft`,
//! then download the files listed by the pack.

pub mod atpack;
pub mod cfpack;
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

/// Format of a local modpack file. ATLauncher packs have no file format; use
/// [`atpack::install_atlauncher_pack`] for those.
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
    /// Names of entries skipped because their type is not supported.
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
async fn resolve_forge_version(mc_version: &str, loader_version: &str) -> Result<String> {
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
