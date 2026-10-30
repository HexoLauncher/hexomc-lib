//! HMCL modpacks, in both formats HMCL exports.
//!
//! - HMCL format: `modpack.json` holds the pack metadata, `minecraft/pack.json` is the
//!   merged version JSON (loaders are read from its `patches` or `libraries`), and every
//!   other file under `minecraft/` belongs in the game directory.
//! - MCBBS format: `mcbbs.packmeta` lists the game and loaders as `addons`; game files
//!   live under `overrides/`. These packs often also carry a CurseForge `manifest.json`;
//!   [`super::detect_modpack_format`] prefers that when it lists CurseForge files.
//!
//! Neither format downloads anything: all content is inside the archive.
//!
//! [`HmclVersionJson`] also reads installed versions of HMCL and the official launcher
//! (`.minecraft/versions/{id}/{id}.json`), which share the format.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    error::{HexoError, Result},
    install::{loader::ProgressFn, vanilla::LoaderType},
    modpack::{
        extract_zip_dir_filtered, install_pack_loader, instance_game_dir, read_zip_json,
        ModpackInfo, ModpackInstallResult,
    },
};

pub const HMCL_MANIFEST_FILE: &str = "modpack.json";
pub const HMCL_VERSION_FILE: &str = "minecraft/pack.json";
pub const MCBBS_MANIFEST_FILE: &str = "mcbbs.packmeta";

const HMCL_GAME_DIR: &str = "minecraft";
const MCBBS_OVERRIDES: &str = "overrides";

/// `modpack.json` of an HMCL-format pack.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HmclManifest {
    pub name: String,
    pub author: Option<String>,
    pub version: Option<String>,
    pub game_version: Option<String>,
    pub description: Option<String>,
}

/// The parts of a launcher version JSON needed to find the game and loader versions.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HmclVersionJson {
    pub id: Option<String>,
    pub inherits_from: Option<String>,
    pub client_version: Option<String>,
    pub patches: Vec<HmclPatch>,
    pub libraries: Vec<HmclLibrary>,
    pub arguments: Option<serde_json::Value>,
}

impl HmclVersionJson {
    /// The Minecraft version: HMCL's `game` patch, then `inheritsFrom`, `clientVersion`
    /// and finally `id` (a plain vanilla version is named after itself).
    pub fn mc_version(&self) -> Option<String> {
        let patch_game = self
            .patches
            .iter()
            .find(|p| p.id == "game")
            .and_then(|p| p.version.as_ref());
        non_empty(patch_game)
            .or_else(|| non_empty(self.inherits_from.as_ref()))
            .or_else(|| non_empty(self.client_version.as_ref()))
            .or_else(|| non_empty(self.id.as_ref()))
    }

    /// The loader and its bare version (Forge `47.2.0`, not `1.20.1-47.2.0`), read from
    /// HMCL `patches`, then FML launch arguments, then library coordinates.
    pub fn loader(&self, mc_version: &str) -> Result<(LoaderType, Option<String>)> {
        let found = match loader_from_ids(
            self.patches.iter().map(|p| (p.id.as_str(), p.version.as_deref())),
        )? {
            Some(found) => Some(found),
            None => match self.loader_from_arguments() {
                Some(found) => Some(found),
                None => loader_from_libraries(&self.libraries)?,
            },
        };
        let (loader, version) = found.unwrap_or((LoaderType::Vanilla, None));
        Ok((loader, version.map(|v| bare_loader_version(mc_version, v))))
    }

    fn loader_from_arguments(&self) -> Option<(LoaderType, Option<String>)> {
        let game = self.arguments.as_ref()?.get("game")?.as_array()?;
        let args: Vec<&str> = game.iter().filter_map(|a| a.as_str()).collect();
        let value_of = |flag: &str| {
            args.iter()
                .position(|a| *a == flag)
                .and_then(|i| args.get(i + 1))
                .map(|v| v.to_string())
        };
        if let Some(version) = value_of("--fml.neoForgeVersion") {
            return Some((LoaderType::NeoForge, Some(version)));
        }
        value_of("--fml.forgeVersion").map(|version| (LoaderType::Forge, Some(version)))
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(default)]
pub struct HmclPatch {
    /// `game`, `forge`, `neoforge`, `fabric`, `quilt`, `optifine`, `liteloader`, ...
    pub id: String,
    pub version: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(default)]
pub struct HmclLibrary {
    /// Maven coordinate, e.g. `net.fabricmc:fabric-loader:0.16.5`.
    pub name: String,
}

/// A parsed HMCL-format pack.
#[derive(Debug, Clone)]
pub struct HmclPack {
    pub manifest: HmclManifest,
    pub version: HmclVersionJson,
}

/// `mcbbs.packmeta` of an MCBBS-format pack.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct McbbsManifest {
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub addons: Vec<McbbsAddon>,
    pub files: Vec<McbbsFile>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(default)]
pub struct McbbsAddon {
    /// `game`, `forge`, `neoforge`, `fabric`, `quilt`, `optifine`, ...
    pub id: String,
    pub version: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct McbbsFile {
    /// `addon` (a file inside `overrides/`) or `curse` (a CurseForge file).
    #[serde(rename = "type")]
    pub kind: String,
    pub path: Option<String>,
    #[serde(rename = "projectID")]
    pub project_id: Option<u64>,
    #[serde(rename = "fileID")]
    pub file_id: Option<u64>,
}

/// Loader ids that cannot be installed, mapped from HMCL patch / MCBBS addon ids.
const UNSUPPORTED_LOADERS: [&str; 3] = ["quilt", "legacyfabric", "cleanroom"];

/// Pick the loader from `(id, version)` pairs using HMCL's patch / addon ids.
/// Add-ons that do not replace the loader (OptiFine, LiteLoader) are ignored.
fn loader_from_ids<'a>(
    ids: impl Iterator<Item = (&'a str, Option<&'a str>)>,
) -> Result<Option<(LoaderType, Option<String>)>> {
    let mut found = None;
    for (id, version) in ids {
        let id = id.to_ascii_lowercase();
        let kind = match id.as_str() {
            "neoforge" => LoaderType::NeoForge,
            "forge" => LoaderType::Forge,
            "fabric" => LoaderType::Fabric,
            other if UNSUPPORTED_LOADERS.contains(&other) => {
                return Err(HexoError::UnsupportedLoader(format!(
                    "{} {}",
                    other,
                    version.unwrap_or_default()
                )))
            }
            _ => continue,
        };
        let version = version.filter(|v| !v.is_empty()).map(str::to_string);
        if found.is_none() || kind == LoaderType::NeoForge {
            found = Some((kind, version));
        }
    }
    Ok(found)
}

/// Pick the loader from the version JSON's libraries.
fn loader_from_libraries(libraries: &[HmclLibrary]) -> Result<Option<(LoaderType, Option<String>)>> {
    for library in libraries {
        let mut parts = library.name.split(':');
        let (Some(group), Some(artifact), Some(version)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let kind = match (group, artifact) {
            ("net.neoforged", "neoforge") | ("net.neoforged", "forge") => LoaderType::NeoForge,
            ("net.minecraftforge", "forge") | ("net.minecraftforge", "fmlloader") => {
                LoaderType::Forge
            }
            ("net.fabricmc", "fabric-loader") => LoaderType::Fabric,
            ("org.quiltmc", "quilt-loader") => {
                return Err(HexoError::UnsupportedLoader(format!("quilt-loader {}", version)))
            }
            _ => continue,
        };
        return Ok(Some((kind, Some(version.to_string()))));
    }
    Ok(None)
}

/// Strip the Minecraft version from the loader version, in either position
/// (`1.20.1-47.2.0`, `14.23.5.2860-1.12.2`), as HMCL stores Forge versions both ways.
fn bare_loader_version(mc_version: &str, version: String) -> String {
    let stripped = version
        .strip_prefix(&format!("{}-", mc_version))
        .unwrap_or(&version);
    let stripped = stripped
        .strip_suffix(&format!("-{}", mc_version))
        .unwrap_or(stripped);
    stripped.to_string()
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value.map(|v| v.trim()).filter(|v| !v.is_empty()).map(str::to_string)
}

impl HmclPack {
    pub fn info(&self) -> Result<ModpackInfo> {
        let mc_version = non_empty(self.manifest.game_version.as_ref())
            .or_else(|| self.version.mc_version())
            .ok_or_else(|| HexoError::Other("HMCL modpack has no game version".into()))?;
        let (loader, loader_version) = self.version.loader(&mc_version)?;

        ModpackInfo {
            name: self.manifest.name.clone(),
            version: non_empty(self.manifest.version.as_ref()),
            loader_version,
            mc_version,
            loader,
        }
        .validated()
    }
}

impl McbbsManifest {
    pub fn info(&self) -> Result<ModpackInfo> {
        let mc_version = self
            .addons
            .iter()
            .find(|a| a.id == "game")
            .map(|a| a.version.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| HexoError::Other("MCBBS modpack has no game addon".into()))?;

        let loader = loader_from_ids(
            self.addons
                .iter()
                .map(|a| (a.id.as_str(), Some(a.version.as_str()))),
        )?;
        let (loader, loader_version) = loader.unwrap_or((LoaderType::Vanilla, None));

        ModpackInfo {
            name: self.name.clone(),
            version: non_empty(self.version.as_ref()),
            loader_version: loader_version.map(|v| bare_loader_version(&mc_version, v)),
            mc_version,
            loader,
        }
        .validated()
    }

    /// CurseForge entries cannot be resolved without a `manifest.json`; they are reported
    /// as skipped instead of failing the install.
    fn skipped(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| f.kind == "curse")
            .map(|f| match (&f.path, f.project_id, f.file_id) {
                (Some(path), _, _) => path.clone(),
                (None, project, file) => format!(
                    "curseforge:{}/{}",
                    project.unwrap_or_default(),
                    file.unwrap_or_default()
                ),
            })
            .collect()
    }
}

/// Read an HMCL-format pack's metadata without installing it.
pub async fn read_hmcl_pack(pack_path: &Path) -> Result<HmclPack> {
    let manifest = read_zip_json(pack_path, HMCL_MANIFEST_FILE).await?;
    let version = match read_zip_json(pack_path, HMCL_VERSION_FILE).await {
        Ok(version) => version,
        Err(HexoError::Zip(zip::result::ZipError::FileNotFound)) => HmclVersionJson::default(),
        Err(e) => return Err(e),
    };
    Ok(HmclPack { manifest, version })
}

/// Read an MCBBS-format pack's `mcbbs.packmeta` without installing it.
pub async fn read_mcbbs_manifest(pack_path: &Path) -> Result<McbbsManifest> {
    read_zip_json(pack_path, MCBBS_MANIFEST_FILE).await
}

/// Install an HMCL-format pack into `instance/{instance_name}`.
pub async fn install_hmcl_pack(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let info = read_hmcl_pack(pack_path).await?.info()?;
    install_pack_loader(&info, instance_name, base_dir, java_path, progress.clone()).await?;
    install_hmcl_pack_files(pack_path, instance_name, base_dir, progress).await
}

/// Extract an HMCL-format pack's game files without installing Minecraft or its loader.
/// `minecraft/pack.json` is HMCL's version JSON and is not copied into the game directory.
pub async fn install_hmcl_pack_files(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let info = read_hmcl_pack(pack_path).await?.info()?;

    let game_dir = instance_game_dir(base_dir, instance_name);
    tokio::fs::create_dir_all(&game_dir).await?;

    progress(0, 0, "Extracting overrides");
    extract_zip_dir_filtered(pack_path, HMCL_GAME_DIR, &game_dir, &["pack.json"]).await?;

    Ok(ModpackInstallResult { info, manual_downloads: Vec::new(), skipped: Vec::new() })
}

/// Install an MCBBS-format pack into `instance/{instance_name}`.
pub async fn install_mcbbs_pack(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    java_path: Option<&Path>,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let info = read_mcbbs_manifest(pack_path).await?.info()?;
    install_pack_loader(&info, instance_name, base_dir, java_path, progress.clone()).await?;
    install_mcbbs_pack_files(pack_path, instance_name, base_dir, progress).await
}

/// Extract an MCBBS-format pack's `overrides/` without installing Minecraft or its loader.
pub async fn install_mcbbs_pack_files(
    pack_path: &Path,
    instance_name: &str,
    base_dir: &Path,
    progress: ProgressFn,
) -> Result<ModpackInstallResult> {
    let manifest = read_mcbbs_manifest(pack_path).await?;
    let info = manifest.info()?;

    let game_dir = instance_game_dir(base_dir, instance_name);
    tokio::fs::create_dir_all(&game_dir).await?;

    progress(0, 0, "Extracting overrides");
    extract_zip_dir_filtered(pack_path, MCBBS_OVERRIDES, &game_dir, &[]).await?;

    Ok(ModpackInstallResult {
        info,
        manual_downloads: Vec::new(),
        skipped: manifest.skipped(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hmcl(manifest: serde_json::Value, version: serde_json::Value) -> HmclPack {
        HmclPack {
            manifest: serde_json::from_value(manifest).unwrap(),
            version: serde_json::from_value(version).unwrap(),
        }
    }

    #[test]
    fn hmcl_reads_loader_from_patches() {
        let pack = hmcl(
            json!({"name": "Pack", "version": "1.0", "gameVersion": "1.20.1"}),
            json!({"id": "Pack", "patches": [
                {"id": "game", "version": "1.20.1"},
                {"id": "forge", "version": "47.2.0"},
                {"id": "optifine", "version": "HD_U_I6"}
            ]}),
        );
        let info = pack.info().unwrap();
        assert_eq!(info.mc_version, "1.20.1");
        assert_eq!(info.loader, LoaderType::Forge);
        assert_eq!(info.loader_version.as_deref(), Some("47.2.0"));
        assert_eq!(info.version.as_deref(), Some("1.0"));
    }

    #[test]
    fn hmcl_reads_loader_from_libraries() {
        let pack = hmcl(
            json!({"name": "Pack"}),
            json!({"id": "Pack", "inheritsFrom": "1.12.2", "libraries": [
                {"name": "org.ow2.asm:asm:9.6"},
                {"name": "net.minecraftforge:forge:1.12.2-14.23.5.2860"}
            ]}),
        );
        let info = pack.info().unwrap();
        assert_eq!(info.mc_version, "1.12.2");
        assert_eq!(info.loader, LoaderType::Forge);
        assert_eq!(info.loader_version.as_deref(), Some("14.23.5.2860"));

        let fabric = hmcl(
            json!({"name": "Pack", "gameVersion": "1.21.1"}),
            json!({"libraries": [{"name": "net.fabricmc:fabric-loader:0.16.5"}]}),
        );
        let info = fabric.info().unwrap();
        assert_eq!(info.loader, LoaderType::Fabric);
        assert_eq!(info.loader_version.as_deref(), Some("0.16.5"));
    }

    #[test]
    fn version_json_reads_fml_arguments() {
        let version: HmclVersionJson = serde_json::from_value(json!({
            "id": "1.20.1-forge-47.2.0", "inheritsFrom": "1.20.1",
            "arguments": {"game": ["--launchTarget", "forgeclient", "--fml.forgeVersion", "47.2.0", "--fml.mcVersion", "1.20.1"]}
        }))
        .unwrap();
        assert_eq!(version.mc_version().as_deref(), Some("1.20.1"));
        assert_eq!(
            version.loader("1.20.1").unwrap(),
            (LoaderType::Forge, Some("47.2.0".to_string()))
        );

        let vanilla: HmclVersionJson = serde_json::from_value(json!({"id": "1.21.1"})).unwrap();
        assert_eq!(vanilla.mc_version().as_deref(), Some("1.21.1"));
        assert_eq!(vanilla.loader("1.21.1").unwrap(), (LoaderType::Vanilla, None));
    }

    #[test]
    fn hmcl_without_loader_is_vanilla() {
        let pack = hmcl(json!({"name": "Pack", "gameVersion": "1.21.1"}), json!({}));
        let info = pack.info().unwrap();
        assert_eq!(info.loader, LoaderType::Vanilla);
        assert_eq!(info.loader_version, None);
    }

    #[test]
    fn hmcl_rejects_quilt() {
        let pack = hmcl(
            json!({"name": "Pack", "gameVersion": "1.21.1"}),
            json!({"libraries": [{"name": "org.quiltmc:quilt-loader:0.26.0"}]}),
        );
        assert!(matches!(pack.info(), Err(HexoError::UnsupportedLoader(_))));
    }

    #[test]
    fn mcbbs_reads_addons() {
        let manifest: McbbsManifest = serde_json::from_value(json!({
            "manifestType": "minecraftModpack", "manifestVersion": 2,
            "name": "Pack", "version": "2.0",
            "addons": [{"id": "game", "version": "1.21.1"}, {"id": "neoforge", "version": "21.1.77"}],
            "files": [
                {"type": "addon", "path": "mods/a.jar", "hash": "abc", "force": true},
                {"type": "curse", "projectID": 1, "fileID": 2, "force": false}
            ],
            "launchInfo": {"minMemory": 0, "supportJava": [21]}
        }))
        .unwrap();
        let info = manifest.info().unwrap();
        assert_eq!(info.mc_version, "1.21.1");
        assert_eq!(info.loader, LoaderType::NeoForge);
        assert_eq!(info.loader_version.as_deref(), Some("21.1.77"));
        assert_eq!(manifest.skipped(), vec!["curseforge:1/2".to_string()]);
    }

    #[tokio::test]
    async fn hmcl_pack_files_skip_version_json() {
        use std::io::Write;
        use zip::write::{SimpleFileOptions, ZipWriter};

        let temp = tempfile::tempdir().unwrap();
        let pack_path = temp.path().join("pack.zip");
        let base = temp.path().join("launcher");
        {
            let mut zip = ZipWriter::new(std::fs::File::create(&pack_path).unwrap());
            let options = SimpleFileOptions::default();
            zip.start_file(HMCL_MANIFEST_FILE, options).unwrap();
            zip.write_all(json!({"name": "Pack", "gameVersion": "1.20.1"}).to_string().as_bytes())
                .unwrap();
            zip.start_file(HMCL_VERSION_FILE, options).unwrap();
            zip.write_all(
                json!({"patches": [{"id": "game", "version": "1.20.1"}, {"id": "fabric", "version": "0.15.0"}]})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
            zip.start_file("minecraft/config/a.toml", options).unwrap();
            zip.write_all(b"a = 1").unwrap();
            zip.finish().unwrap();
        }

        assert_eq!(
            super::super::detect_modpack_format(&pack_path).await.unwrap(),
            super::super::ModpackFormat::Hmcl
        );
        let result = super::super::install_modpack_files(
            &pack_path,
            "hmcl",
            &base,
            None,
            crate::no_progress(),
        )
        .await
        .unwrap();
        let game = base.join("instance/hmcl/.minecraft");
        assert_eq!(std::fs::read_to_string(game.join("config/a.toml")).unwrap(), "a = 1");
        assert!(!game.join("pack.json").exists());
        assert_eq!(result.info.loader, LoaderType::Fabric);
        assert_eq!(result.info.loader_version.as_deref(), Some("0.15.0"));
    }

    #[tokio::test]
    async fn mcbbs_with_curseforge_files_is_curseforge() {
        use std::io::Write;
        use zip::write::{SimpleFileOptions, ZipWriter};

        let temp = tempfile::tempdir().unwrap();
        let write_pack = |name: &str, cf_files: serde_json::Value| {
            let path = temp.path().join(name);
            let mut zip = ZipWriter::new(std::fs::File::create(&path).unwrap());
            let options = SimpleFileOptions::default();
            zip.start_file(MCBBS_MANIFEST_FILE, options).unwrap();
            zip.write_all(
                json!({"name": "Pack", "addons": [{"id": "game", "version": "1.20.1"}]})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
            zip.start_file(super::super::cfpack::MANIFEST_FILE, options).unwrap();
            zip.write_all(
                json!({"minecraft": {"version": "1.20.1"}, "files": cf_files})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
            zip.finish().unwrap();
            path
        };

        let with_files = write_pack("cf.zip", json!([{"projectID": 1, "fileID": 2}]));
        let without_files = write_pack("mcbbs.zip", json!([]));
        assert_eq!(
            super::super::detect_modpack_format(&with_files).await.unwrap(),
            super::super::ModpackFormat::CurseForge
        );
        assert_eq!(
            super::super::detect_modpack_format(&without_files).await.unwrap(),
            super::super::ModpackFormat::Mcbbs
        );
    }
}
