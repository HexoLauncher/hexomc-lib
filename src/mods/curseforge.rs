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
    /// Project category: 6 = mods, 12 = resource packs, 6552 = shaders.
    #[serde(rename = "classId", default)]
    pub class_id: Option<u32>,
    #[serde(default)]
    pub links: Option<CfLinks>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CfLinks {
    #[serde(rename = "websiteUrl", default)]
    pub website_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CfModFile {
    pub id: u64,
    #[serde(rename = "modId", default)]
    pub mod_id: u64,
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
    #[serde(rename = "fileFingerprint", default)]
    pub file_fingerprint: u64,
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
    pub async fn search_mod(&self, name: &str, loader: &ModLoader) -> Result<Vec<CfMod>> {
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
            .error_for_status()?
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
            .error_for_status()?
            .json()
            .await?;

        Ok(resp.data)
    }

    /// Look up several files at once by file ID.
    pub async fn get_files(&self, file_ids: &[u64]) -> Result<Vec<CfModFile>> {
        #[derive(Serialize)]
        struct Body<'a> {
            #[serde(rename = "fileIds")]
            file_ids: &'a [u64],
        }
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<CfModFile>,
        }

        let resp: Resp = self
            .client
            .post(format!("{}/mods/files", CF_BASE))
            .headers(self.headers())
            .json(&Body { file_ids })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(resp.data)
    }

    /// Look up several mods at once by project ID.
    pub async fn get_mods(&self, mod_ids: &[u64]) -> Result<Vec<CfMod>> {
        #[derive(Serialize)]
        struct Body<'a> {
            #[serde(rename = "modIds")]
            mod_ids: &'a [u64],
        }
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<CfMod>,
        }

        let resp: Resp = self
            .client
            .post(format!("{}/mods", CF_BASE))
            .headers(self.headers())
            .json(&Body { mod_ids })
            .send()
            .await?
            .error_for_status()?
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

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CfFingerprintMatch {
    #[serde(rename = "id")]
    pub mod_id: u64,
    pub file: CfModFile,
}

impl CurseForgeClient {
    pub async fn match_fingerprints(&self, fingerprints: &[u32]) -> Result<Vec<CfFingerprintMatch>> {
        #[derive(Serialize)]
        struct Body<'a> {
            fingerprints: &'a [u32],
        }
        #[derive(Deserialize)]
        struct Data {
            #[serde(rename = "exactMatches", default)]
            exact_matches: Vec<CfFingerprintMatch>,
        }
        #[derive(Deserialize)]
        struct Resp {
            data: Data,
        }

        if fingerprints.is_empty() {
            return Ok(Vec::new());
        }
        let resp: Resp = self
            .client
            .post(format!("{}/fingerprints/432", CF_BASE))
            .headers(self.headers())
            .json(&Body { fingerprints })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(resp.data.exact_matches)
    }
}

pub fn curseforge_fingerprint(bytes: &[u8]) -> u32 {
    let data: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !matches!(b, 9 | 10 | 13 | 32))
        .collect();
    murmur2(&data, 1)
}

pub async fn file_curseforge_fingerprint(path: &std::path::Path) -> Result<u32> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || Ok(curseforge_fingerprint(&std::fs::read(path)?)))
        .await
        .map_err(|e| crate::error::HexoError::Other(e.to_string()))?
}

fn murmur2(data: &[u8], seed: u32) -> u32 {
    const M: u32 = 0x5bd1_e995;
    const R: u32 = 24;

    let mut h = seed ^ data.len() as u32;
    let mut chunks = data.chunks_exact(4);
    for chunk in &mut chunks {
        let mut k = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M) ^ k;
    }

    let rest = chunks.remainder();
    if rest.len() >= 3 {
        h ^= u32::from(rest[2]) << 16;
    }
    if rest.len() >= 2 {
        h ^= u32::from(rest[1]) << 8;
    }
    if !rest.is_empty() {
        h ^= u32::from(rest[0]);
        h = h.wrapping_mul(M);
    }

    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn murmur2_of_empty_input_is_only_the_final_mix() {
        assert_eq!(murmur2(b"", 0), 0);
        assert_eq!(murmur2(b"", 1), 0x5bd1_5e36);
    }

    #[test]
    fn murmur2_uses_every_tail_byte() {
        let base = murmur2(b"abcdefg", 1);
        assert_ne!(base, murmur2(b"abcdefh", 1));
        assert_ne!(base, murmur2(b"abcdexg", 1));
        assert_ne!(base, murmur2(b"abcdxfg", 1));
    }

    #[tokio::test]
    #[ignore = "needs CURSEFORGE_API_KEY and network"]
    async fn fingerprint_matches_curseforge() {
        let key = std::env::var("CURSEFORGE_API_KEY").expect("CURSEFORGE_API_KEY");
        let client = CurseForgeClient::new(key);
        let jei = 238222;
        let file = client
            .get_mod_files(jei)
            .await
            .unwrap()
            .into_iter()
            .find(|f| f.download_url.is_some() && f.file_fingerprint != 0)
            .expect("a downloadable JEI file");
        let bytes = reqwest::get(file.download_url.as_deref().unwrap())
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let fingerprint = curseforge_fingerprint(&bytes);
        assert_eq!(u64::from(fingerprint), file.file_fingerprint);

        let matches = client.match_fingerprints(&[fingerprint]).await.unwrap();
        assert!(matches.iter().any(|m| m.mod_id == jei && m.file.id == file.id));
    }

    #[test]
    fn fingerprint_ignores_whitespace() {
        assert_eq!(
            curseforge_fingerprint(b"a b\tc\r\nd"),
            curseforge_fingerprint(b"abcd")
        );
        assert_ne!(curseforge_fingerprint(b"abcd"), curseforge_fingerprint(b"abce"));
    }

    #[test]
    fn fingerprint_matches_parse() {
        let json = r#"{"data":{"isCacheBuilt":true,"exactMatches":[{"id":238222,"file":{
            "id":4593548,"modId":238222,"displayName":"Pack 1.2","fileName":"pack-1.2.zip",
            "downloadUrl":null,"gameVersions":["1.20.1"],"fileDate":"2023-06-01T00:00:00Z",
            "hashes":[{"value":"abc","algo":1}]},"latestFiles":[]}],"exactFingerprints":[1]}}"#;
        #[derive(serde::Deserialize)]
        struct Data {
            #[serde(rename = "exactMatches")]
            exact_matches: Vec<CfFingerprintMatch>,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            data: Data,
        }
        let resp: Resp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.exact_matches[0].mod_id, 238222);
        assert_eq!(resp.data.exact_matches[0].file.id, 4593548);
    }
}
