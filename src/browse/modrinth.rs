use std::collections::HashMap;

use serde::Deserialize;

use super::{
    get_json, iso_to_unix, loader_name, nullable, push_link, safe_file_name, truncate, Browser,
    DescriptionFormat, GalleryImage, ProjectDetail, ProjectKind, ProjectVersion, ReleaseType,
    RemoteFile, SearchHit, SearchPage, SortBy, PAGE_SIZE,
};
use crate::error::{HexoError, Result};
use crate::mods::detector::ModLoader;

pub(crate) const API: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Version {
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    #[serde(default, deserialize_with = "nullable")]
    pub version_number: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub version_type: String,
    #[serde(default, deserialize_with = "nullable")]
    pub date_published: String,
    #[serde(default)]
    pub files: Vec<File>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct File {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub hashes: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Dependency {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub dependency_type: String,
}

impl Version {
    pub fn primary_file(&self) -> Option<&File> {
        self.files
            .iter()
            .find(|f| f.primary)
            .or_else(|| self.files.first())
    }

    fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.version_number.clone()
        } else {
            self.name.clone()
        }
    }
}

impl File {
    pub fn remote(&self, fallback: &str) -> RemoteFile {
        RemoteFile {
            url: self.url.clone(),
            file_name: safe_file_name(&self.filename, fallback),
            sha1: self.hashes.get("sha1").cloned(),
        }
    }
}

impl Browser {
    pub(super) async fn modrinth_search(
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
            hits: Vec<Hit>,
            total_hits: usize,
        }
        #[derive(Deserialize)]
        struct Hit {
            project_id: String,
            slug: String,
            title: String,
            #[serde(default, deserialize_with = "nullable")]
            author: String,
            #[serde(default, deserialize_with = "nullable")]
            description: String,
            #[serde(default)]
            icon_url: Option<String>,
            #[serde(default)]
            downloads: u64,
        }

        let mut facets = vec![vec![format!("project_type:{}", kind.modrinth_type())]];
        if let Some(mc) = mc {
            facets.push(vec![format!("versions:{}", mc)]);
        }
        if let Some(name) = loader.as_ref().and_then(loader_name) {
            facets.push(vec![format!("categories:{}", name)]);
        }
        let facets = serde_json::to_string(&facets)?;

        let index = match sort {
            SortBy::Relevance | SortBy::Name => "relevance",
            SortBy::Downloads => "downloads",
            SortBy::Updated => "updated",
        };
        let offset_text = offset.to_string();
        let limit_text = PAGE_SIZE.to_string();

        let resp: Resp = get_json(self.client.get(format!("{}/search", API)).query(&[
            ("query", query),
            ("index", index),
            ("facets", &facets),
            ("offset", &offset_text),
            ("limit", &limit_text),
        ]))
        .await?;

        let kind_path = kind.modrinth_type();
        let next = offset + resp.hits.len();
        Ok(SearchPage {
            next_offset: Some(next).filter(|n| *n < resp.total_hits && !resp.hits.is_empty()),
            total: resp.total_hits,
            hits: resp
                .hits
                .into_iter()
                .map(|h| SearchHit {
                    website: Some(format!("https://modrinth.com/{}/{}", kind_path, h.slug)),
                    id: h.project_id,
                    name: h.title,
                    author: h.author,
                    summary: h.description,
                    icon_url: h.icon_url.filter(|u| !u.is_empty()),
                    downloads: h.downloads,
                })
                .collect(),
        })
    }

    pub(super) async fn modrinth_versions(
        &self,
        project_id: &str,
        mc: Option<&str>,
        loader: Option<ModLoader>,
    ) -> Result<Vec<ProjectVersion>> {
        let mut request = self
            .client
            .get(format!("{}/project/{}/version", API, project_id));
        if let Some(mc) = mc {
            request = request.query(&[("game_versions", serde_json::to_string(&[mc])?)]);
        }
        if let Some(name) = loader.as_ref().and_then(loader_name) {
            request = request.query(&[("loaders", serde_json::to_string(&[name])?)]);
        }

        let versions: Vec<Version> = get_json(request).await?;
        Ok(versions
            .into_iter()
            .map(|v| ProjectVersion {
                file_name: v
                    .primary_file()
                    .map(|f| f.filename.clone())
                    .unwrap_or_default(),
                downloadable: v.primary_file().is_some(),
                name: v.display_name(),
                mc_version: v.game_versions.last().cloned().unwrap_or_default(),
                release_type: ReleaseType::parse(&v.version_type),
                published: iso_to_unix(&v.date_published),
                loaders: v.loaders,
                game_versions: v.game_versions,
                id: v.id,
            })
            .collect())
    }

    pub(crate) async fn modrinth_version(&self, version_id: &str) -> Result<Version> {
        self.get_json(&format!("{}/version/{}", API, version_id))
            .await
    }

    pub(super) async fn modrinth_file(
        &self,
        kind: ProjectKind,
        version_id: &str,
    ) -> Result<RemoteFile> {
        let version = self.modrinth_version(version_id).await?;
        let file = if kind == ProjectKind::Modpack {
            let is_pack = |f: &&File| f.filename.to_ascii_lowercase().ends_with(".mrpack");
            version
                .files
                .iter()
                .filter(is_pack)
                .find(|f| f.primary)
                .or_else(|| version.files.iter().find(is_pack))
                .ok_or_else(|| HexoError::Other("this version has no .mrpack file".into()))?
        } else {
            version
                .primary_file()
                .ok_or_else(|| HexoError::Other("this version has no file".into()))?
        };
        Ok(file.remote(match kind {
            ProjectKind::Mod => "download.jar",
            ProjectKind::Modpack => "download.mrpack",
            _ => "download.zip",
        }))
    }

    pub(super) async fn modrinth_detail(
        &self,
        kind: ProjectKind,
        project_id: &str,
    ) -> Result<ProjectDetail> {
        #[derive(Deserialize)]
        struct Project {
            id: String,
            #[serde(default)]
            slug: String,
            #[serde(default)]
            title: String,
            #[serde(default, deserialize_with = "nullable")]
            description: String,
            #[serde(default, deserialize_with = "nullable")]
            body: String,
            #[serde(default)]
            icon_url: Option<String>,
            #[serde(default)]
            downloads: u64,
            #[serde(default)]
            followers: u64,
            #[serde(default)]
            categories: Vec<String>,
            #[serde(default)]
            game_versions: Vec<String>,
            #[serde(default)]
            loaders: Vec<String>,
            #[serde(default)]
            license: Option<License>,
            #[serde(default, deserialize_with = "nullable")]
            updated: String,
            #[serde(default)]
            gallery: Vec<Gallery>,
            #[serde(default)]
            issues_url: Option<String>,
            #[serde(default)]
            source_url: Option<String>,
            #[serde(default)]
            wiki_url: Option<String>,
            #[serde(default)]
            discord_url: Option<String>,
        }
        #[derive(Deserialize)]
        struct License {
            #[serde(default)]
            id: String,
            #[serde(default)]
            name: Option<String>,
        }
        #[derive(Deserialize)]
        struct Gallery {
            url: String,
            #[serde(default)]
            title: Option<String>,
        }

        let project: Project = self
            .get_json(&format!("{}/project/{}", API, project_id))
            .await?;

        let mut links = Vec::new();
        push_link(
            &mut links,
            "website",
            Some(format!(
                "https://modrinth.com/{}/{}",
                kind.modrinth_type(),
                project.slug
            )),
        );
        push_link(&mut links, "source", project.source_url);
        push_link(&mut links, "issues", project.issues_url);
        push_link(&mut links, "wiki", project.wiki_url);
        push_link(&mut links, "discord", project.discord_url);

        Ok(ProjectDetail {
            id: project.id,
            name: project.title,
            summary: project.description,
            description: truncate(project.body),
            description_format: DescriptionFormat::Markdown,
            icon_url: project.icon_url.filter(|u| !u.is_empty()),
            downloads: project.downloads,
            follows: project.followers,
            categories: project.categories,
            game_versions: project.game_versions,
            loaders: project.loaders,
            license: project.license.and_then(|l| {
                l.name
                    .filter(|n| !n.trim().is_empty())
                    .or(Some(l.id))
                    .filter(|s| !s.trim().is_empty())
            }),
            updated: iso_to_unix(&project.updated),
            gallery: project
                .gallery
                .into_iter()
                .map(|g| GalleryImage {
                    url: g.url,
                    title: g.title.unwrap_or_default(),
                })
                .collect(),
            links,
        })
    }
}
