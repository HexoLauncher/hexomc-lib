use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::assets::nbt::{self, Nbt};


#[derive(Debug, Clone)]
pub struct ResourcePack {
    pub name: String,
    pub path: PathBuf,
    /// true = folder, false = .zip
    pub is_folder: bool,
}

#[derive(Debug, Clone)]
pub struct ShaderPack {
    pub name: String,
    pub path: PathBuf,
    pub is_folder: bool,
}

#[derive(Debug, Clone)]
pub struct WorldMap {
    /// `LevelName` from `level.dat`, or the folder name.
    pub name: String,
    /// Folder name under `saves/`.
    pub folder: String,
    pub path: PathBuf,
    /// Raw bytes of the world's icon.png, if present.
    pub icon: Option<Vec<u8>>,
    pub game_mode: Option<GameMode>,
    pub hardcore: bool,
    pub cheats: bool,
    /// Minecraft version that last saved the world.
    pub version: Option<String>,
    /// Unix seconds; 0 if unknown.
    pub last_played: u64,
    /// Size of the world folder in bytes.
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
    Unknown,
}

impl GameMode {
    pub fn from_id(id: i64) -> Self {
        match id {
            0 => GameMode::Survival,
            1 => GameMode::Creative,
            2 => GameMode::Adventure,
            3 => GameMode::Spectator,
            _ => GameMode::Unknown,
        }
    }
}

/// One entry of the multiplayer server list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerEntry {
    pub name: String,
    pub address: String,
    /// Base64-encoded PNG, as stored by the game.
    pub icon: Option<String>,
    /// Whether server resource packs are accepted.
    pub accept_textures: bool,
}

/// Scan `.minecraft/resourcepacks/`. A valid pack has `pack.mcmeta` (folder) or is
/// a `.zip` with `pack.mcmeta` at its root.
pub fn detect_resource_packs(minecraft_dir: &Path) -> Vec<ResourcePack> {
    let dir = minecraft_dir.join("resourcepacks");
    scan_packs(&dir, is_resource_pack_zip, is_resource_pack_folder)
        .into_iter()
        .map(|(name, path, is_folder)| ResourcePack {
            name,
            path,
            is_folder,
        })
        .collect()
}

fn is_resource_pack_zip(zip_path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(zip_path) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    let result = archive.by_name("pack.mcmeta").is_ok();
    result
}

fn is_resource_pack_folder(folder: &Path) -> bool {
    folder.join("pack.mcmeta").exists()
}

/// Scan `.minecraft/shaderpacks/`. A valid pack has a `shaders/` directory
/// (folder) or a `shaders/` folder inside the zip.
pub fn detect_shader_packs(minecraft_dir: &Path) -> Vec<ShaderPack> {
    let dir = minecraft_dir.join("shaderpacks");
    scan_packs(&dir, is_shader_zip, is_shader_folder)
        .into_iter()
        .map(|(name, path, is_folder)| ShaderPack {
            name,
            path,
            is_folder,
        })
        .collect()
}

fn is_shader_zip(zip_path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(zip_path) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    let result = (0..archive.len()).any(|i| {
        archive
            .by_index_raw(i)
            .map(|e| e.name().starts_with("shaders/"))
            .unwrap_or(false)
    });
    result
}

fn is_shader_folder(folder: &Path) -> bool {
    folder.join("shaders").is_dir()
}

/// Scan `.minecraft/saves/`. A folder containing `level.dat` is a world. Worlds are
/// sorted by last played, newest first.
pub fn detect_maps(minecraft_dir: &Path) -> Vec<WorldMap> {
    let Ok(entries) = std::fs::read_dir(minecraft_dir.join("saves")) else {
        return Vec::new();
    };
    let mut maps: Vec<WorldMap> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| read_world(&path))
        .collect();
    maps.sort_by(|a, b| b.last_played.cmp(&a.last_played));
    maps
}

/// Read one world folder. Returns `None` without a `level.dat`; unreadable NBT still
/// yields a world named after its folder.
pub fn read_world(path: &Path) -> Option<WorldMap> {
    let bytes = std::fs::read(path.join("level.dat")).ok()?;
    let folder = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let root = nbt::parse(&bytes);
    let data = root.as_ref().and_then(|r| r.get("Data"));
    let get = |key: &str| data.and_then(|d| d.get(key));

    Some(WorldMap {
        name: get("LevelName")
            .and_then(Nbt::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&folder)
            .to_string(),
        path: path.to_path_buf(),
        icon: std::fs::read(path.join("icon.png")).ok(),
        game_mode: get("GameType").and_then(Nbt::as_int).map(GameMode::from_id),
        hardcore: get("hardcore").and_then(Nbt::as_int).unwrap_or_default() != 0,
        cheats: get("allowCommands").and_then(Nbt::as_int).unwrap_or_default() != 0,
        version: get("Version")
            .and_then(|v| v.get("Name"))
            .and_then(Nbt::as_str)
            .map(str::to_string),
        last_played: get("LastPlayed")
            .and_then(Nbt::as_int)
            .map(|ms| (ms / 1000).max(0) as u64)
            .unwrap_or_default(),
        size: dir_size(path),
        folder,
    })
}

/// Total size in bytes of the files under `dir`.
pub fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => dir_size(&entry.path()),
            Ok(t) if t.is_file() => entry.metadata().map(|m| m.len()).unwrap_or_default(),
            _ => 0,
        })
        .sum()
}

/// Read the multiplayer server list from `servers.dat`.
pub fn detect_servers(minecraft_dir: &Path) -> Vec<ServerEntry> {
    let Ok(bytes) = std::fs::read(minecraft_dir.join("servers.dat")) else {
        return Vec::new();
    };
    let Some(root) = nbt::parse(&bytes) else {
        return Vec::new();
    };
    let Some(servers) = root.get("servers").and_then(Nbt::as_list) else {
        return Vec::new();
    };
    servers
        .iter()
        .map(|entry| {
            let text = |key: &str| entry.get(key).and_then(Nbt::as_str).unwrap_or_default();
            ServerEntry {
                name: text("name").to_string(),
                address: text("ip").to_string(),
                icon: Some(text("icon").to_string()).filter(|s| !s.is_empty()),
                accept_textures: entry
                    .get("acceptTextures")
                    .and_then(Nbt::as_int)
                    .is_some_and(|v| v == 1),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::{FileOptions, ZipWriter};

    fn make_zip_with_file(dir: &std::path::Path, zip_name: &str, inner_file: &str) -> PathBuf {
        let zip_path = dir.join(zip_name);
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let opts = FileOptions::<()>::default();
        zip.start_file(inner_file, opts).unwrap();
        zip.write_all(b"{}").unwrap();
        zip.finish().unwrap();
        zip_path
    }

    #[test]
    fn detect_resource_pack_zip() {
        let tmp = TempDir::new().unwrap();
        let mc_dir = tmp.path();
        let rp_dir = mc_dir.join("resourcepacks");
        fs::create_dir_all(&rp_dir).unwrap();
        make_zip_with_file(&rp_dir, "MyPack.zip", "pack.mcmeta");

        let packs = detect_resource_packs(mc_dir);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].name, "MyPack");
        assert!(!packs[0].is_folder);
    }

    #[test]
    fn detect_resource_pack_folder() {
        let tmp = TempDir::new().unwrap();
        let mc_dir = tmp.path();
        let rp_dir = mc_dir.join("resourcepacks").join("FolderPack");
        fs::create_dir_all(&rp_dir).unwrap();
        fs::write(rp_dir.join("pack.mcmeta"), b"{}").unwrap();

        let packs = detect_resource_packs(mc_dir);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].name, "FolderPack");
        assert!(packs[0].is_folder);
    }

    #[test]
    fn invalid_zip_ignored() {
        let tmp = TempDir::new().unwrap();
        let mc_dir = tmp.path();
        let rp_dir = mc_dir.join("resourcepacks");
        fs::create_dir_all(&rp_dir).unwrap();
        make_zip_with_file(&rp_dir, "NotAPack.zip", "some_other_file.txt");

        let packs = detect_resource_packs(mc_dir);
        assert!(packs.is_empty());
    }

    #[test]
    fn detect_shader_pack_folder() {
        let tmp = TempDir::new().unwrap();
        let mc_dir = tmp.path();
        let sp_dir = mc_dir.join("shaderpacks").join("MyShader");
        fs::create_dir_all(sp_dir.join("shaders")).unwrap();

        let shaders = detect_shader_packs(mc_dir);
        assert_eq!(shaders.len(), 1);
        assert_eq!(shaders[0].name, "MyShader");
    }

    #[test]
    fn detect_shader_pack_zip() {
        let tmp = TempDir::new().unwrap();
        let mc_dir = tmp.path();
        let sp_dir = mc_dir.join("shaderpacks");
        fs::create_dir_all(&sp_dir).unwrap();
        make_zip_with_file(&sp_dir, "CoolShader.zip", "shaders/final.fsh");

        let shaders = detect_shader_packs(mc_dir);
        assert_eq!(shaders.len(), 1);
        assert_eq!(shaders[0].name, "CoolShader");
    }

    #[test]
    fn detect_world_map() {
        let tmp = TempDir::new().unwrap();
        let mc_dir = tmp.path();
        let world_dir = mc_dir.join("saves").join("My World");
        fs::create_dir_all(&world_dir).unwrap();
        fs::write(world_dir.join("level.dat"), b"\x1f\x8b").unwrap();

        let maps = detect_maps(mc_dir);
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].name, "My World");
        assert!(maps[0].icon.is_none());
    }

    #[test]
    fn world_with_icon() {
        let tmp = TempDir::new().unwrap();
        let mc_dir = tmp.path();
        let world_dir = mc_dir.join("saves").join("IconWorld");
        fs::create_dir_all(&world_dir).unwrap();
        fs::write(world_dir.join("level.dat"), b"data").unwrap();
        fs::write(world_dir.join("icon.png"), b"\x89PNG").unwrap();

        let maps = detect_maps(mc_dir);
        assert_eq!(maps.len(), 1);
        assert!(maps[0].icon.is_some());
        assert_eq!(maps[0].icon.as_deref(), Some(b"\x89PNG".as_ref()));
    }

    fn tag(out: &mut Vec<u8>, id: u8, key: &str) {
        out.push(id);
        out.extend((key.len() as u16).to_be_bytes());
        out.extend(key.as_bytes());
    }

    fn string(out: &mut Vec<u8>, value: &str) {
        out.extend((value.len() as u16).to_be_bytes());
        out.extend(value.as_bytes());
    }

    #[test]
    fn read_world_parses_level_dat() {
        let tmp = TempDir::new().unwrap();
        let world = tmp.path().join("New World (1)");
        fs::create_dir_all(&world).unwrap();

        let mut dat = Vec::new();
        tag(&mut dat, 10, "");
        tag(&mut dat, 10, "Data");
        tag(&mut dat, 8, "LevelName");
        string(&mut dat, "Hello");
        tag(&mut dat, 3, "GameType");
        dat.extend(1i32.to_be_bytes());
        tag(&mut dat, 1, "hardcore");
        dat.push(1);
        tag(&mut dat, 4, "LastPlayed");
        dat.extend(1_700_000_000_123i64.to_be_bytes());
        tag(&mut dat, 10, "Version");
        tag(&mut dat, 8, "Name");
        string(&mut dat, "1.21.4");
        dat.extend([0, 0, 0]);
        fs::write(world.join("level.dat"), &dat).unwrap();

        let info = read_world(&world).unwrap();
        assert_eq!(info.name, "Hello");
        assert_eq!(info.folder, "New World (1)");
        assert_eq!(info.game_mode, Some(GameMode::Creative));
        assert!(info.hardcore);
        assert!(!info.cheats);
        assert_eq!(info.version.as_deref(), Some("1.21.4"));
        assert_eq!(info.last_played, 1_700_000_000);
        assert_eq!(info.size, dat.len() as u64);
    }

    #[test]
    fn servers_dat_is_read() {
        let tmp = TempDir::new().unwrap();
        let mut dat = Vec::new();
        tag(&mut dat, 10, "");
        tag(&mut dat, 9, "servers");
        dat.push(10);
        dat.extend(1i32.to_be_bytes());
        tag(&mut dat, 8, "name");
        string(&mut dat, "Hub");
        tag(&mut dat, 8, "ip");
        string(&mut dat, "mc.example.org");
        tag(&mut dat, 1, "acceptTextures");
        dat.push(1);
        dat.extend([0, 0]);
        fs::write(tmp.path().join("servers.dat"), &dat).unwrap();

        let servers = detect_servers(tmp.path());
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "Hub");
        assert_eq!(servers[0].address, "mc.example.org");
        assert!(servers[0].icon.is_none());
        assert!(servers[0].accept_textures);
        assert!(detect_servers(&tmp.path().join("missing")).is_empty());
    }

    #[test]
    fn empty_saves_dir() {
        let tmp = TempDir::new().unwrap();
        let mc_dir = tmp.path();
        fs::create_dir_all(mc_dir.join("saves")).unwrap();
        assert!(detect_maps(mc_dir).is_empty());
    }
}

/// Generic scan returning (name, path, is_folder).
fn scan_packs(
    dir: &Path,
    check_zip: impl Fn(&Path) -> bool,
    check_folder: impl Fn(&Path) -> bool,
) -> Vec<(String, PathBuf, bool)> {
    let mut results = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return results;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("zip")
                && check_zip(&path) {
                    let name = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    results.push((name, path, false));
                }
        } else if path.is_dir()
            && check_folder(&path) {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                results.push((name, path, true));
            }
    }

    results
}
