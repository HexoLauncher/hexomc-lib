use std::path::Path;
use xml::reader::{EventReader, XmlEvent};

use crate::{
    error::{HexoError, Result},
    install::{forge::install_forge_like, vanilla::LoaderType},
};

const NEOFORGE_MAVEN_BASE: &str =
    "https://maven.neoforged.net/releases/net/neoforged/neoforge";
const NEOFORGE_META_XML: &str =
    "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";

/// NeoForge versions for the given MC version.
///
/// NeoForge version format: `{mc_minor}.{patch}` (e.g. MC 1.21.1 -> NeoForge 21.1.x).
pub async fn get_neoforge_versions(mc_version: &str) -> Result<Vec<String>> {
    let client = reqwest::Client::new();
    let xml_text = client
        .get(NEOFORGE_META_XML)
        .send()
        .await?
        .text()
        .await?;

    // Derive the NeoForge version prefix from the MC version, e.g. "1.21.1" -> "21.1.".
    let prefix = mc_version_to_neoforge_prefix(mc_version);

    let versions = parse_maven_xml_versions(&xml_text)
        .into_iter()
        .filter(|v| v.starts_with(&prefix))
        .collect();

    Ok(versions)
}

/// Install NeoForge (same flow as Forge).
pub async fn install_neoforge(
    mc_version: &str,
    neoforge_version: Option<&str>,
    instance_name: &str,
    java_path: &Path,
    base_dir: &Path,
) -> Result<()> {
    let versions = get_neoforge_versions(mc_version).await?;
    let nf_ver = if let Some(v) = neoforge_version {
        v.to_string()
    } else {
        // List is ordered oldest -> newest.
        versions
            .into_iter()
            .last()
            .ok_or_else(|| HexoError::VersionNotFound(format!("NeoForge for {}", mc_version)))?
    };

    let installer_url = format!(
        "{}/{}/neoforge-{}-installer.jar",
        NEOFORGE_MAVEN_BASE, nf_ver, nf_ver
    );

    install_forge_like(
        mc_version,
        &nf_ver,
        &installer_url,
        instance_name,
        java_path,
        base_dir,
        LoaderType::NeoForge,
    )
    .await
}

/// "1.21.1" -> "21.1.", "1.21" -> "21."
fn mc_version_to_neoforge_prefix(mc_version: &str) -> String {
    let parts: Vec<&str> = mc_version.split('.').collect();
    if parts.len() >= 3 {
        format!("{}.{}.", parts[1], parts[2])
    } else if parts.len() == 2 {
        format!("{}.", parts[1])
    } else {
        String::new()
    }
}

fn parse_maven_xml_versions(xml_text: &str) -> Vec<String> {
    let parser = EventReader::from_str(xml_text);
    let mut in_version = false;
    let mut versions = Vec::new();

    for event in parser {
        match event {
            Ok(XmlEvent::StartElement { name, .. }) if name.local_name == "version" => {
                in_version = true;
            }
            Ok(XmlEvent::Characters(text)) if in_version => {
                versions.push(text);
                in_version = false;
            }
            Ok(XmlEvent::EndElement { name }) if name.local_name == "version" => {
                in_version = false;
            }
            _ => {}
        }
    }
    versions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_three_part_version() {
        assert_eq!(mc_version_to_neoforge_prefix("1.21.1"), "21.1.");
        assert_eq!(mc_version_to_neoforge_prefix("1.20.4"), "20.4.");
    }

    #[test]
    fn prefix_two_part_version() {
        assert_eq!(mc_version_to_neoforge_prefix("1.21"), "21.");
    }

    #[test]
    fn prefix_invalid_version() {
        assert_eq!(mc_version_to_neoforge_prefix("1"), "");
    }

    #[test]
    fn parse_maven_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata>
  <versioning>
    <versions>
      <version>21.1.10</version>
      <version>21.1.11</version>
      <version>21.4.0</version>
    </versions>
    <latest>21.4.0</latest>
    <release>21.4.0</release>
  </versioning>
</metadata>"#;
        let versions = parse_maven_xml_versions(xml);
        assert_eq!(versions, vec!["21.1.10", "21.1.11", "21.4.0"]);
    }

    #[test]
    fn filter_versions_by_prefix() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata>
  <versioning>
    <versions>
      <version>21.1.10</version>
      <version>21.1.11</version>
      <version>21.4.0</version>
    </versions>
  </versioning>
</metadata>"#;
        let prefix = mc_version_to_neoforge_prefix("1.21.1");
        let versions: Vec<_> = parse_maven_xml_versions(xml)
            .into_iter()
            .filter(|v| v.starts_with(&prefix))
            .collect();
        assert_eq!(versions, vec!["21.1.10", "21.1.11"]);
    }
}
