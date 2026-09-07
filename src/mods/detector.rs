use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModLoader {
    Fabric,
    Forge,
    NeoForge,
    Quilt,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModInfo {
    pub file_name: String,
    pub file_path: PathBuf,
    pub mod_id: String,
    pub name: String,
    pub version: String,
    pub loader: ModLoader,
    pub description: Option<String>,
}

/// Scan a mods directory, reading each .jar's metadata.
pub fn detect_mods(mods_dir: &Path) -> Vec<ModInfo> {
    let mut mods = Vec::new();

    let Ok(entries) = std::fs::read_dir(mods_dir) else {
        return mods;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jar") {
            continue;
        }

        if let Some(info) = read_mod_info(&path) {
            mods.push(info);
        }
    }

    mods
}

fn read_mod_info(jar_path: &Path) -> Option<ModInfo> {
    let file = std::fs::File::open(jar_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;

    let file_name = jar_path.file_name()?.to_string_lossy().to_string();

    // Fabric: fabric.mod.json
    if let Some(info) = read_fabric_mod(&mut archive, jar_path, &file_name) {
        return Some(info);
    }

    // Forge/NeoForge: META-INF/mods.toml
    if let Some(info) = read_forge_mod(&mut archive, jar_path, &file_name) {
        return Some(info);
    }

    // Old Forge: mcmod.info
    if let Some(info) = read_mcmod_info(&mut archive, jar_path, &file_name) {
        return Some(info);
    }

    // Unrecognized: return basic info.
    Some(ModInfo {
        file_name: file_name.clone(),
        file_path: jar_path.to_path_buf(),
        mod_id: file_name.clone(),
        name: file_name,
        version: "unknown".to_string(),
        loader: ModLoader::Unknown,
        description: None,
    })
}

#[derive(Deserialize)]
struct FabricModJson {
    id: String,
    version: String,
    name: Option<String>,
    description: Option<String>,
}

fn read_fabric_mod(
    archive: &mut zip::ZipArchive<std::fs::File>,
    jar_path: &Path,
    file_name: &str,
) -> Option<ModInfo> {
    let entry = archive.by_name("fabric.mod.json").ok()?;
    let fabric: FabricModJson = serde_json::from_reader(entry).ok()?;

    Some(ModInfo {
        file_name: file_name.to_string(),
        file_path: jar_path.to_path_buf(),
        mod_id: fabric.id.clone(),
        name: fabric.name.unwrap_or(fabric.id),
        version: fabric.version,
        loader: ModLoader::Fabric,
        description: fabric.description,
    })
}

fn read_forge_mod(
    archive: &mut zip::ZipArchive<std::fs::File>,
    jar_path: &Path,
    file_name: &str,
) -> Option<ModInfo> {
    use std::io::Read;
    let mut entry = archive.by_name("META-INF/mods.toml").ok()?;
    let mut content = String::new();
    entry.read_to_string(&mut content).ok()?;

    let parsed: toml::Value = toml::from_str(&content).ok()?;
    let mods_arr = parsed.get("mods")?.as_array()?;
    let first = mods_arr.first()?;

    let mod_id = first.get("modId")?.as_str()?.to_string();
    let version = first
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let name = first
        .get("displayName")
        .and_then(|v| v.as_str())
        .unwrap_or(&mod_id)
        .to_string();
    let description = first
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // NeoForge if mods.toml mentions neoforge.
    let loader = if content.contains("neoforge") {
        ModLoader::NeoForge
    } else {
        ModLoader::Forge
    };

    Some(ModInfo {
        file_name: file_name.to_string(),
        file_path: jar_path.to_path_buf(),
        mod_id,
        name,
        version,
        loader,
        description,
    })
}

#[derive(Deserialize)]
struct McModInfo {
    modid: String,
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
}

fn read_mcmod_info(
    archive: &mut zip::ZipArchive<std::fs::File>,
    jar_path: &Path,
    file_name: &str,
) -> Option<ModInfo> {
    let entry = archive.by_name("mcmod.info").ok()?;
    // mcmod.info may be an array or an object with a "modList" key.
    let val: serde_json::Value = serde_json::from_reader(entry).ok()?;

    let info: McModInfo = if val.is_array() {
        serde_json::from_value(val.as_array()?.first()?.clone()).ok()?
    } else {
        let list = val.get("modList")?.as_array()?.first()?.clone();
        serde_json::from_value(list).ok()?
    };

    Some(ModInfo {
        file_name: file_name.to_string(),
        file_path: jar_path.to_path_buf(),
        mod_id: info.modid.clone(),
        name: info.name.unwrap_or(info.modid),
        version: info.version.unwrap_or_else(|| "unknown".to_string()),
        loader: ModLoader::Forge,
        description: info.description,
    })
}
