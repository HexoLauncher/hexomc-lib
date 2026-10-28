use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use serde::Deserialize;

use super::{
    dedup, nullable, push_link, truncate, Browser, DescriptionFormat, Platform, ProjectDetail,
    ProjectVersion, ReleaseType, SearchHit, SearchPage, SortBy, PAGE_SIZE,
};
use crate::error::{HexoError, Result};
use crate::modpack::atpack::get_atlauncher_pack_versions;

const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const ATL_API: &str = "https://api.atlauncher.com/v1";
const FTB_API: &str = "https://api.feed-the-beast.com/v1/modpacks/public";

#[derive(Debug, Clone)]
struct Entry {
    hit: SearchHit,
    /// Oldest first.
    mc_versions: Vec<String>,
    popularity: i64,
    updated: i64,
    keywords: String,
}

struct Cached<T> {
    at: Instant,
    value: Arc<T>,
}

type Slot<T> = Mutex<Option<Cached<T>>>;

fn cache_get<T>(slot: &Slot<T>) -> Option<Arc<T>> {
    let guard = slot.lock().ok()?;
    guard
        .as_ref()
        .filter(|c| c.at.elapsed() < CACHE_TTL)
        .map(|c| c.value.clone())
}

fn cache_put<T>(slot: &Slot<T>, value: T) -> Arc<T> {
    let value = Arc::new(value);
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(Cached {
            at: Instant::now(),
            value: value.clone(),
        });
    }
    value
}

fn local_search(
    entries: &[Entry],
    query: &str,
    sort: SortBy,
    mc: Option<&str>,
    offset: usize,
) -> SearchPage {
    let query = query.to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();

    let mut matched: Vec<(&Entry, u8)> = entries
        .iter()
        .filter(|e| mc.is_none_or(|mc| e.mc_versions.iter().any(|v| v == mc)))
        .filter(|e| words.iter().all(|w| e.keywords.contains(w)))
        .map(|e| {
            let name = e.hit.name.to_lowercase();
            let score = if words.is_empty() {
                0
            } else if name == query {
                2
            } else if words.iter().all(|w| name.contains(w)) {
                1
            } else {
                0
            };
            (e, score)
        })
        .collect();

    match sort {
        SortBy::Relevance => {
            matched.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.popularity.cmp(&a.0.popularity)))
        }
        SortBy::Downloads => matched.sort_by(|a, b| b.0.popularity.cmp(&a.0.popularity)),
        SortBy::Updated => matched.sort_by(|a, b| b.0.updated.cmp(&a.0.updated)),
        SortBy::Name => matched.sort_by(|a, b| {
            a.0.hit
                .name
                .to_lowercase()
                .cmp(&b.0.hit.name.to_lowercase())
        }),
    }

    let total = matched.len();
    SearchPage {
        hits: matched
            .iter()
            .skip(offset)
            .take(PAGE_SIZE)
            .map(|(e, _)| e.hit.clone())
            .collect(),
        total,
        next_offset: Some(offset + PAGE_SIZE).filter(|next| *next < total),
    }
}

static ATL_CACHE: Slot<Vec<Entry>> = Mutex::new(None);

#[derive(Debug, Deserialize)]
struct AtlPackList {
    #[serde(default, deserialize_with = "nullable")]
    data: Vec<AtlPack>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AtlPack {
    name: String,
    #[serde(default, deserialize_with = "nullable")]
    safe_name: String,
    #[serde(rename = "type", default, deserialize_with = "nullable")]
    pack_type: String,
    #[serde(default, deserialize_with = "nullable")]
    versions: Vec<AtlVersion>,
    #[serde(default, deserialize_with = "nullable")]
    description: String,
    #[serde(rename = "websiteURL", default)]
    website_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AtlVersion {
    #[serde(default, deserialize_with = "nullable")]
    minecraft: String,
    #[serde(default, deserialize_with = "nullable")]
    published: i64,
}

fn atl_entries(list: AtlPackList) -> Vec<Entry> {
    list.data
        .into_iter()
        .filter(|p| p.pack_type == "public" && !p.versions.is_empty() && !p.safe_name.is_empty())
        .map(|mut p| {
            p.versions.sort_by_key(|v| v.published);
            let updated = p.versions.last().map(|v| v.published).unwrap_or(0);
            Entry {
                keywords: format!("{} {}", p.name, p.description).to_lowercase(),
                mc_versions: dedup(p.versions.into_iter().map(|v| v.minecraft)),
                popularity: 0,
                updated,
                hit: SearchHit {
                    icon_url: None,
                    id: p.safe_name,
                    name: p.name,
                    author: String::new(),
                    summary: p.description,
                    downloads: 0,
                    website: p.website_url.filter(|u| !u.is_empty()),
                },
            }
        })
        .collect()
}

/// ATLauncher safe names are plain ASCII alphanumerics.
fn atl_safe_name(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(HexoError::InvalidId(value.to_string()));
    }
    Ok(value)
}

#[derive(Debug, Clone, Deserialize)]
struct FtbPack {
    id: u64,
    name: String,
    #[serde(default, deserialize_with = "nullable")]
    slug: String,
    #[serde(default, deserialize_with = "nullable")]
    synopsis: String,
    #[serde(default, deserialize_with = "nullable")]
    description: String,
    #[serde(default)]
    art: Vec<FtbArt>,
    #[serde(default)]
    authors: Vec<FtbName>,
    #[serde(default)]
    versions: Vec<FtbVersion>,
    #[serde(default)]
    tags: Vec<FtbName>,
    #[serde(default)]
    installs: u64,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    private: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct FtbArt {
    url: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Clone, Deserialize)]
struct FtbName {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct FtbVersion {
    id: u64,
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    targets: Vec<FtbTarget>,
}

#[derive(Debug, Clone, Deserialize)]
struct FtbTarget {
    name: String,
    version: String,
}

impl FtbVersion {
    fn target(&self, name: &str) -> Option<&str> {
        self.targets
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.version.as_str())
    }
}

impl FtbPack {
    fn icon(&self) -> Option<String> {
        self.art
            .iter()
            .find(|a| a.kind == "square")
            .or_else(|| self.art.first())
            .map(|a| a.url.clone())
    }

    /// Public versions, oldest first.
    fn public_versions(&self) -> Vec<&FtbVersion> {
        let mut versions: Vec<&FtbVersion> = self.versions.iter().filter(|v| !v.private).collect();
        versions.sort_by_key(|v| v.updated);
        versions
    }

    fn website(&self) -> Option<String> {
        Some(format!(
            "https://www.feed-the-beast.com/modpacks/{}-{}",
            self.id, self.slug
        ))
        .filter(|_| !self.slug.is_empty())
    }

    fn entry(&self) -> Entry {
        Entry {
            hit: SearchHit {
                id: self.id.to_string(),
                name: self.name.clone(),
                author: self
                    .authors
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                summary: self.synopsis.clone(),
                icon_url: self.icon(),
                downloads: self.installs,
                website: self.website(),
            },
            mc_versions: dedup(
                self.public_versions()
                    .iter()
                    .filter_map(|v| v.target("minecraft").map(str::to_string)),
            ),
            popularity: self.installs as i64,
            updated: self.updated,
            keywords: std::iter::once(self.name.as_str())
                .chain(std::iter::once(self.synopsis.as_str()))
                .chain(self.tags.iter().map(|t| t.name.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase(),
        }
    }
}

struct FtbCatalog {
    entries: Vec<Entry>,
    packs: HashMap<u64, FtbPack>,
}

static FTB_CACHE: Slot<FtbCatalog> = Mutex::new(None);

impl Browser {
    async fn atl_catalog(&self) -> Result<Arc<Vec<Entry>>> {
        if let Some(cached) = cache_get(&ATL_CACHE) {
            return Ok(cached);
        }
        let list: AtlPackList = self
            .get_json(&format!("{}/packs/full/public", ATL_API))
            .await?;
        Ok(cache_put(&ATL_CACHE, atl_entries(list)))
    }

    async fn ftb_catalog(&self) -> Result<Arc<FtbCatalog>> {
        #[derive(Deserialize)]
        struct AllPacks {
            #[serde(default)]
            packs: Vec<u64>,
        }

        if let Some(cached) = cache_get(&FTB_CACHE) {
            return Ok(cached);
        }

        let all: AllPacks = self.get_json(&format!("{}/modpack/all", FTB_API)).await?;
        let packs: Vec<FtbPack> = stream::iter(all.packs)
            .map(|id| async move { self.ftb_fetch(id).await.ok() })
            .buffer_unordered(8)
            .filter_map(|pack| async move { pack })
            .filter(|pack| {
                let visible = !pack.private && pack.versions.iter().any(|v| !v.private);
                async move { visible }
            })
            .collect()
            .await;

        Ok(cache_put(
            &FTB_CACHE,
            FtbCatalog {
                entries: packs.iter().map(FtbPack::entry).collect(),
                packs: packs.into_iter().map(|p| (p.id, p)).collect(),
            },
        ))
    }

    async fn ftb_fetch(&self, pack_id: u64) -> Result<FtbPack> {
        self.get_json(&format!("{}/modpack/{}", FTB_API, pack_id))
            .await
    }

    async fn ftb_pack(&self, pack_id: u64) -> Result<FtbPack> {
        if let Some(pack) = cache_get(&FTB_CACHE).and_then(|c| c.packs.get(&pack_id).cloned()) {
            return Ok(pack);
        }
        self.ftb_fetch(pack_id).await
    }

    pub(super) async fn catalog_search(
        &self,
        platform: Platform,
        query: &str,
        sort: SortBy,
        mc: Option<&str>,
        offset: usize,
    ) -> Result<SearchPage> {
        Ok(match platform {
            Platform::Ftb => {
                local_search(&self.ftb_catalog().await?.entries, query, sort, mc, offset)
            }
            _ => local_search(&self.atl_catalog().await?, query, sort, mc, offset),
        })
    }

    pub(super) async fn catalog_versions(
        &self,
        platform: Platform,
        pack_id: &str,
    ) -> Result<Vec<ProjectVersion>> {
        if platform == Platform::Ftb {
            let pack = self.ftb_pack(super::numeric_id(pack_id)?).await?;
            return Ok(pack
                .public_versions()
                .into_iter()
                .map(|v| {
                    let mc = v.target("minecraft").unwrap_or_default().to_string();
                    ProjectVersion {
                        id: v.id.to_string(),
                        name: v.name.clone(),
                        game_versions: vec![mc.clone()],
                        mc_version: mc,
                        loaders: ftb_loaders(v),
                        release_type: ReleaseType::parse(&v.kind),
                        published: v.updated,
                        file_name: String::new(),
                        downloadable: true,
                    }
                })
                .collect());
        }

        Ok(get_atlauncher_pack_versions(atl_safe_name(pack_id)?)
            .await?
            .into_iter()
            .map(|v| ProjectVersion {
                id: v.version.clone(),
                name: v.version,
                game_versions: vec![v.minecraft.clone()],
                mc_version: v.minecraft,
                loaders: Vec::new(),
                release_type: ReleaseType::Release,
                published: v.published,
                file_name: String::new(),
                downloadable: true,
            })
            .collect())
    }

    pub(super) async fn catalog_detail(
        &self,
        platform: Platform,
        pack_id: &str,
    ) -> Result<ProjectDetail> {
        if platform == Platform::Ftb {
            let pack = self.ftb_pack(super::numeric_id(pack_id)?).await?;
            let versions = pack.public_versions();
            let mut links = Vec::new();
            push_link(&mut links, "website", pack.website());
            return Ok(ProjectDetail {
                id: pack.id.to_string(),
                name: pack.name.clone(),
                summary: pack.synopsis.clone(),
                description: truncate(pack.description.clone()),
                description_format: DescriptionFormat::Markdown,
                icon_url: pack.icon(),
                downloads: pack.installs,
                follows: 0,
                categories: pack.tags.iter().map(|t| t.name.clone()).collect(),
                game_versions: dedup(
                    versions
                        .iter()
                        .filter_map(|v| v.target("minecraft").map(str::to_string)),
                ),
                loaders: dedup(versions.iter().flat_map(|v| ftb_loaders(v))),
                license: None,
                updated: pack.updated,
                gallery: Vec::new(),
                links,
            });
        }

        let safe_name = atl_safe_name(pack_id)?;
        let catalog = self.atl_catalog().await?;
        let entry = catalog
            .iter()
            .find(|e| e.hit.id == safe_name)
            .ok_or_else(|| HexoError::Other(format!("ATLauncher pack not found: {}", safe_name)))?;

        let mut links = Vec::new();
        push_link(&mut links, "website", entry.hit.website.clone());
        Ok(ProjectDetail {
            id: entry.hit.id.clone(),
            name: entry.hit.name.clone(),
            summary: String::new(),
            description: truncate(entry.hit.summary.clone()),
            description_format: DescriptionFormat::Markdown,
            icon_url: None,
            downloads: 0,
            follows: 0,
            categories: Vec::new(),
            game_versions: entry.mc_versions.clone(),
            loaders: Vec::new(),
            license: None,
            updated: entry.updated,
            gallery: Vec::new(),
            links,
        })
    }
}

fn ftb_loaders(version: &FtbVersion) -> Vec<String> {
    version
        .targets
        .iter()
        .filter(|t| matches!(t.name.as_str(), "forge" | "fabric" | "neoforge" | "quilt"))
        .map(|t| t.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atl_pack_accepts_null_fields() {
        let json = r#"{
            "error": false,
            "data": [{
                "id": 491,
                "name": "Sky Factory 4",
                "safeName": "SkyFactory4",
                "type": "public",
                "versions": [
                    { "version": "4.2.4", "minecraft": "1.12.2", "published": 1639485294 },
                    { "version": "4.0.0", "minecraft": "1.12.1", "published": 1500000000 }
                ],
                "description": null,
                "websiteURL": null
            }]
        }"#;
        let list: AtlPackList = serde_json::from_str(json).unwrap();
        let entries = atl_entries(list);
        assert_eq!(entries[0].hit.id, "SkyFactory4");
        assert_eq!(entries[0].hit.summary, "");
        assert_eq!(entries[0].mc_versions, ["1.12.1", "1.12.2"]);
        assert_eq!(entries[0].updated, 1639485294);
        assert!(entries[0].hit.website.is_none());
    }

    fn entry(name: &str, popularity: i64, mc: &str) -> Entry {
        Entry {
            hit: SearchHit {
                id: name.to_string(),
                name: name.to_string(),
                author: String::new(),
                summary: String::new(),
                icon_url: None,
                downloads: 0,
                website: None,
            },
            mc_versions: vec![mc.to_string()],
            popularity,
            updated: 0,
            keywords: name.to_lowercase(),
        }
    }

    #[test]
    fn local_search_ranks_name_matches() {
        let entries = vec![
            entry("Sky Factory", 1, "1.12.2"),
            entry("Skies", 5, "1.21.1"),
            entry("Tech World", 9, "1.12.2"),
        ];

        let page = local_search(&entries, "sky factory", SortBy::Relevance, None, 0);
        assert_eq!(page.total, 1);
        assert_eq!(page.hits[0].name, "Sky Factory");

        let page = local_search(&entries, "", SortBy::Downloads, Some("1.12.2"), 0);
        let names: Vec<_> = page.hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["Tech World", "Sky Factory"]);
        assert_eq!(page.next_offset, None);
    }

    #[test]
    fn atl_names_are_validated() {
        assert!(atl_safe_name("SkyFactory4").is_ok());
        assert!(atl_safe_name("Sky/Factory").is_err());
    }
}
