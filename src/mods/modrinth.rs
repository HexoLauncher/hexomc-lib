use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::mods::detector::ModLoader;

const MR_BASE: &str = "https://api.modrinth.com/v2";
const USER_AGENT: &str = concat!("hexo-mc-lib/", env!("CARGO_PKG_VERSION"));

pub struct ModrinthClient {
    client: reqwest::Client,
}

impl ModrinthClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

impl Default for ModrinthClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MrProject {
    #[serde(alias = "project_id")]
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub downloads: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MrVersion {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub date_published: String,
    pub files: Vec<MrFile>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MrFile {
    pub hashes: MrHashes,
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MrHashes {
    pub sha1: String,
    pub sha512: String,
}

impl MrVersion {
    /// The file marked primary, or the first file if none is.
    pub fn primary_file(&self) -> Option<&MrFile> {
        self.files
            .iter()
            .find(|f| f.primary)
            .or_else(|| self.files.first())
    }

    pub fn sha1(&self) -> Option<&str> {
        self.primary_file().map(|f| f.hashes.sha1.as_str())
    }

    pub fn download_url(&self) -> Option<&str> {
        self.primary_file().map(|f| f.url.as_str())
    }

    pub fn file_name(&self) -> Option<&str> {
        self.primary_file().map(|f| f.filename.as_str())
    }
}

/// Modrinth's loader tag. Empty for `Unknown` (no loader facet applied).
fn loader_tag(loader: &ModLoader) -> &'static str {
    match loader {
        ModLoader::Forge => "forge",
        ModLoader::Fabric => "fabric",
        ModLoader::Quilt => "quilt",
        ModLoader::NeoForge => "neoforge",
        ModLoader::Unknown => "",
    }
}

impl ModrinthClient {
    /// Search for mods.
    pub async fn search_mod(&self, name: &str, loader: &ModLoader) -> Result<Vec<MrProject>> {
        #[derive(Deserialize)]
        struct Resp {
            hits: Vec<MrProject>,
        }

        let tag = loader_tag(loader);
        let facets = if tag.is_empty() {
            "[[\"project_type:mod\"]]".to_string()
        } else {
            format!("[[\"project_type:mod\"],[\"categories:{}\"]]", tag)
        };

        let resp: Resp = self
            .client
            .get(format!("{}/search", MR_BASE))
            .query(&[("query", name), ("facets", &facets), ("limit", "20")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(resp.hits)
    }

    /// Versions of a project matching the given MC version and loader.
    /// `id` may be a project ID or slug.
    pub async fn get_project_versions(
        &self,
        id: &str,
        mc_version: &str,
        loader: &ModLoader,
    ) -> Result<Vec<MrVersion>> {
        let mut query: Vec<(&str, String)> =
            vec![("game_versions", format!("[\"{}\"]", mc_version))];
        let tag = loader_tag(loader);
        if !tag.is_empty() {
            query.push(("loaders", format!("[\"{}\"]", tag)));
        }

        let versions: Vec<MrVersion> = self
            .client
            .get(format!("{}/project/{}/version", MR_BASE, id))
            .query(&query)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(versions)
    }

    /// Newest version (by publish date) matching the MC version and loader.
    pub async fn get_latest_version(
        &self,
        id: &str,
        mc_version: &str,
        loader: &ModLoader,
    ) -> Result<Option<MrVersion>> {
        let versions = self.get_project_versions(id, mc_version, loader).await?;
        Ok(versions
            .into_iter()
            .max_by(|a, b| a.date_published.cmp(&b.date_published)))
    }

    /// Identify a locally installed file by its SHA1. Returns `None` if Modrinth
    /// doesn't know the hash.
    pub async fn get_version_by_hash(&self, sha1: &str) -> Result<Option<MrVersion>> {
        let resp = self
            .client
            .get(format!("{}/version_file/{}", MR_BASE, sha1))
            .query(&[("algorithm", "sha1")])
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        let version: MrVersion = resp.error_for_status()?.json().await?;
        Ok(Some(version))
    }

    /// Latest version for a file identified by its SHA1, constrained to the given
    /// MC version and loader. Returns `None` if the hash is unknown to Modrinth.
    pub async fn get_latest_by_hash(
        &self,
        sha1: &str,
        mc_version: &str,
        loader: &ModLoader,
    ) -> Result<Option<MrVersion>> {
        #[derive(Serialize)]
        struct Body {
            loaders: Vec<String>,
            game_versions: Vec<String>,
        }

        let tag = loader_tag(loader);
        let loaders = if tag.is_empty() {
            Vec::new()
        } else {
            vec![tag.to_string()]
        };

        let resp = self
            .client
            .post(format!("{}/version_file/{}/update", MR_BASE, sha1))
            .query(&[("algorithm", "sha1")])
            .json(&Body {
                loaders,
                game_versions: vec![mc_version.to_string()],
            })
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        let version: MrVersion = resp.error_for_status()?.json().await?;
        Ok(Some(version))
    }
}
