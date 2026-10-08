use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use toml::Value as Toml;

type Jar = zip::ZipArchive<File>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModLoader {
    Fabric,
    Forge,
    NeoForge,
    Quilt,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyKind {
    Required,
    Optional,
    Incompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModDependency {
    pub mod_id: String,
    /// Version requirement as written by the mod (e.g. `>=0.15`, `[1.20,)`).
    pub version: Option<String>,
    pub kind: DependencyKind,
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
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub sources: Option<String>,
    #[serde(default)]
    pub issues: Option<String>,
    /// Path of the icon inside the jar; read it with [`read_mod_icon`].
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<ModDependency>,
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

/// Read a single mod jar. Any file name works (e.g. `foo.jar.disabled`);
/// returns basic info from the file name when the jar has no known metadata,
/// and `None` when it is not a zip at all.
pub fn read_mod_info(jar_path: &Path) -> Option<ModInfo> {
    let mut archive = zip::ZipArchive::new(File::open(jar_path).ok()?).ok()?;
    let file_name = jar_path.file_name()?.to_string_lossy().to_string();

    let meta = read_fabric(&mut archive)
        .or_else(|| read_quilt(&mut archive))
        // NeoForge 1.20.5+
        .or_else(|| read_mods_toml(&mut archive, "META-INF/neoforge.mods.toml", ModLoader::NeoForge))
        .or_else(|| read_mods_toml(&mut archive, "META-INF/mods.toml", ModLoader::Forge))
        // Old Forge
        .or_else(|| read_mcmod_info(&mut archive));

    Some(match meta {
        Some(meta) => meta.into_info(file_name, jar_path),
        // Unrecognized: return basic info.
        None => ModInfo {
            file_name: file_name.clone(),
            file_path: jar_path.to_path_buf(),
            mod_id: file_name.clone(),
            name: file_name,
            version: "unknown".to_string(),
            loader: ModLoader::Unknown,
            description: None,
            authors: Vec::new(),
            license: None,
            homepage: None,
            sources: None,
            issues: None,
            icon: None,
            dependencies: Vec::new(),
        },
    })
}

/// Read the raw bytes of a file inside a mod jar, typically [`ModInfo::icon`].
/// Entries larger than `max_bytes` are skipped.
pub fn read_mod_icon(jar_path: &Path, icon: &str, max_bytes: u64) -> Option<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(File::open(jar_path).ok()?).ok()?;
    let mut entry = archive.by_name(icon.trim_start_matches('/')).ok()?;
    if entry.size() > max_bytes {
        return None;
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Everything except the file name / path.
struct Meta {
    mod_id: String,
    name: String,
    version: String,
    loader: ModLoader,
    description: Option<String>,
    authors: Vec<String>,
    license: Option<String>,
    homepage: Option<String>,
    sources: Option<String>,
    issues: Option<String>,
    icon: Option<String>,
    dependencies: Vec<ModDependency>,
}

impl Meta {
    fn into_info(self, file_name: String, jar_path: &Path) -> ModInfo {
        ModInfo {
            file_name,
            file_path: jar_path.to_path_buf(),
            mod_id: self.mod_id,
            name: self.name,
            version: self.version,
            loader: self.loader,
            description: self.description,
            authors: self.authors,
            license: self.license,
            homepage: self.homepage,
            sources: self.sources,
            issues: self.issues,
            icon: self.icon,
            dependencies: self.dependencies,
        }
    }
}

fn read_text(archive: &mut Jar, name: &str) -> Option<String> {
    let mut entry = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(text.trim_start_matches('\u{feff}').to_string())
}

/// Many mods put raw newlines inside json strings; retry with control
/// characters replaced when strict parsing fails.
fn read_json(archive: &mut Jar, name: &str) -> Option<Json> {
    let text = read_text(archive, name)?;
    serde_json::from_str(&text).ok().or_else(|| {
        let relaxed: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        serde_json::from_str(&relaxed).ok()
    })
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn json_str(value: &Json, key: &str) -> Option<String> {
    non_empty(value.get(key)?.as_str())
}

/// A fabric "person" is either a string or `{ name, contact }`.
fn json_people(value: Option<&Json>) -> Vec<String> {
    let Some(Json::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| match p {
            Json::String(s) => non_empty(Some(s)),
            other => json_str(other, "name"),
        })
        .collect()
}

fn json_license(value: Option<&Json>) -> Option<String> {
    match value? {
        Json::String(s) => non_empty(Some(s)),
        Json::Array(items) => {
            let names: Vec<String> = items
                .iter()
                .filter_map(|l| json_license(Some(l)))
                .collect();
            non_empty(Some(&names.join(", ")))
        }
        // quilt: { name, id, url }
        other => json_str(other, "name").or_else(|| json_str(other, "id")),
    }
}

/// `icon` is a path or `{ "16": "...", "128": "..." }`; pick the largest.
fn json_icon(value: Option<&Json>) -> Option<String> {
    match value? {
        Json::String(s) => non_empty(Some(s)),
        Json::Object(sizes) => sizes
            .iter()
            .filter_map(|(size, path)| Some((size.parse::<u32>().unwrap_or(0), path.as_str()?)))
            .max_by_key(|(size, _)| *size)
            .and_then(|(_, path)| non_empty(Some(path))),
        _ => None,
    }
}

fn json_version_req(value: &Json) -> Option<String> {
    match value {
        Json::String(s) => non_empty(Some(s)),
        Json::Array(items) => {
            let parts: Vec<&str> = items.iter().filter_map(Json::as_str).collect();
            non_empty(Some(&parts.join(" || ")))
        }
        _ => None,
    }
}

fn read_fabric(archive: &mut Jar) -> Option<Meta> {
    let json = read_json(archive, "fabric.mod.json")?;
    let mod_id = json_str(&json, "id")?;
    let contact = json.get("contact");

    let mut dependencies = Vec::new();
    for (key, kind) in [
        ("depends", DependencyKind::Required),
        ("recommends", DependencyKind::Optional),
        ("suggests", DependencyKind::Optional),
        ("breaks", DependencyKind::Incompatible),
        ("conflicts", DependencyKind::Incompatible),
    ] {
        if let Some(Json::Object(map)) = json.get(key) {
            for (id, req) in map {
                dependencies.push(ModDependency {
                    mod_id: id.clone(),
                    version: json_version_req(req),
                    kind,
                });
            }
        }
    }

    let mut authors = json_people(json.get("authors"));
    authors.extend(json_people(json.get("contributors")));

    Some(Meta {
        name: json_str(&json, "name").unwrap_or_else(|| mod_id.clone()),
        version: json_str(&json, "version").unwrap_or_else(|| "unknown".to_string()),
        loader: ModLoader::Fabric,
        description: json_str(&json, "description"),
        authors,
        license: json_license(json.get("license")),
        homepage: contact.and_then(|c| json_str(c, "homepage")),
        sources: contact.and_then(|c| json_str(c, "sources")),
        issues: contact.and_then(|c| json_str(c, "issues")),
        icon: json_icon(json.get("icon")),
        dependencies,
        mod_id,
    })
}

fn read_quilt(archive: &mut Jar) -> Option<Meta> {
    let json = read_json(archive, "quilt.mod.json")?;
    let loader = json.get("quilt_loader")?;
    let mod_id = json_str(loader, "id")?;
    let meta = loader.get("metadata");
    let contact = meta.and_then(|m| m.get("contact"));

    // contributors: { "name": "role" }
    let authors = match meta.and_then(|m| m.get("contributors")) {
        Some(Json::Object(map)) => map.keys().cloned().collect(),
        other => json_people(other),
    };

    let mut dependencies = Vec::new();
    for (key, kind) in [
        ("depends", DependencyKind::Required),
        ("breaks", DependencyKind::Incompatible),
    ] {
        let Some(Json::Array(items)) = loader.get(key) else {
            continue;
        };
        for item in items {
            let dep = match item {
                Json::String(id) => ModDependency {
                    mod_id: id.clone(),
                    version: None,
                    kind,
                },
                obj => {
                    let Some(id) = json_str(obj, "id") else {
                        continue;
                    };
                    let optional = obj.get("optional").and_then(Json::as_bool) == Some(true);
                    ModDependency {
                        mod_id: id,
                        version: obj.get("versions").and_then(json_version_req),
                        kind: if optional && kind == DependencyKind::Required {
                            DependencyKind::Optional
                        } else {
                            kind
                        },
                    }
                }
            };
            dependencies.push(dep);
        }
    }

    Some(Meta {
        name: meta
            .and_then(|m| json_str(m, "name"))
            .unwrap_or_else(|| mod_id.clone()),
        version: json_str(loader, "version").unwrap_or_else(|| "unknown".to_string()),
        loader: ModLoader::Quilt,
        description: meta.and_then(|m| json_str(m, "description")),
        authors,
        license: json_license(meta.and_then(|m| m.get("license"))),
        homepage: contact.and_then(|c| json_str(c, "homepage")),
        sources: contact.and_then(|c| json_str(c, "sources")),
        issues: contact.and_then(|c| json_str(c, "issues")),
        icon: json_icon(meta.and_then(|m| m.get("icon"))),
        dependencies,
        mod_id,
    })
}

fn toml_str(value: &Toml, key: &str) -> Option<String> {
    non_empty(value.get(key)?.as_str())
}

/// `${file.jarVersion}` lives in MANIFEST.MF as Implementation-Version.
fn manifest_version(archive: &mut Jar) -> Option<String> {
    let manifest = read_text(archive, "META-INF/MANIFEST.MF")?;
    manifest.lines().find_map(|line| {
        let value = line.strip_prefix("Implementation-Version:")?;
        non_empty(Some(value))
    })
}

fn read_mods_toml(archive: &mut Jar, file: &str, default_loader: ModLoader) -> Option<Meta> {
    let content = read_text(archive, file)?;
    let parsed: Toml = toml::from_str(&content).ok()?;
    let first = parsed.get("mods")?.as_array()?.first()?;
    let mod_id = toml_str(first, "modId")?;

    let mut version = toml_str(first, "version").unwrap_or_else(|| "unknown".to_string());
    if version.contains("${file.jarVersion}") {
        version = match manifest_version(archive) {
            Some(real) => version.replace("${file.jarVersion}", &real),
            None => "unknown".to_string(),
        };
    }

    let dependencies: Vec<ModDependency> = parsed
        .get("dependencies")
        .and_then(|d| d.get(&mod_id))
        .and_then(Toml::as_array)
        .into_iter()
        .flatten()
        .filter_map(|dep| {
            // Forge uses `mandatory`, NeoForge uses `type`.
            let kind = match dep.get("type").and_then(Toml::as_str) {
                Some(t) if t.eq_ignore_ascii_case("required") => DependencyKind::Required,
                Some(t)
                    if t.eq_ignore_ascii_case("incompatible")
                        || t.eq_ignore_ascii_case("discouraged") =>
                {
                    DependencyKind::Incompatible
                }
                Some(_) => DependencyKind::Optional,
                None if dep.get("mandatory").and_then(Toml::as_bool) == Some(false) => {
                    DependencyKind::Optional
                }
                None => DependencyKind::Required,
            };
            Some(ModDependency {
                mod_id: toml_str(dep, "modId")?,
                version: toml_str(dep, "versionRange"),
                kind,
            })
        })
        .collect();

    // Older NeoForge versions still use META-INF/mods.toml.
    let loader = if default_loader == ModLoader::NeoForge
        || dependencies.iter().any(|d| d.mod_id == "neoforge")
    {
        ModLoader::NeoForge
    } else {
        default_loader
    };

    let authors = toml_str(first, "authors")
        .into_iter()
        .chain(toml_str(first, "credits"))
        .collect();

    Some(Meta {
        name: toml_str(first, "displayName").unwrap_or_else(|| mod_id.clone()),
        version,
        loader,
        description: toml_str(first, "description"),
        authors,
        license: toml_str(&parsed, "license"),
        homepage: toml_str(first, "displayURL"),
        sources: None,
        issues: toml_str(&parsed, "issueTrackerURL"),
        icon: toml_str(first, "logoFile"),
        dependencies,
        mod_id,
    })
}

fn read_mcmod_info(archive: &mut Jar) -> Option<Meta> {
    // mcmod.info may be an array or an object with a "modList" key.
    let json = read_json(archive, "mcmod.info")?;
    let first = match &json {
        Json::Array(items) => items.first()?,
        other => other.get("modList")?.as_array()?.first()?,
    };
    let mod_id = json_str(first, "modid")?;

    let mut authors = json_people(first.get("authorList"));
    if authors.is_empty() {
        authors = json_people(first.get("authors"));
    }

    let dependencies = match first.get("requiredMods") {
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(Json::as_str)
            // "Forge@[10.13.4.1614,)"
            .map(|dep| {
                let (id, version) = dep.split_once('@').unwrap_or((dep, ""));
                ModDependency {
                    mod_id: id.trim().to_string(),
                    version: non_empty(Some(version)),
                    kind: DependencyKind::Required,
                }
            })
            .collect(),
        _ => Vec::new(),
    };

    Some(Meta {
        name: json_str(first, "name").unwrap_or_else(|| mod_id.clone()),
        version: json_str(first, "version").unwrap_or_else(|| "unknown".to_string()),
        loader: ModLoader::Forge,
        description: json_str(first, "description"),
        authors,
        license: None,
        homepage: json_str(first, "url"),
        sources: None,
        issues: None,
        icon: json_str(first, "logoFile"),
        dependencies,
        mod_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn jar(files: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    #[test]
    fn fabric_full() {
        let file = jar(&[(
            "fabric.mod.json",
            br#"{
                "id": "sodium", "version": "0.5.8", "name": "Sodium",
                "description": "line one
line two",
                "authors": ["JellySquid", {"name": "IMS"}],
                "license": "LGPL-3.0-only",
                "contact": {"homepage": "https://example.com", "issues": "https://example.com/issues"},
                "icon": {"16": "a.png", "128": "b.png"},
                "depends": {"fabricloader": ">=0.12", "minecraft": ["1.20", "1.20.1"]},
                "breaks": {"optifabric": "*"}
            }"#,
        )]);
        let info = read_mod_info(file.path()).unwrap();
        assert_eq!(info.mod_id, "sodium");
        assert_eq!(info.loader, ModLoader::Fabric);
        assert_eq!(info.authors, ["JellySquid", "IMS"]);
        assert_eq!(info.icon.as_deref(), Some("b.png"));
        assert_eq!(info.description.as_deref(), Some("line one line two"));
        let mc = info.dependencies.iter().find(|d| d.mod_id == "minecraft").unwrap();
        assert_eq!(mc.version.as_deref(), Some("1.20 || 1.20.1"));
        assert!(info
            .dependencies
            .iter()
            .any(|d| d.mod_id == "optifabric" && d.kind == DependencyKind::Incompatible));
    }

    #[test]
    fn neoforge_toml_with_jar_version() {
        let file = jar(&[
            (
                "META-INF/neoforge.mods.toml",
                br#"
license = "MIT"
[[mods]]
modId = "jei"
version = "${file.jarVersion}"
displayName = "Just Enough Items"
logoFile = "logo.png"
authors = "mezz"
[[dependencies.jei]]
modId = "neoforge"
type = "required"
versionRange = "[20.4,)"
[[dependencies.jei]]
modId = "emi"
type = "incompatible"
"#,
            ),
            (
                "META-INF/MANIFEST.MF",
                b"Manifest-Version: 1.0\r\nImplementation-Version: 17.3.0.49\r\n",
            ),
        ]);
        let info = read_mod_info(file.path()).unwrap();
        assert_eq!(info.loader, ModLoader::NeoForge);
        assert_eq!(info.version, "17.3.0.49");
        assert_eq!(info.license.as_deref(), Some("MIT"));
        assert_eq!(info.icon.as_deref(), Some("logo.png"));
        assert_eq!(info.dependencies.len(), 2);
        assert_eq!(info.dependencies[1].kind, DependencyKind::Incompatible);
    }

    #[test]
    fn forge_optional_dependency() {
        let file = jar(&[(
            "META-INF/mods.toml",
            br#"
[[mods]]
modId = "create"
version = "0.5.1"
[[dependencies.create]]
modId = "flywheel"
mandatory = false
"#,
        )]);
        let info = read_mod_info(file.path()).unwrap();
        assert_eq!(info.loader, ModLoader::Forge);
        assert_eq!(info.name, "create");
        assert_eq!(info.dependencies[0].kind, DependencyKind::Optional);
    }

    #[test]
    fn quilt_and_icon_bytes() {
        let file = jar(&[
            (
                "quilt.mod.json",
                br#"{"quilt_loader": {"id": "qsl", "version": "1.0",
                    "metadata": {"name": "QSL", "contributors": {"Quilt": "Owner"}, "icon": "icon.png"},
                    "depends": ["minecraft", {"id": "fabric", "optional": true}]}}"#,
            ),
            ("icon.png", b"\x89PNG"),
        ]);
        let info = read_mod_info(file.path()).unwrap();
        assert_eq!(info.loader, ModLoader::Quilt);
        assert_eq!(info.authors, ["Quilt"]);
        assert_eq!(info.dependencies[1].kind, DependencyKind::Optional);
        let icon = info.icon.unwrap();
        assert_eq!(read_mod_icon(file.path(), &icon, 1024).unwrap(), b"\x89PNG");
        assert!(read_mod_icon(file.path(), &icon, 2).is_none());
    }

    #[test]
    fn mcmod_info_required_mods() {
        let file = jar(&[(
            "mcmod.info",
            br#"{"modListVersion": 2, "modList": [{"modid": "journeymap", "version": "5.2",
                "authorList": ["techbrew"], "requiredMods": ["Forge@[10.13,)", "CodeChickenCore"]}]}"#,
        )]);
        let info = read_mod_info(file.path()).unwrap();
        assert_eq!(info.dependencies[0].mod_id, "Forge");
        assert_eq!(info.dependencies[0].version.as_deref(), Some("[10.13,)"));
        assert_eq!(info.dependencies[1].version, None);
    }

    #[test]
    fn unknown_jar_falls_back_to_file_name() {
        let file = jar(&[("a.txt", b"")]);
        let info = read_mod_info(file.path()).unwrap();
        assert_eq!(info.loader, ModLoader::Unknown);
        assert_eq!(info.version, "unknown");
    }
}
