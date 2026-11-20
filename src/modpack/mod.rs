//! Modpack installation: Modrinth (`.mrpack`), CurseForge (`manifest.json` zip), HMCL
//! (`modpack.json` or `mcbbs.packmeta` zip) and ATLauncher and FTB (online packs).
//!
//! Every format follows the same flow: parse the pack, install vanilla + the loader via
//! `install_with_loader`, extract overrides/configs into `instance/{name}/.minecraft`,
//! then download the files listed by the pack.
//! The `*_files` entry points only install pack contents, leaving Minecraft and its
//! loader for a later launch. They do not create `instance_config.json`.

pub mod atpack;
pub mod cfpack;
pub mod ftbpack;
pub mod hmclpack;
pub mod mrpack;
mod source;
mod state;

pub use source::{
    download_modpack_archive, identify_modpack, inspect_modpack, modpack_file_sha1,
    modpack_update_candidates, DownloadedModpack, IdentifiedModpack, ModpackPreview,
    ModpackUpdateCandidates, OnlineModpack,
};

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
    /// HMCL's own format (`modpack.json` + `minecraft/`).
    Hmcl,
    /// MCBBS format exported by HMCL (`mcbbs.packmeta` + `overrides/`).
    Mcbbs,
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
    pub sha1: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModpackInstallResult {
    pub info: ModpackInfo,
    pub manual_downloads: Vec<ManualDownload>,
    /// Names of entries skipped because their type is unsupported or no download source exists.
    pub skipped: Vec<String>,
}

/// Result of changing an installed local modpack to another version.
#[derive(Debug, Clone)]
pub struct ModpackUpdateResult {
    pub install: ModpackInstallResult,
    /// Files managed by the previous pack version and removed by this update.
    pub removed_files: Vec<PathBuf>,
    /// Local changes or personal files kept in place when the pack overlapped with them.
    pub preserved_files: Vec<PathBuf>,
}

/// Detect a modpack's format from the zip contents.
///
/// MCBBS packs usually also ship a CurseForge `manifest.json` for compatibility. When
/// that manifest lists CurseForge files the pack is treated as CurseForge so the files
/// get downloaded; otherwise `mcbbs.packmeta` wins.
pub async fn detect_modpack_format(pack_path: &Path) -> Result<ModpackFormat> {
    let pack_path = pack_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&pack_path)?)?;
        let has = |archive: &zip::ZipArchive<std::fs::File>, name| archive.index_for_name(name).is_some();
        let has_cf = has(&archive, cfpack::MANIFEST_FILE);
        if has(&archive, mrpack::INDEX_FILE) {
            Ok(ModpackFormat::Modrinth)
        } else if has(&archive, hmclpack::MCBBS_MANIFEST_FILE) {
            let cf_has_files = has_cf
                && serde_json::from_reader::<_, cfpack::CfManifest>(
                    archive.by_name(cfpack::MANIFEST_FILE)?,
                )
                .is_ok_and(|manifest| !manifest.files.is_empty());
            Ok(if cf_has_files {
                ModpackFormat::CurseForge
            } else {
                ModpackFormat::Mcbbs
            })
        } else if has_cf {
            Ok(ModpackFormat::CurseForge)
        } else if has(&archive, hmclpack::HMCL_MANIFEST_FILE) {
            Ok(ModpackFormat::Hmcl)
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

/// Select whether to install the game and loader in addition to pack contents.
#[derive(Debug, Clone, Copy)]
pub enum PackGameInstall<'a> {
    FilesOnly,
    WithGame { java_path: Option<&'a Path> },
}

/// Install a local archive and the Minecraft/loader versions it requires.
pub async fn install_modpack(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    curseforge: Option<&CurseForgeClient>,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    Ok(update_modpack(
        pack_path,
        instance_name,
        base_dir,
        java_path,
        curseforge,
        progress,
    )
    .await?
    .install)
}

/// Apply a local archive and install the Minecraft/loader versions it requires.
pub async fn update_modpack(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    curseforge: Option<&CurseForgeClient>,
    progress: ProgressFn,
) -> Result<ModpackUpdateResult> {
    apply_archive(
        pack_path,
        instance_name,
        base_dir,
        PackGameInstall::WithGame { java_path },
        curseforge,
        progress,
    )
    .await
}

/// Install the files from a local archive without installing Minecraft or its loader.
pub async fn install_modpack_files(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    curseforge: Option<&CurseForgeClient>,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    Ok(
        update_modpack_files(pack_path, instance_name, base_dir, curseforge, progress)
            .await?
            .install,
    )
}

/// Update the files from a local archive without installing Minecraft or its loader.
pub async fn update_modpack_files(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    curseforge: Option<&CurseForgeClient>,
    progress: ProgressFn,
) -> Result<ModpackUpdateResult> {
    apply_archive(
        pack_path,
        instance_name,
        base_dir,
        PackGameInstall::FilesOnly,
        curseforge,
        progress,
    )
    .await
}

async fn apply_archive(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    game_install: PackGameInstall<'_>,
    curseforge: Option<&CurseForgeClient>,
    progress: ProgressFn,
) -> Result<ModpackUpdateResult> {
    let format = detect_modpack_format(pack_path).await?;
    apply_pack(
        instance_name,
        base_dir,
        game_install,
        progress,
        |base, installed_game, progress| async move {
            match format {
                ModpackFormat::Modrinth => {
                    mrpack::stage_mrpack_files(
                        pack_path,
                        "staged",
                        &base,
                        Some(&installed_game),
                        progress,
                    )
                    .await
                }
                ModpackFormat::CurseForge => {
                    let client = curseforge.ok_or_else(|| {
                        HexoError::Other("a CurseForge modpack requires a CurseForgeClient".into())
                    })?;
                    cfpack::stage_cfpack_files(
                        pack_path,
                        "staged",
                        &base,
                        client,
                        Some(&installed_game),
                        progress,
                    )
                    .await
                }
                ModpackFormat::Hmcl => {
                    hmclpack::install_hmcl_pack_files(pack_path, "staged", &base, progress).await
                }
                ModpackFormat::Mcbbs => {
                    hmclpack::install_mcbbs_pack_files(pack_path, "staged", &base, progress).await
                }
            }
        },
    )
    .await
}

/// Install or update a manifest-based online pack, keeping existing local modifications.
pub async fn apply_online_modpack(
    pack: &OnlineModpack,
    instance_name: &str,
    base_dir: &Path,
    game_install: PackGameInstall<'_>,
    curseforge: Option<&CurseForgeClient>,
    include_optional: bool,
    progress: ProgressFn,
) -> Result<ModpackUpdateResult> {
    apply_pack(
        instance_name,
        base_dir,
        game_install,
        progress,
        |base, installed_game, progress| async move {
            match pack {
                OnlineModpack::AtLauncher {
                    name,
                    version,
                    config,
                } => {
                    atpack::install_config_files(
                        config,
                        name,
                        version,
                        "staged",
                        &base,
                        include_optional,
                        Some(&installed_game),
                        progress,
                    )
                    .await
                }
                OnlineModpack::Ftb { name, manifest } => {
                    ftbpack::install_manifest_files(
                        manifest,
                        name,
                        "staged",
                        &base,
                        curseforge,
                        include_optional,
                        Some(&installed_game),
                        progress,
                    )
                    .await
                }
            }
        },
    )
    .await
}

static PACK_WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn apply_pack<F, Fut>(
    instance_name: &str,
    base_dir: &Path,
    game_install: PackGameInstall<'_>,
    progress: ProgressFn,
    stage: F,
) -> Result<ModpackUpdateResult>
where
    F: FnOnce(PathBuf, PathBuf, ProgressFn) -> Fut,
    Fut: std::future::Future<Output = Result<ModpackInstallResult>>,
{
    let instance = pack_instance_dir(base_dir, instance_name)?;
    tokio::fs::create_dir_all(&instance).await?;
    let staging = tempfile::Builder::new()
        .prefix(".pack-stage-")
        .tempdir_in(&instance)?;
    let staged_game = instance_game_dir(staging.path(), "staged");
    let installed_game = safe_join(&instance, ".minecraft")?;
    let install = stage(staging.path().to_owned(), installed_game, progress.clone()).await?;
    let lock = PACK_WRITE.lock().await;
    if let PackGameInstall::WithGame { java_path } = game_install {
        install_pack_loader(
            &install.info,
            instance_name,
            base_dir,
            java_path,
            progress.clone(),
        )
        .await?;
    }
    progress(0, 0, "Applying modpack files");
    tokio::task::spawn_blocking(move || {
        let _staging = staging;
        let _lock = lock;
        state::apply(&instance, &staged_game, install)
    })
    .await
    .map_err(|error| HexoError::Other(error.to_string()))?
}

fn pack_instance_dir(base_dir: &Path, instance_name: &str) -> Result<PathBuf> {
    let mut name = Path::new(instance_name).components();
    if !matches!(
        (name.next(), name.next()),
        (Some(Component::Normal(_)), None)
    ) {
        return Err(HexoError::Other(format!(
            "invalid instance name: {instance_name}"
        )));
    }
    safe_join(base_dir, &format!("instance/{instance_name}"))
}

/// List absent files recorded by the pack. Disabled mods count as present.
pub async fn missing_modpack_files(base_dir: &Path, instance_name: &str) -> Result<Vec<String>> {
    let instance = pack_instance_dir(base_dir, instance_name)?;
    tokio::task::spawn_blocking(move || state::missing_files(&instance))
        .await
        .map_err(|error| HexoError::Other(error.to_string()))?
}

/// Determine whether the installed versions already meet the pack's requirements.
pub fn modpack_game_matches(
    current_mc: &str,
    current_loader: &LoaderType,
    current_version: &str,
    target_mc: &str,
    target_loader: &LoaderType,
    target_version: Option<&str>,
) -> bool {
    if current_mc != target_mc || current_loader != target_loader {
        return false;
    }
    if *target_loader == LoaderType::Vanilla {
        return true;
    }
    let Some(wanted) = target_version.filter(|version| !version.is_empty()) else {
        return false;
    };
    if current_version == wanted {
        return true;
    }
    if *target_loader != LoaderType::Forge {
        return false;
    }
    let bare = wanted
        .strip_prefix(&format!("{target_mc}-"))
        .unwrap_or(wanted);
    let full = format!("{target_mc}-{bare}");
    current_version == bare
        || current_version == full
        || current_version.starts_with(&format!("{full}-"))
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
    #[cfg(windows)]
    if rel.components().any(|part| match part {
        Component::Normal(name) => {
            let name = name.to_string_lossy();
            let stem = name
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            name.contains(':')
                || name.ends_with([' ', '.'])
                || matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                )
                || stem
                    .strip_prefix("COM")
                    .or_else(|| stem.strip_prefix("LPT"))
                    .is_some_and(|suffix| {
                        matches!(
                            suffix,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    })
        }
        _ => false,
    }) {
        return Err(HexoError::Other(format!(
            "unsafe path in modpack: {}",
            relative
        )));
    }
    let joined = root.join(rel);
    for ancestor in joined.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                let linked = metadata.file_type().is_symlink();
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    linked || metadata.file_attributes() & 0x400 != 0
                };
                if linked {
                    return Err(HexoError::Other(format!(
                        "linked path in modpack destination: {}", ancestor.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        if ancestor == root {
            break;
        }
    }
    Ok(joined)
}

/// Extract every file under `prefix/` in the zip into `dest`, stripping the prefix.
/// An empty `prefix` extracts everything. Returns the number of files written.
pub(crate) async fn extract_zip_dir(zip_path: &Path, prefix: &str, dest: &Path) -> Result<usize> {
    extract_zip_dir_filtered(zip_path, prefix, dest, &[]).await
}

/// Like [`extract_zip_dir`], but skips the files whose path relative to `prefix` is in `skip`.
pub(crate) async fn extract_zip_dir_filtered(
    zip_path: &Path,
    prefix: &str,
    dest: &Path,
    skip: &[&str],
) -> Result<usize> {
    let zip_path = zip_path.to_path_buf();
    let dest = dest.to_path_buf();
    let prefix = prefix.trim_matches('/').to_string();
    let skip: Vec<PathBuf> = skip.iter().map(PathBuf::from).collect();

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
            if rel.as_os_str().is_empty() || skip.contains(&rel) {
                continue;
            }
            let out = safe_join(&dest, &rel.to_string_lossy())?;
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

    fn write_pack(path: &Path, version: &str, files: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut pack = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        pack.start_file(mrpack::INDEX_FILE, options).unwrap();
        write!(
            pack,
            "{}",
            serde_json::json!({
                "formatVersion": 1, "game": "minecraft", "versionId": version,
                "name": "Pack", "files": [], "dependencies": {"minecraft": "1.21.1"},
            })
        )
        .unwrap();
        for (file, content) in files {
            pack.start_file(format!("overrides/{file}"), options)
                .unwrap();
            pack.write_all(content).unwrap();
        }
        pack.finish().unwrap();
    }

    #[tokio::test]
    async fn updates_clean_files_and_preserves_personal_edits_across_retries() {
        let folder = tempfile::tempdir().unwrap();
        let base = folder.path();
        let first = base.join("first.mrpack");
        let second = base.join("second.mrpack");
        write_pack(
            &first,
            "1",
            &[
                ("config/edited.toml", b"default"),
                ("config/clean.toml", b"one"),
                ("config/removed.toml", b"unused"),
                ("config/edited-removed.toml", b"unused"),
                ("mods/disabled.jar", b"old jar"),
            ],
        );
        write_pack(
            &second,
            "2",
            &[
                ("config/edited.toml", b"new defaults"),
                ("config/clean.toml", b"two"),
                ("config/new.toml", b"new"),
                ("mods/disabled.jar", b"new jar"),
            ],
        );
        install_modpack_files(&first, "test", base, None, crate::no_progress())
            .await
            .unwrap();
        let game = instance_game_dir(base, "test");
        std::fs::write(game.join("config/edited.toml"), b"my settings").unwrap();
        std::fs::write(game.join("config/edited-removed.toml"), b"keep me").unwrap();
        std::fs::write(game.join("mods/personal.jar"), b"my mod").unwrap();
        std::fs::rename(
            game.join("mods/disabled.jar"),
            game.join("mods/disabled.jar.disabled"),
        )
        .unwrap();
        for _ in 0..2 {
            let result = update_modpack_files(&second, "test", base, None, crate::no_progress())
                .await
                .unwrap();
            assert!(result
                .preserved_files
                .contains(&game.join("config/edited.toml")));
            assert_eq!(
                std::fs::read(game.join("config/edited.toml")).unwrap(),
                b"my settings"
            );
            assert_eq!(
                std::fs::read(game.join("config/clean.toml")).unwrap(),
                b"two"
            );
            assert_eq!(
                std::fs::read(game.join("config/edited-removed.toml")).unwrap(),
                b"keep me"
            );
            assert!(!game.join("config/removed.toml").exists());
            assert_eq!(
                std::fs::read(game.join("mods/personal.jar")).unwrap(),
                b"my mod"
            );
            assert_eq!(
                std::fs::read(game.join("mods/disabled.jar.disabled")).unwrap(),
                b"new jar"
            );
            assert!(!game.join("mods/disabled.jar").exists());
            assert!(missing_modpack_files(base, "test")
                .await
                .unwrap()
                .is_empty());
        }
        std::fs::remove_file(game.join("config/new.toml")).unwrap();
        assert_eq!(
            missing_modpack_files(base, "test").await.unwrap(),
            ["config/new.toml"]
        );
        update_modpack_files(&second, "test", base, None, crate::no_progress())
            .await
            .unwrap();
        assert_eq!(std::fs::read(game.join("config/new.toml")).unwrap(), b"new");
    }

    #[tokio::test]
    async fn legacy_state_keeps_unverified_files() {
        let folder = tempfile::tempdir().unwrap();
        let base = folder.path();
        let archive = base.join("test.mrpack");
        write_pack(&archive, "2", &[("config/conflict.toml", b"defaults")]);
        let game = instance_game_dir(base, "test");
        std::fs::create_dir_all(game.join("config")).unwrap();
        std::fs::write(game.join("config/conflict.toml"), b"personal").unwrap();
        std::fs::write(game.join("config/removed.toml"), b"old").unwrap();
        std::fs::write(
            game.parent().unwrap().join("modpack_state.json"),
            r#"{"managed_files":["config/removed.toml"]}"#,
        )
        .unwrap();
        let result = update_modpack_files(&archive, "test", base, None, crate::no_progress())
            .await
            .unwrap();
        assert_eq!(result.preserved_files.len(), 2);
        assert_eq!(
            std::fs::read(game.join("config/conflict.toml")).unwrap(),
            b"personal"
        );
        assert_eq!(
            std::fs::read(game.join("config/removed.toml")).unwrap(),
            b"old"
        );
    }

    #[tokio::test]
    async fn failed_apply_restores_previous_files_and_state() {
        let folder = tempfile::tempdir().unwrap();
        let base = folder.path();
        let archive = base.join("pack.mrpack");
        write_pack(&archive, "1", &[("config/a.toml", b"old config")]);
        install_modpack_files(&archive, "test", base, None, crate::no_progress())
            .await
            .unwrap();
        let game = instance_game_dir(base, "test");
        let state = game.parent().unwrap().join("modpack_state.json");
        let original_state = std::fs::read(&state).unwrap();
        std::fs::write(game.join("z"), b"personal file").unwrap();
        write_pack(
            &archive,
            "2",
            &[("config/a.toml", b"new config"), ("z/x.toml", b"new")],
        );
        assert!(
            update_modpack_files(&archive, "test", base, None, crate::no_progress())
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(game.join("config/a.toml")).unwrap(),
            b"old config"
        );
        assert_eq!(std::fs::read(game.join("z")).unwrap(), b"personal file");
        assert_eq!(std::fs::read(state).unwrap(), original_state);
    }

    #[tokio::test]
    async fn update_reuses_checked_downloads_and_respects_personal_mod_ids() {
        use std::io::{Cursor, Write};
        fn jar_bytes(id: &str, version: &str) -> Vec<u8> {
            let mut jar = zip::ZipWriter::new(Cursor::new(Vec::new()));
            jar.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            write!(jar, "{{\"id\":\"{id}\",\"version\":\"{version}\"}}").unwrap();
            jar.finish().unwrap().into_inner()
        }

        let folder = tempfile::tempdir().unwrap();
        let base = folder.path();
        let initial = base.join("initial.mrpack");
        let updated = base.join("updated.mrpack");
        write_pack(
            &initial,
            "1",
            &[
                ("mods/managed-1.jar", &jar_bytes("managed", "1")),
                ("mods/cache.jar", b"cached download"),
            ],
        );
        install_modpack_files(&initial, "test", base, None, crate::no_progress())
            .await
            .unwrap();
        let game = instance_game_dir(base, "test");
        std::fs::rename(
            game.join("mods/managed-1.jar"),
            game.join("mods/managed-1.jar.disabled"),
        )
        .unwrap();
        std::fs::write(game.join("mods/my-version.jar"), jar_bytes("personal", "0")).unwrap();
        let hash = state::sha1_file(&game.join("mods/cache.jar")).unwrap();
        {
            let mut archive = zip::ZipWriter::new(std::fs::File::create(&updated).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            archive.start_file(mrpack::INDEX_FILE, options).unwrap();
            write!(
                archive,
                "{}",
                serde_json::json!({
                    "formatVersion": 1, "game": "minecraft", "versionId": "2", "name": "Pack",
                    "files": [{
                        "path": "mods/cache.jar", "hashes": {"sha1": hash},
                        "downloads": ["https://invalid.invalid/cache.jar"]
                    }],
                    "dependencies": {"minecraft": "1.21.1"},
                })
            )
            .unwrap();
            for (name, id) in [("managed-2", "managed"), ("pack-version", "personal")] {
                archive
                    .start_file(format!("overrides/mods/{name}.jar"), options)
                    .unwrap();
                archive.write_all(&jar_bytes(id, "2")).unwrap();
            }
            archive.finish().unwrap();
        }
        let result = update_modpack_files(&updated, "test", base, None, crate::no_progress())
            .await
            .unwrap();
        assert_eq!(
            result.removed_files,
            [game.join("mods/managed-1.jar.disabled")]
        );
        assert_eq!(result.preserved_files, [game.join("mods/my-version.jar")]);
        assert!(game.join("mods/managed-2.jar.disabled").is_file());
        assert!(!game.join("mods/pack-version.jar").exists());
        assert!(missing_modpack_files(base, "test")
            .await
            .unwrap()
            .is_empty());
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

    #[tokio::test]
    async fn archive_paths_cannot_escape_after_prefix_removal() {
        use std::io::Write;
        let root = tempfile::tempdir().unwrap();
        let pack = root.path().join("pack.zip");
        let outside = root.path().join("instance_config.json");
        std::fs::write(&outside, b"original").unwrap();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&pack).unwrap());
        zip.start_file("overrides/../instance_config.json", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"changed").unwrap();
        zip.finish().unwrap();
        let _ = extract_zip_dir(&pack, "overrides", &root.path().join("game")).await;
        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn pack_destinations_reject_existing_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, game.join("config")).unwrap();
        assert!(safe_join(&game, "config/settings.toml").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn pack_destinations_reject_existing_junctions() {
        use std::os::windows::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let status = std::process::Command::new("powershell.exe")
            .creation_flags(0x08000000)
            .args(["-NoProfile", "-NonInteractive", "-Command", "New-Item -ItemType Junction -Path $env:HEXOMC_TEST_LINK -Target $env:HEXOMC_TEST_TARGET -ErrorAction Stop | Out-Null"])
            .env("HEXOMC_TEST_LINK", game.join("config"))
            .env("HEXOMC_TEST_TARGET", &outside)
            .status().unwrap();
        assert!(status.success());
        assert!(safe_join(&game, "config/settings.toml").is_err());
    }
}
