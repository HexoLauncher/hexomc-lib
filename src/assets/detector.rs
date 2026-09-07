use std::path::{Path, PathBuf};

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
    pub name: String,
    pub path: PathBuf,
    /// Raw bytes of the world's icon.png, if present.
    pub icon: Option<Vec<u8>>,
}

/// Scan `.minecraft/resourcepacks/`. A valid pack has `pack.mcmeta` (folder) or is
/// a `.zip` with `pack.mcmeta` at its root.
pub fn detect_resource_packs(minecraft_dir: &Path) -> Vec<ResourcePack> {
    let dir = minecraft_dir.join("resourcepacks");
    scan_packs(&dir, is_resource_pack_zip, is_resource_pack_folder)
        .into_iter()
        .map(|(name, path, is_folder)| ResourcePack { name, path, is_folder })
        .collect()
}

fn is_resource_pack_zip(zip_path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(zip_path) else { return false; };
    let Ok(mut archive) = zip::ZipArchive::new(file) else { return false; };
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
        .map(|(name, path, is_folder)| ShaderPack { name, path, is_folder })
        .collect()
}

fn is_shader_zip(zip_path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(zip_path) else { return false; };
    let Ok(mut archive) = zip::ZipArchive::new(file) else { return false; };
    let result = (0..archive.len()).any(|i| {
        archive.by_index_raw(i)
            .map(|e| e.name().starts_with("shaders/"))
            .unwrap_or(false)
    });
    result
}

fn is_shader_folder(folder: &Path) -> bool {
    folder.join("shaders").is_dir()
}

/// Scan `.minecraft/saves/`. A folder containing `level.dat` is a world.
pub fn detect_maps(minecraft_dir: &Path) -> Vec<WorldMap> {
    let saves_dir = minecraft_dir.join("saves");
    let mut maps = Vec::new();

    let Ok(entries) = std::fs::read_dir(&saves_dir) else {
        return maps;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("level.dat").exists() {
            continue;
        }

        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let icon_path = path.join("icon.png");
        let icon = std::fs::read(&icon_path).ok();

        maps.push(WorldMap {
            name,
            path,
            icon,
        });
    }

    maps
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
            if path.extension().and_then(|e| e.to_str()) == Some("zip") {
                if check_zip(&path) {
                    let name = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    results.push((name, path, false));
                }
            }
        } else if path.is_dir() {
            if check_folder(&path) {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                results.push((name, path, true));
            }
        }
    }

    results
}
