use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::mods::detector::ModLoader;

const CF_BASE: &str = "https://api.curseforge.com/v1";

pub struct CurseForgeClient {
    client: reqwest::Client,
    api_key: String,
}

impl CurseForgeClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
        }
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-api-key", self.api_key.parse().unwrap());
        headers.insert("Content-Type", "application/json".parse().unwrap());
        headers
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CfMod {
    pub id: u64,
    pub name: String,
    pub slug: String,
    pub summary: String,
    #[serde(rename = "downloadCount")]
    pub download_count: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CfModFile {
    pub id: u64,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "downloadUrl")]
    pub download_url: Option<String>,
    #[serde(rename = "gameVersions")]
    pub game_versions: Vec<String>,
    #[serde(rename = "fileDate")]
    pub file_date: String,
    pub hashes: Vec<CfHash>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CfHash {
    pub value: String,
    pub algo: u32, // 1=Sha1, 2=Md5
}

impl CfModFile {
    pub fn sha1(&self) -> Option<&str> {
        self.hashes
            .iter()
            .find(|h| h.algo == 1)
            .map(|h| h.value.as_str())
    }
}

fn loader_type_id(loader: &ModLoader) -> u32 {
    match loader {
        ModLoader::Forge => 1,
        ModLoader::Fabric => 4,
        ModLoader::Quilt => 5,
        ModLoader::NeoForge => 6,
        ModLoader::Unknown => 0,
    }
}

impl CurseForgeClient {
    /// Search for mods.
    pub async fn search_mod(
        &self,
        name: &str,
        loader: &ModLoader,
    ) -> Result<Vec<CfMod>> {
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<CfMod>,
        }

        let resp: Resp = self
            .client
            .get(format!("{}/mods/search", CF_BASE))
            .headers(self.headers())
            .query(&[
                ("gameId", "432"),
                ("searchFilter", name),
                ("modLoaderType", &loader_type_id(loader).to_string()),
                ("sortOrder", "desc"),
            ])
            .send()
            .await?
            .json()
            .await?;

        Ok(resp.data)
    }

    /// All file versions of a mod.
    pub async fn get_mod_files(&self, mod_id: u64) -> Result<Vec<CfModFile>> {
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<CfModFile>,
        }

        let resp: Resp = self
            .client
            .get(format!("{}/mods/{}/files", CF_BASE, mod_id))
            .headers(self.headers())
            .send()
            .await?
            .json()
            .await?;

        Ok(resp.data)
    }

    /// Latest file matching the given MC version and loader.
    pub async fn get_latest_file(
        &self,
        mod_id: u64,
        mc_version: &str,
        loader: &ModLoader,
    ) -> Result<Option<CfModFile>> {
        let files = self.get_mod_files(mod_id).await?;
        let loader_str = format!("{:?}", loader);

        let best = files
            .into_iter()
            .filter(|f| {
                f.game_versions.iter().any(|v| v == mc_version)
                    && f.game_versions
                        .iter()
                        .any(|v| v.to_lowercase() == loader_str.to_lowercase())
            })
            .max_by_key(|f| f.file_date.clone());

        Ok(best)
    }
}
