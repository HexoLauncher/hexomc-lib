use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::Result;

const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VersionManifest {
    pub latest: Latest,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    pub url: String,
    pub sha1: String,
    pub release_time: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    pub main_class: String,
    pub asset_index: AssetIndex,
    pub assets: String,
    pub java_version: JavaVersionInfo,
    pub libraries: Vec<Library>,
    pub arguments: Option<Arguments>,
    /// Used by old versions (pre-1.13).
    pub minecraft_arguments: Option<String>,
    pub downloads: ClientDownloads,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AssetIndex {
    pub id: String,
    pub sha1: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersionInfo {
    pub major_version: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Arguments {
    pub jvm: Vec<Argument>,
    pub game: Vec<Argument>,
}

/// A launch arg is either a plain string or an object with rules.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum Argument {
    Simple(String),
    Conditional {
        rules: Vec<Rule>,
        value: ArgumentValue,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum ArgumentValue {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Rule {
    pub action: String,
    pub os: Option<OsRule>,
    pub features: Option<HashMap<String, bool>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Library {
    pub name: String,
    pub downloads: Option<LibraryDownloads>,
    pub rules: Option<Vec<Rule>>,
    pub natives: Option<HashMap<String, String>>,
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LibraryDownloads {
    pub artifact: Option<LibraryArtifact>,
    pub classifiers: Option<HashMap<String, LibraryArtifact>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LibraryArtifact {
    pub path: String,
    pub sha1: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ClientDownloads {
    pub client: DownloadEntry,
    pub server: Option<DownloadEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DownloadEntry {
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AssetIndexData {
    pub objects: HashMap<String, AssetObject>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

/// Whether the current platform satisfies a library rule.
pub fn check_library_rule(rules: &[Rule]) -> bool {
    let current_os = current_os_name();
    let mut allowed = false;

    for rule in rules {
        // Feature rules (has_custom_resolution / is_demo_user) are unsupported;
        // skip the whole rule.
        if rule.features.is_some() {
            continue;
        }

        let matches = if let Some(os) = &rule.os {
            os.name.as_deref().map_or(true, |n| n == current_os)
        } else {
            true
        };

        if matches {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

/// Whether the current platform satisfies a JVM argument rule.
pub fn check_jvm_rule(rules: &[Rule]) -> bool {
    let current_os = current_os_name();
    let current_arch = current_arch();
    let mut allowed = false;

    for rule in rules {
        // Feature rules skipped, as above.
        if rule.features.is_some() {
            continue;
        }

        let os_matches = if let Some(os) = &rule.os {
            let name_ok = os.name.as_deref().map_or(true, |n| n == current_os);
            let arch_ok = os.arch.as_deref().map_or(true, |a| a == current_arch);
            name_ok && arch_ok
        } else {
            true
        };

        if os_matches {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

fn current_os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

fn current_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86"
    }
}

pub async fn fetch_version_manifest() -> Result<VersionManifest> {
    let client = reqwest::Client::new();
    let manifest = client
        .get(VERSION_MANIFEST_URL)
        .send()
        .await?
        .json::<VersionManifest>()
        .await?;
    Ok(manifest)
}

pub async fn fetch_version_json(url: &str) -> Result<VersionJson> {
    let client = reqwest::Client::new();
    let json = client
        .get(url)
        .send()
        .await?
        .json::<VersionJson>()
        .await?;
    Ok(json)
}

pub async fn fetch_asset_index(url: &str) -> Result<AssetIndexData> {
    let client = reqwest::Client::new();
    let data = client
        .get(url)
        .send()
        .await?
        .json::<AssetIndexData>()
        .await?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow_rule(os_name: &str) -> Rule {
        Rule {
            action: "allow".to_string(),
            os: Some(OsRule {
                name: Some(os_name.to_string()),
                arch: None,
                version: None,
            }),
            features: None,
        }
    }

    fn disallow_rule(os_name: &str) -> Rule {
        Rule {
            action: "disallow".to_string(),
            os: Some(OsRule {
                name: Some(os_name.to_string()),
                arch: None,
                version: None,
            }),
            features: None,
        }
    }

    fn allow_all_rule() -> Rule {
        Rule {
            action: "allow".to_string(),
            os: None,
            features: None,
        }
    }

    #[test]
    fn allow_rule_matches_current_os() {
        let os = if cfg!(target_os = "windows") { "windows" }
                 else if cfg!(target_os = "macos") { "osx" }
                 else { "linux" };
        assert!(check_library_rule(&[allow_rule(os)]));
    }

    #[test]
    fn allow_rule_does_not_match_other_os() {
        // Can't be both windows and osx at once.
        let other = if cfg!(target_os = "windows") { "osx" } else { "windows" };
        // allow rule for another OS -> false (no matching rule).
        assert!(!check_library_rule(&[allow_rule(other)]));
    }

    #[test]
    fn allow_all_then_disallow_current() {
        let os = if cfg!(target_os = "windows") { "windows" }
                 else if cfg!(target_os = "macos") { "osx" }
                 else { "linux" };
        // allow all, then disallow current OS -> should be false
        let rules = vec![allow_all_rule(), disallow_rule(os)];
        assert!(!check_library_rule(&rules));
    }

    #[test]
    fn empty_rules_not_allowed() {
        // No rules -> false (at least one allow is required).
        assert!(!check_library_rule(&[]));
    }

    #[tokio::test]
    #[ignore = "需要網路"]
    async fn fetch_manifest_returns_versions() {
        let manifest = fetch_version_manifest().await.unwrap();
        assert!(!manifest.versions.is_empty());
        assert!(!manifest.latest.release.is_empty());
    }

    #[tokio::test]
    #[ignore = "需要網路"]
    async fn fetch_known_version_json() {
        let manifest = fetch_version_manifest().await.unwrap();
        let entry = manifest.versions.iter().find(|v| v.id == "1.21.4").unwrap();
        let json = fetch_version_json(&entry.url).await.unwrap();
        assert_eq!(json.id, "1.21.4");
        assert!(!json.libraries.is_empty());
        assert!(json.java_version.major_version >= 21);
    }
}
