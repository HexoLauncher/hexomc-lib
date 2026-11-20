use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Serialize;

use super::detector::{read_mod_info, DependencyKind, ModLoader};
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DependencyStatus {
    Installed,
    Disabled,
    Missing,
    Runtime,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyProvider {
    pub file_name: String,
    pub version: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModDependencyEdge {
    pub file_name: String,
    pub mod_id: String,
    pub mod_name: String,
    pub dependency_id: String,
    pub requirement: Option<String>,
    pub kind: DependencyKind,
    pub status: DependencyStatus,
    pub providers: Vec<DependencyProvider>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModDependencyReport {
    pub relationships: Vec<ModDependencyEdge>,
    pub unrecognized_files: Vec<String>,
}

/// Read relationships declared by enabled client mods and locate installed providers.
/// A provider's presence does not imply that its version satisfies the declared range.
pub fn inspect_mod_dependencies(
    mods_dir: &Path,
    runtime_ids: &[&str],
) -> Result<ModDependencyReport> {
    let mut report = ModDependencyReport::default();
    let entries = match fs::read_dir(mods_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(error.into()),
    };
    let mut providers = BTreeMap::<String, Vec<DependencyProvider>>::new();
    let mut enabled = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let lower = file_name.to_ascii_lowercase();
        let is_enabled = lower.ends_with(".jar");
        if !is_enabled && !lower.ends_with(".jar.disabled") {
            continue;
        }
        let Some(info) =
            read_mod_info(&entry.path()).filter(|info| info.loader != ModLoader::Unknown)
        else {
            report.unrecognized_files.push(file_name);
            continue;
        };
        for (id, version) in super::detector::provided_mod_ids(&entry.path(), &info) {
            providers
                .entry(id.to_ascii_lowercase())
                .or_default()
                .push(DependencyProvider {
                    file_name: info.file_name.clone(),
                    version,
                    enabled: is_enabled,
                });
        }
        if is_enabled {
            enabled.push(info);
        }
    }
    for matches in providers.values_mut() {
        matches.sort_by(|a, b| {
            b.enabled
                .cmp(&a.enabled)
                .then(a.file_name.cmp(&b.file_name))
        });
    }
    for info in enabled {
        for dependency in info.dependencies {
            let candidates = providers.get(&dependency.mod_id.to_ascii_lowercase());
            let status = if runtime_ids
                .iter()
                .any(|id| id.eq_ignore_ascii_case(&dependency.mod_id))
            {
                DependencyStatus::Runtime
            } else if candidates.is_some_and(|list| list.iter().any(|provider| provider.enabled)) {
                DependencyStatus::Installed
            } else if candidates.is_some() {
                DependencyStatus::Disabled
            } else {
                DependencyStatus::Missing
            };
            report.relationships.push(ModDependencyEdge {
                file_name: info.file_name.clone(),
                mod_id: info.mod_id.clone(),
                mod_name: info.name.clone(),
                dependency_id: dependency.mod_id,
                requirement: dependency.version,
                kind: dependency.kind,
                status,
                providers: candidates.cloned().unwrap_or_default(),
            });
        }
    }
    report.relationships.sort_by(|a, b| {
        let needs_attention = |edge: &ModDependencyEdge| {
            (edge.kind == DependencyKind::Required
                && matches!(
                    edge.status,
                    DependencyStatus::Missing | DependencyStatus::Disabled
                ))
                || (edge.kind == DependencyKind::Incompatible
                    && matches!(
                        edge.status,
                        DependencyStatus::Installed | DependencyStatus::Runtime
                    ))
        };
        needs_attention(b)
            .cmp(&needs_attention(a))
            .then(a.file_name.cmp(&b.file_name))
            .then(a.dependency_id.cmp(&b.dependency_id))
    });
    report.unrecognized_files.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fabric_jar(path: &Path, metadata: &str) {
        let mut jar = zip::ZipWriter::new(fs::File::create(path).unwrap());
        jar.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        jar.write_all(metadata.as_bytes()).unwrap();
        jar.finish().unwrap();
    }

    #[test]
    fn recognizes_present_aliases_disabled_providers_and_absent_mods() {
        let mods = tempfile::tempdir().unwrap();
        fabric_jar(
            &mods.path().join("source.jar"),
            r#"{
            "id":"source", "version":"1", "depends":{"alias":">=2","off":"*","missing":"*","minecraft":"*"},
            "suggests":{"optional":"*"}, "breaks":{"incompatible":"<1"}
        }"#,
        );
        fabric_jar(
            &mods.path().join("libraries.jar"),
            r#"{"id":"libraries","version":"2","provides":["alias","incompatible"]}"#,
        );
        fabric_jar(
            &mods.path().join("off.jar.disabled"),
            r#"{"id":"off","version":"1"}"#,
        );
        fs::write(mods.path().join("broken.jar"), b"not a zip").unwrap();
        let report = inspect_mod_dependencies(mods.path(), &["minecraft"]).unwrap();
        let by_id: BTreeMap<_, _> = report
            .relationships
            .iter()
            .map(|edge| (edge.dependency_id.as_str(), edge))
            .collect();
        assert_eq!(by_id["alias"].status, DependencyStatus::Installed);
        assert_eq!(by_id["alias"].providers[0].version, "2");
        assert_eq!(by_id["off"].status, DependencyStatus::Disabled);
        assert_eq!(by_id["missing"].status, DependencyStatus::Missing);
        assert_eq!(by_id["minecraft"].status, DependencyStatus::Runtime);
        assert_eq!(by_id["optional"].kind, DependencyKind::Optional);
        assert_eq!(by_id["incompatible"].kind, DependencyKind::Incompatible);
        assert_eq!(report.unrecognized_files, ["broken.jar"]);
    }
}
