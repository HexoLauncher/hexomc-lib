use serde::Deserialize;

use super::{
    dedup, get_json, iso_to_unix, nullable, push_link, safe_file_name, truncate, Browser,
    DescriptionFormat, GalleryImage, ProjectDetail, ProjectKind, ProjectVersion, ReleaseType,
    RemoteFile, SearchHit, SearchPage, SortBy, PAGE_SIZE,
};
use crate::error::{HexoError, Result};
use crate::mods::detector::ModLoader;

const API: &str = "https://api.curseforge.com/v1";
const GAME_MINECRAFT: &str = "432";

/// CurseForge rejects `index + pageSize` above this.
pub const CURSEFORGE_MAX_RESULTS: usize = 10_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct File {
    pub id: u64,
    #[serde(default)]
    pub mod_id: u64,
    #[serde(default, deserialize_with = "nullable")]
    pub display_name: String,
    #[serde(default, deserialize_with = "nullable")]
    pub file_name: String,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub release_type: u32,
    #[serde(default, deserialize_with = "nullable")]
    pub file_date: String,
    #[serde(default)]
    pub is_server_pack: bool,
    #[serde(default)]
    pub hashes: Vec<Hash>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Hash {
    pub value: String,
    pub algo: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Dependency {
    pub mod_id: u64,
    pub relation_type: u32,
}

impl File {
    fn is_game_version(v: &str) -> bool {
        v.starts_with(|c: char| c.is_ascii_digit())
    }

    pub fn mc_versions(&self) -> Vec<String> {
        self.game_versions
            .iter()
            .filter(|v| Self::is_game_version(v))
            .cloned()
            .collect()
    }

    pub fn loaders(&self) -> Vec<String> {
        self.game_versions
            .iter()
            .filter(|v| !Self::is_game_version(v))
            .map(|v| v.to_lowercase())
            .collect()
    }

    pub fn sha1(&self) -> Option<String> {
        self.hashes
            .iter()
            .find(|h| h.algo == 1)
            .map(|h| h.value.clone())
    }

    fn release_type(&self) -> ReleaseType {
        match self.release_type {
            2 => ReleaseType::Beta,
            3 => ReleaseType::Alpha,
            _ => ReleaseType::Release,
        }
    }
}

fn loader_type(loader: &ModLoader) -> Option<&'static str> {
    match loader {
        ModLoader::Forge => Some("1"),
        ModLoader::Fabric => Some("4"),
        ModLoader::Quilt => Some("5"),
        ModLoader::NeoForge => Some("6"),
        ModLoader::Unknown => None,
    }
}

fn loader_label(id: u32) -> Option<&'static str> {
    match id {
        1 => Some("forge"),
        4 => Some("fabric"),
        5 => Some("quilt"),
        6 => Some("neoforge"),
        _ => None,
    }
}

impl Browser {
    fn curseforge(&self, path: &str) -> Result<reqwest::RequestBuilder> {
        let key = self
            .curseforge_key
            .as_deref()
            .ok_or(HexoError::MissingApiKey)?;
        Ok(self
            .client
            .get(format!("{}{}", API, path))
            .header("x-api-key", key))
    }

    pub(super) async fn curseforge_search(
        &self,
        kind: ProjectKind,
        query: &str,
        sort: SortBy,
        mc: Option<&str>,
        loader: Option<ModLoader>,
        offset: usize,
    ) -> Result<SearchPage> {
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<Mod>,
            pagination: Pagination,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Pagination {
            #[serde(default)]
            total_count: usize,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Mod {
            id: u64,
            name: String,
            #[serde(default, deserialize_with = "nullable")]
            summary: String,
            #[serde(default)]
            download_count: u64,
            #[serde(default)]
            logo: Option<Logo>,
            #[serde(default)]
            authors: Vec<Author>,
            #[serde(default)]
            links: Option<Links>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Logo {
            #[serde(default)]
            thumbnail_url: Option<String>,
        }
        #[derive(Deserialize)]
        struct Author {
            name: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Links {
            #[serde(default)]
            website_url: Option<String>,
        }

        if offset + PAGE_SIZE > CURSEFORGE_MAX_RESULTS {
            return Ok(SearchPage {
                hits: Vec::new(),
                total: CURSEFORGE_MAX_RESULTS,
                next_offset: None,
            });
        }

        let (sort_field, sort_order) = match sort {
            SortBy::Relevance => ("2", "desc"),
            SortBy::Downloads => ("6", "desc"),
            SortBy::Updated => ("3", "desc"),
            SortBy::Name => ("4", "asc"),
        };
        let offset_text = offset.to_string();
        let limit_text = PAGE_SIZE.to_string();
        let mut params = vec![
            ("gameId", GAME_MINECRAFT),
            ("classId", kind.curseforge_class()),
            ("sortField", sort_field),
            ("sortOrder", sort_order),
            ("index", offset_text.as_str()),
            ("pageSize", limit_text.as_str()),
        ];
        if !query.is_empty() {
            params.push(("searchFilter", query));
        }
        if let Some(mc) = mc {
            params.push(("gameVersion", mc));
        }
        if let Some(loader) = loader.as_ref().and_then(loader_type) {
            params.push(("modLoaderType", loader));
        }

        let resp: Resp = get_json(self.curseforge("/mods/search")?.query(&params)).await?;

        let total = resp.pagination.total_count.min(CURSEFORGE_MAX_RESULTS);
        let next = offset + resp.data.len();
        Ok(SearchPage {
            next_offset: Some(next).filter(|n| {
                *n < total && n + PAGE_SIZE <= CURSEFORGE_MAX_RESULTS && !resp.data.is_empty()
            }),
            total,
            hits: resp
                .data
                .into_iter()
                .map(|m| SearchHit {
                    id: m.id.to_string(),
                    name: m.name,
                    author: m
                        .authors
                        .into_iter()
                        .map(|a| a.name)
                        .collect::<Vec<_>>()
                        .join(", "),
                    summary: m.summary,
                    icon_url: m
                        .logo
                        .and_then(|l| l.thumbnail_url)
                        .filter(|u| !u.is_empty()),
                    downloads: m.download_count,
                    website: m.links.and_then(|l| l.website_url),
                })
                .collect(),
        })
    }

    pub(super) async fn curseforge_versions(
        &self,
        mod_id: u64,
        mc: Option<&str>,
        loader: Option<ModLoader>,
    ) -> Result<Vec<ProjectVersion>> {
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<File>,
        }

        let mut params = vec![("pageSize", "50")];
        if let Some(mc) = mc {
            params.push(("gameVersion", mc));
        }
        if let Some(loader) = loader.as_ref().and_then(loader_type) {
            params.push(("modLoaderType", loader));
        }

        let resp: Resp = get_json(
            self.curseforge(&format!("/mods/{}/files", mod_id))?
                .query(&params),
        )
        .await?;

        Ok(resp
            .data
            .into_iter()
            .filter(|f| !f.is_server_pack)
            .map(|f| {
                let game_versions = f.mc_versions();
                ProjectVersion {
                    id: f.id.to_string(),
                    mc_version: game_versions.first().cloned().unwrap_or_default(),
                    loaders: f.loaders(),
                    release_type: f.release_type(),
                    published: iso_to_unix(&f.file_date),
                    downloadable: f.download_url.is_some(),
                    game_versions,
                    file_name: f.file_name,
                    name: f.display_name,
                }
            })
            .collect())
    }

    pub(crate) async fn curseforge_file_info(&self, mod_id: u64, file_id: u64) -> Result<File> {
        #[derive(Deserialize)]
        struct Resp {
            data: File,
        }
        let resp: Resp =
            get_json(self.curseforge(&format!("/mods/{}/files/{}", mod_id, file_id))?).await?;
        Ok(resp.data)
    }

    /// Download URL of a file, or [`HexoError::DownloadBlocked`] with the project page.
    pub(crate) async fn curseforge_remote(&self, mod_id: u64, file: &File) -> Result<RemoteFile> {
        let Some(url) = file.download_url.clone() else {
            return Err(HexoError::DownloadBlocked {
                project: mod_id.to_string(),
                website: self.curseforge_website(mod_id).await.ok().flatten(),
            });
        };
        Ok(RemoteFile {
            url,
            file_name: safe_file_name(&file.file_name, "download.zip"),
            sha1: file.sha1(),
        })
    }

    pub(super) async fn curseforge_file(&self, mod_id: u64, file_id: u64) -> Result<RemoteFile> {
        let file = self.curseforge_file_info(mod_id, file_id).await?;
        self.curseforge_remote(mod_id, &file).await
    }

    async fn curseforge_website(&self, mod_id: u64) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct Resp {
            data: Project,
        }
        #[derive(Deserialize)]
        struct Project {
            #[serde(default)]
            links: Option<Links>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Links {
            #[serde(default)]
            website_url: Option<String>,
        }
        let resp: Resp = get_json(self.curseforge(&format!("/mods/{}", mod_id))?).await?;
        Ok(resp.data.links.and_then(|l| l.website_url))
    }

    pub(super) async fn curseforge_detail(&self, mod_id: u64) -> Result<ProjectDetail> {
        #[derive(Deserialize)]
        struct ModResp {
            data: Project,
        }
        #[derive(Deserialize)]
        struct DescResp {
            #[serde(default, deserialize_with = "nullable")]
            data: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Project {
            id: u64,
            #[serde(default)]
            name: String,
            #[serde(default, deserialize_with = "nullable")]
            summary: String,
            #[serde(default)]
            download_count: u64,
            #[serde(default)]
            thumbs_up_count: u64,
            #[serde(default)]
            logo: Option<Logo>,
            #[serde(default)]
            categories: Vec<Category>,
            #[serde(default)]
            screenshots: Vec<Screenshot>,
            #[serde(default)]
            links: Option<Links>,
            #[serde(default, deserialize_with = "nullable")]
            date_modified: String,
            #[serde(default)]
            latest_files_indexes: Vec<FileIndex>,
        }
        #[derive(Deserialize)]
        struct Logo {
            #[serde(default)]
            url: Option<String>,
        }
        #[derive(Deserialize)]
        struct Category {
            #[serde(default)]
            name: String,
        }
        #[derive(Deserialize)]
        struct Screenshot {
            url: String,
            #[serde(default, deserialize_with = "nullable")]
            title: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Links {
            #[serde(default)]
            website_url: Option<String>,
            #[serde(default)]
            wiki_url: Option<String>,
            #[serde(default)]
            issues_url: Option<String>,
            #[serde(default)]
            source_url: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct FileIndex {
            #[serde(default, deserialize_with = "nullable")]
            game_version: String,
            #[serde(default)]
            mod_loader: Option<u32>,
        }

        let info_request = self.curseforge(&format!("/mods/{}", mod_id))?;
        let desc_request = self.curseforge(&format!("/mods/{}/description", mod_id))?;
        let (info, description) = futures::join!(
            get_json::<ModResp>(info_request),
            get_json::<DescResp>(desc_request)
        );

        let project = info?.data;
        let description = description.map(|d| truncate(d.data)).unwrap_or_default();

        let mut links = Vec::new();
        if let Some(l) = project.links {
            push_link(&mut links, "website", l.website_url);
            push_link(&mut links, "source", l.source_url);
            push_link(&mut links, "issues", l.issues_url);
            push_link(&mut links, "wiki", l.wiki_url);
        }

        let mut game_versions = dedup(
            project
                .latest_files_indexes
                .iter()
                .map(|f| f.game_version.clone()),
        );
        game_versions.reverse();

        Ok(ProjectDetail {
            id: project.id.to_string(),
            name: project.name,
            summary: project.summary,
            description,
            description_format: DescriptionFormat::Html,
            icon_url: project.logo.and_then(|l| l.url).filter(|u| !u.is_empty()),
            downloads: project.download_count,
            follows: project.thumbs_up_count,
            categories: project.categories.into_iter().map(|c| c.name).collect(),
            game_versions,
            loaders: dedup(
                project
                    .latest_files_indexes
                    .iter()
                    .filter_map(|f| f.mod_loader)
                    .filter_map(loader_label)
                    .map(str::to_string),
            ),
            license: None,
            updated: iso_to_unix(&project.date_modified),
            gallery: project
                .screenshots
                .into_iter()
                .map(|s| GalleryImage {
                    url: s.url,
                    title: s.title,
                })
                .collect(),
            links,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_versions_are_split_from_loaders() {
        let file: File = serde_json::from_value(serde_json::json!({
            "id": 1,
            "gameVersions": ["1.20.1", "Forge", "NeoForge", "1.20"]
        }))
        .unwrap();
        assert_eq!(file.mc_versions(), ["1.20.1", "1.20"]);
        assert_eq!(file.loaders(), ["forge", "neoforge"]);
    }

    #[tokio::test]
    async fn requests_without_a_key_fail_early() {
        let browser = Browser::new().with_curseforge_key("  ");
        let error = browser.curseforge_detail(1).await.unwrap_err();
        assert!(matches!(error, HexoError::MissingApiKey));
    }
}
