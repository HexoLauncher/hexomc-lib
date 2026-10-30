//! Download sources.
//!
//! The source is process-wide: [`set_download_source`] affects every later request for
//! Mojang, Forge, NeoForge and Fabric files and metadata. Mirrored requests fall back to
//! the official URL when the mirror fails, so an incomplete mirror never breaks installs.

use std::sync::RwLock;

use serde::de::DeserializeOwned;

use crate::error::{HexoError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DownloadSource {
    /// Mojang, Forge, NeoForge and Fabric servers.
    #[default]
    Official,
    /// BMCLAPI (<https://bmclapi2.bangbang93.com>), with the official servers as fallback.
    Bmclapi,
    /// A mirror with the same path layout as BMCLAPI, given by its base URL without a
    /// trailing slash. Build it with [`DownloadSource::custom`].
    Custom(String),
}

impl DownloadSource {
    /// A custom mirror rooted at `base_url`, which must be an absolute http(s) URL.
    ///
    /// The mirror is expected to serve the BMCLAPI layout: Mojang metadata and client
    /// files at the root, libraries and loader Maven files under `/maven/`, assets under
    /// `/assets/` and Fabric metadata under `/fabric-meta/`.
    pub fn custom(base_url: &str) -> Result<Self> {
        let trimmed = base_url.trim().trim_end_matches('/');
        let parsed = reqwest::Url::parse(trimmed)
            .map_err(|_| HexoError::InvalidMirrorUrl(base_url.to_string()))?;
        let valid = matches!(parsed.scheme(), "http" | "https")
            && parsed.host_str().is_some()
            && parsed.query().is_none()
            && parsed.fragment().is_none();
        if !valid {
            return Err(HexoError::InvalidMirrorUrl(base_url.to_string()));
        }
        Ok(Self::Custom(trimmed.to_string()))
    }

    fn base_url(&self) -> Option<&str> {
        match self {
            Self::Official => None,
            Self::Bmclapi => Some(BMCLAPI),
            Self::Custom(base) => Some(base),
        }
    }
}

static SOURCE: RwLock<DownloadSource> = RwLock::new(DownloadSource::Official);

pub fn set_download_source(source: DownloadSource) {
    *SOURCE.write().unwrap_or_else(|e| e.into_inner()) = source;
}

pub fn download_source() -> DownloadSource {
    SOURCE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

const BMCLAPI: &str = "https://bmclapi2.bangbang93.com";

/// Official URL prefix -> path on a BMCLAPI-layout mirror.
const MIRROR_RULES: [(&str, &str); 11] = [
    ("https://piston-meta.mojang.com/", "/"),
    ("https://launchermeta.mojang.com/", "/"),
    ("https://piston-data.mojang.com/", "/"),
    ("https://launcher.mojang.com/", "/"),
    ("https://libraries.minecraft.net/", "/maven/"),
    ("https://resources.download.minecraft.net/", "/assets/"),
    ("https://maven.minecraftforge.net/", "/maven/"),
    ("https://files.minecraftforge.net/maven/", "/maven/"),
    ("https://maven.neoforged.net/releases/", "/maven/"),
    ("https://meta.fabricmc.net/", "/fabric-meta/"),
    ("https://maven.fabricmc.net/", "/maven/"),
];

/// The mirrored form of `url` for `source`, or None when the source does not cover it.
pub fn mirror_url(url: &str, source: &DownloadSource) -> Option<String> {
    let base = source.base_url()?;
    MIRROR_RULES.iter().find_map(|(prefix, path)| {
        url.strip_prefix(prefix)
            .map(|rest| format!("{}{}{}", base, path, rest))
    })
}

/// URLs to try for `url` with the current source: the mirror first when it applies,
/// then the original.
pub fn candidate_urls(url: &str) -> Vec<String> {
    let mut urls = Vec::with_capacity(2);
    if let Some(mirrored) = mirror_url(url, &download_source()) {
        urls.push(mirrored);
    }
    urls.push(url.to_string());
    urls
}

/// GET `url` through the current source and deserialize the JSON body, trying the next
/// candidate when a request, status or body fails.
pub(crate) async fn get_json<T: DeserializeOwned>(client: &reqwest::Client, url: &str) -> Result<T> {
    let mut last = None;
    for candidate in candidate_urls(url) {
        match fetch(client, &candidate).await {
            Ok(response) => match response.json::<T>().await {
                Ok(value) => return Ok(value),
                Err(e) => last = Some(e.into()),
            },
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or(HexoError::DownloadFailed { url: url.to_string() }))
}

/// GET `url` through the current source and return the body as text.
pub(crate) async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let mut last = None;
    for candidate in candidate_urls(url) {
        match fetch(client, &candidate).await {
            Ok(response) => match response.text().await {
                Ok(text) => return Ok(text),
                Err(e) => last = Some(e.into()),
            },
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or(HexoError::DownloadFailed { url: url.to_string() }))
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<reqwest::Response> {
    Ok(client.get(url).send().await?.error_for_status()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmclapi_rewrites_known_hosts_only() {
        let source = &DownloadSource::Bmclapi;
        assert_eq!(
            mirror_url("https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar", source)
                .as_deref(),
            Some("https://bmclapi2.bangbang93.com/maven/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar")
        );
        assert_eq!(
            mirror_url("https://resources.download.minecraft.net/ab/abcdef", source).as_deref(),
            Some("https://bmclapi2.bangbang93.com/assets/ab/abcdef")
        );
        assert_eq!(
            mirror_url("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json", source)
                .as_deref(),
            Some("https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json")
        );
        assert_eq!(
            mirror_url("https://meta.fabricmc.net/v2/versions/loader/1.21.1", source).as_deref(),
            Some("https://bmclapi2.bangbang93.com/fabric-meta/v2/versions/loader/1.21.1")
        );
        assert_eq!(
            mirror_url(
                "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml",
                source
            )
            .as_deref(),
            Some("https://bmclapi2.bangbang93.com/maven/net/neoforged/neoforge/maven-metadata.xml")
        );
        assert_eq!(mirror_url("https://cdn.modrinth.com/data/x/a.jar", source), None);
        assert_eq!(
            mirror_url("https://libraries.minecraft.net/a.jar", &DownloadSource::Official),
            None
        );
    }

    #[test]
    fn custom_mirror_uses_bmclapi_layout() {
        let source = DownloadSource::custom(" https://mirror.example.com/mc/ ").unwrap();
        assert_eq!(source, DownloadSource::Custom("https://mirror.example.com/mc".into()));
        assert_eq!(
            mirror_url("https://maven.fabricmc.net/net/fabricmc/a.jar", &source).as_deref(),
            Some("https://mirror.example.com/mc/maven/net/fabricmc/a.jar")
        );
        for bad in ["", "mirror.example.com", "ftp://mirror.example.com", "https://x.com/?a=1"] {
            assert!(DownloadSource::custom(bad).is_err(), "{bad:?}");
        }
    }
}
