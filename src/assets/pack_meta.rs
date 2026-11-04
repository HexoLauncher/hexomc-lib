//! Details of resource packs / shader packs, and which ones the game has enabled.

use serde_json::Value as Json;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Entries larger than this are not read (icons, text files).
const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct ResourcePackMeta {
    pub pack_format: Option<i64>,
    /// `supported_formats` as an inclusive range.
    pub supported_formats: Option<(i64, i64)>,
    /// Text component flattened to a string with legacy `§` formatting codes.
    pub description: Option<String>,
    /// Raw bytes of pack.png.
    pub icon: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
pub struct ShaderPackMeta {
    /// From `shaders/pack.json` (`shaderDescription`), when present.
    pub description: Option<String>,
    /// Dimension folders under `shaders/` (e.g. `world0`, `world-1`, `world1`).
    pub dimensions: Vec<String>,
    /// `profile.<NAME>` entries in `shaders/shaders.properties`, in file order.
    pub profiles: Vec<String>,
}

/// A pack is either a folder or a zip archive.
enum Source {
    Folder(std::path::PathBuf),
    Zip(zip::ZipArchive<File>),
}

impl Source {
    fn open(path: &Path) -> Option<Self> {
        if path.is_dir() {
            return Some(Source::Folder(path.to_path_buf()));
        }
        let archive = zip::ZipArchive::new(File::open(path).ok()?).ok()?;
        Some(Source::Zip(archive))
    }

    fn read(&mut self, name: &str) -> Option<Vec<u8>> {
        match self {
            Source::Folder(root) => {
                let file = root.join(name);
                if std::fs::metadata(&file).ok()?.len() > MAX_ENTRY_BYTES {
                    return None;
                }
                std::fs::read(file).ok()
            }
            Source::Zip(archive) => {
                let mut entry = archive.by_name(name).ok()?;
                if entry.size() > MAX_ENTRY_BYTES {
                    return None;
                }
                let mut bytes = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut bytes).ok()?;
                Some(bytes)
            }
        }
    }

    fn read_text(&mut self, name: &str) -> Option<String> {
        let bytes = self.read(name)?;
        let text = String::from_utf8_lossy(&bytes);
        Some(text.trim_start_matches('\u{feff}').to_string())
    }

    fn has_folder(&mut self, dir: &str) -> bool {
        match self {
            Source::Folder(root) => root.join(dir).is_dir(),
            Source::Zip(archive) => {
                let prefix = format!("{}/", dir);
                archive.file_names().any(|name| name.starts_with(&prefix))
            }
        }
    }

    /// Names of the direct sub-folders of `dir` (no trailing slash in `dir`).
    fn sub_folders(&mut self, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = match self {
            Source::Folder(root) => std::fs::read_dir(root.join(dir))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect(),
            Source::Zip(archive) => {
                let prefix = format!("{}/", dir);
                archive
                    .file_names()
                    .filter_map(|name| {
                        let rest = name.strip_prefix(&prefix)?;
                        let (folder, _) = rest.split_once('/')?;
                        (!folder.is_empty()).then(|| folder.to_string())
                    })
                    .collect()
            }
        };
        names.sort();
        names.dedup();
        names
    }
}

/// Read `pack.mcmeta` and `pack.png` of a resource pack (folder or .zip).
pub fn read_resource_pack_meta(path: &Path) -> Option<ResourcePackMeta> {
    let mut source = Source::open(path)?;
    let mcmeta = source.read_text("pack.mcmeta")?;
    let json: Json = serde_json::from_str(&mcmeta).ok().or_else(|| {
        // Raw newlines inside strings are common.
        let relaxed: String = mcmeta
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        serde_json::from_str(&relaxed).ok()
    })?;
    let pack = json.get("pack")?;

    let supported_formats = match pack.get("supported_formats") {
        Some(Json::Number(n)) => n.as_i64().map(|v| (v, v)),
        Some(Json::Array(range)) => match range.as_slice() {
            [min, max] => min.as_i64().zip(max.as_i64()),
            _ => None,
        },
        Some(obj @ Json::Object(_)) => obj
            .get("min_inclusive")
            .and_then(Json::as_i64)
            .zip(obj.get("max_inclusive").and_then(Json::as_i64)),
        _ => None,
    };

    let mut description = String::new();
    if let Some(desc) = pack.get("description") {
        flatten_text(desc, "", &mut description);
    }

    Some(ResourcePackMeta {
        pack_format: pack.get("pack_format").and_then(Json::as_i64),
        supported_formats,
        description: Some(description.trim().to_string()).filter(|d| !d.is_empty()),
        icon: source.read("pack.png"),
    })
}

fn color_code(name: &str) -> Option<char> {
    Some(match name {
        "black" => '0',
        "dark_blue" => '1',
        "dark_green" => '2',
        "dark_aqua" => '3',
        "dark_red" => '4',
        "dark_purple" => '5',
        "gold" => '6',
        "gray" => '7',
        "dark_gray" => '8',
        "blue" => '9',
        "green" => 'a',
        "aqua" => 'b',
        "red" => 'c',
        "light_purple" => 'd',
        "yellow" => 'e',
        "white" => 'f',
        _ => return None,
    })
}

/// Flatten a text component into a `§`-coded string. Children inherit the
/// parent's style, like the game does.
fn flatten_text(value: &Json, inherited: &str, out: &mut String) {
    match value {
        Json::String(_) | Json::Number(_) | Json::Bool(_) => {
            if !inherited.is_empty() {
                out.push_str("§r");
                out.push_str(inherited);
            }
            match value {
                Json::String(s) => out.push_str(s),
                other => out.push_str(&other.to_string()),
            }
        }
        Json::Array(items) => {
            // The first element is the parent of the rest.
            let Some((first, rest)) = items.split_first() else {
                return;
            };
            let style = format!("{}{}", inherited, own_style(first));
            flatten_text(first, inherited, out);
            for item in rest {
                flatten_text(item, &style, out);
            }
        }
        Json::Object(obj) => {
            let style = format!("{}{}", inherited, own_style(value));
            let text = obj
                .get("text")
                .and_then(Json::as_str)
                .or_else(|| obj.get("translate").and_then(Json::as_str))
                .unwrap_or_default();
            if !text.is_empty() {
                out.push_str("§r");
                out.push_str(&style);
                out.push_str(text);
            }
            for child in obj.get("extra").and_then(Json::as_array).into_iter().flatten() {
                out.push_str("§r");
                flatten_text(child, &style, out);
            }
        }
        Json::Null => {}
    }
}

fn own_style(value: &Json) -> String {
    let mut style = String::new();
    let Json::Object(obj) = value else {
        return style;
    };
    if let Some(code) = obj.get("color").and_then(Json::as_str).and_then(color_code) {
        style.push('§');
        style.push(code);
    }
    for (key, code) in [
        ("obfuscated", 'k'),
        ("bold", 'l'),
        ("strikethrough", 'm'),
        ("underlined", 'n'),
        ("italic", 'o'),
    ] {
        if obj.get(key).and_then(Json::as_bool) == Some(true) {
            style.push('§');
            style.push(code);
        }
    }
    style
}

/// Read what a shader pack (folder or .zip) tells about itself.
/// Returns `None` when it has no `shaders/` folder.
pub fn read_shader_pack_meta(path: &Path) -> Option<ShaderPackMeta> {
    let mut source = Source::open(path)?;
    if !source.has_folder("shaders") {
        return None;
    }

    let dimensions: Vec<String> = source
        .sub_folders("shaders")
        .into_iter()
        .filter(|name| name.starts_with("world"))
        .collect();
    let properties = source.read_text("shaders/shaders.properties");
    let pack_json = source
        .read_text("shaders/pack.json")
        .and_then(|text| serde_json::from_str::<Json>(&text).ok());

    let mut profiles = Vec::new();
    for line in properties.iter().flat_map(|p| p.lines()) {
        let Some(rest) = line.trim_start().strip_prefix("profile.") else {
            continue;
        };
        let Some((name, _)) = rest.split_once('=') else {
            continue;
        };
        let name = name.trim().to_string();
        if !name.is_empty() && !profiles.contains(&name) {
            profiles.push(name);
        }
    }

    Some(ShaderPackMeta {
        description: pack_json
            .as_ref()
            .and_then(|j| j.get("shaderDescription"))
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string),
        dimensions,
        profiles,
    })
}

/// File names of the resource packs enabled in `options.txt`, in load order.
pub fn active_resource_packs(minecraft_dir: &Path) -> Vec<String> {
    let Ok(options) = std::fs::read_to_string(minecraft_dir.join("options.txt")) else {
        return Vec::new();
    };
    options
        .lines()
        .find_map(|line| line.strip_prefix("resourcePacks:"))
        .and_then(|list| serde_json::from_str::<Vec<String>>(list).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| id.strip_prefix("file/").map(str::to_string))
        .collect()
}

/// The shader pack currently selected in Iris (`config/iris.properties`) or
/// OptiFine (`optionsshaders.txt`), if shaders are enabled.
pub fn active_shader_pack(minecraft_dir: &Path) -> Option<String> {
    if let Ok(iris) = std::fs::read_to_string(minecraft_dir.join("config/iris.properties")) {
        if property(&iris, "enableShaders").as_deref() == Some("false") {
            return None;
        }
        return property(&iris, "shaderPack").filter(|p| !p.is_empty());
    }
    let optifine = std::fs::read_to_string(minecraft_dir.join("optionsshaders.txt")).ok()?;
    property(&optifine, "shaderPack").filter(|p| !p.is_empty() && p != "OFF" && p != "(internal)")
}

/// Value of `key` in a Java .properties file, with `\` escapes removed.
fn property(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim_start();
        if line.starts_with('#') || line.starts_with('!') {
            return None;
        }
        let rest = line.strip_prefix(key)?.trim_start();
        let value = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':'))?;
        let mut out = String::new();
        let mut chars = value.trim().chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(c);
            }
        }
        Some(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn zip(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        for (entry, data) in files {
            zip.start_file(*entry, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn resource_pack_plain_description() {
        let tmp = TempDir::new().unwrap();
        let path = zip(
            tmp.path(),
            "a.zip",
            &[
                (
                    "pack.mcmeta",
                    br#"{"pack": {"pack_format": 15, "supported_formats": {"min_inclusive": 15, "max_inclusive": 99},
                        "description": "\u00a7fCreate Style\n\u00a78by me"}}"#,
                ),
                ("pack.png", b"\x89PNG"),
            ],
        );
        let meta = read_resource_pack_meta(&path).unwrap();
        assert_eq!(meta.pack_format, Some(15));
        assert_eq!(meta.supported_formats, Some((15, 99)));
        assert_eq!(meta.description.as_deref(), Some("§fCreate Style\n§8by me"));
        assert_eq!(meta.icon.as_deref(), Some(&b"\x89PNG"[..]));
    }

    #[test]
    fn resource_pack_component_description() {
        let tmp = TempDir::new().unwrap();
        let path = zip(
            tmp.path(),
            "a.zip",
            &[(
                "pack.mcmeta",
                br#"{"pack": {"pack_format": 10, "supported_formats": [10, 12],
                    "description": [{"text": "Gold", "color": "gold", "bold": true}, {"text": " plain"}]}}"#,
            )],
        );
        let meta = read_resource_pack_meta(&path).unwrap();
        assert_eq!(meta.supported_formats, Some((10, 12)));
        assert_eq!(meta.description.as_deref(), Some("§r§6§lGold§r§6§l plain"));
        assert!(meta.icon.is_none());
    }

    #[test]
    fn resource_pack_folder() {
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("Folder");
        fs::create_dir_all(&pack).unwrap();
        fs::write(pack.join("pack.mcmeta"), br#"{"pack": {"pack_format": 4, "description": {"text": "Hi"}}}"#).unwrap();
        let meta = read_resource_pack_meta(&pack).unwrap();
        assert_eq!(meta.pack_format, Some(4));
        assert_eq!(meta.description.as_deref(), Some("§rHi"));
    }

    #[test]
    fn shader_pack_zip() {
        let tmp = TempDir::new().unwrap();
        let path = zip(
            tmp.path(),
            "s.zip",
            &[
                ("shaders/world0/composite.fsh", b""),
                ("shaders/world-1/composite.fsh", b""),
                ("shaders/lib/common.glsl", b""),
                (
                    "shaders/shaders.properties",
                    b"  profile.LOW = a=1\nprofile.HIGH=a=2\n#profile.NO=1\n",
                ),
                ("shaders/pack.json", br#"{"shaderDescription": "Nice shaders"}"#),
            ],
        );
        let meta = read_shader_pack_meta(&path).unwrap();
        assert_eq!(meta.dimensions, ["world-1", "world0"]);
        assert_eq!(meta.profiles, ["LOW", "HIGH"]);
        assert_eq!(meta.description.as_deref(), Some("Nice shaders"));
    }

    #[test]
    fn not_a_shader_pack() {
        let tmp = TempDir::new().unwrap();
        let path = zip(tmp.path(), "x.zip", &[("readme.txt", b"")]);
        assert!(read_shader_pack_meta(&path).is_none());
    }

    #[test]
    fn active_packs() {
        let tmp = TempDir::new().unwrap();
        let mc = tmp.path();
        fs::write(
            mc.join("options.txt"),
            "version:3465\nresourcePacks:[\"vanilla\",\"file/A.zip\",\"fabric\",\"file/B Folder\"]\n",
        )
        .unwrap();
        assert_eq!(active_resource_packs(mc), ["A.zip", "B Folder"]);

        fs::write(mc.join("optionsshaders.txt"), "shaderPack=OFF\n").unwrap();
        assert_eq!(active_shader_pack(mc), None);
        fs::write(mc.join("optionsshaders.txt"), "shaderPack=BSL.zip\n").unwrap();
        assert_eq!(active_shader_pack(mc).as_deref(), Some("BSL.zip"));

        // Iris wins over OptiFine.
        fs::create_dir_all(mc.join("config")).unwrap();
        fs::write(
            mc.join("config/iris.properties"),
            "#comment\nshaderPack=Complementary\\ + Patch\nenableShaders=true\n",
        )
        .unwrap();
        assert_eq!(active_shader_pack(mc).as_deref(), Some("Complementary + Patch"));
        fs::write(mc.join("config/iris.properties"), "shaderPack=X.zip\nenableShaders=false\n").unwrap();
        assert_eq!(active_shader_pack(mc), None);
    }
}
