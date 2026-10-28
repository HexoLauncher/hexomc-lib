mod catalog;
mod curseforge;
mod modrinth;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{HexoError, Result};
use crate::mods::detector::ModLoader;

/// Results per search page.
pub const PAGE_SIZE: usize = 20;

const USER_AGENT: &str = concat!("hexomc-lib/", env!("CARGO_PKG_VERSION"));

/// Where a project is hosted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Modrinth,
    CurseForge,
    AtLauncher,
    Ftb,
}

impl Platform {
    /// Whether the platform publishes mods, resource packs and shaders, not only modpacks.
    pub fn hosts_content(self) -> bool {
        matches!(self, Platform::Modrinth | Platform::CurseForge)
    }
}

/// What kind of project to browse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Mod,
    ResourcePack,
    Shader,
    Modpack,
}

impl ProjectKind {
    /// Folder under `.minecraft` that files of this kind are installed into.
    pub fn folder(self) -> Option<&'static str> {
        match self {
            ProjectKind::Mod => Some("mods"),
            ProjectKind::ResourcePack => Some("resourcepacks"),
            ProjectKind::Shader => Some("shaderpacks"),
            ProjectKind::Modpack => None,
        }
    }

    /// Whether search and version filters should apply the mod loader.
    pub fn uses_loader(self) -> bool {
        matches!(self, ProjectKind::Mod)
    }

    pub(crate) fn modrinth_type(self) -> &'static str {
        match self {
            ProjectKind::Mod => "mod",
            ProjectKind::ResourcePack => "resourcepack",
            ProjectKind::Shader => "shader",
            ProjectKind::Modpack => "modpack",
        }
    }

    pub(crate) fn curseforge_class(self) -> &'static str {
        match self {
            ProjectKind::Mod => "6",
            ProjectKind::ResourcePack => "12",
            ProjectKind::Shader => "6552",
            ProjectKind::Modpack => "4471",
        }
    }
}

/// Search result ordering. Platforms without a matching index fall back to relevance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    #[default]
    Relevance,
    Downloads,
    Updated,
    Name,
}

/// A search request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub platform: Platform,
    pub kind: ProjectKind,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub sort: SortBy,
    /// Only projects with a file for this Minecraft version.
    #[serde(default)]
    pub mc_version: Option<String>,
    /// Only projects for this loader. Ignored unless `kind` is [`ProjectKind::Mod`].
    #[serde(default)]
    pub loader: Option<ModLoader>,
    #[serde(default)]
    pub offset: usize,
}

impl SearchQuery {
    pub fn new(platform: Platform, kind: ProjectKind) -> Self {
        Self {
            platform,
            kind,
            query: String::new(),
            sort: SortBy::Relevance,
            mc_version: None,
            loader: None,
            offset: 0,
        }
    }
}

/// One project in a search page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// Modrinth project ID, CurseForge numeric ID, ATLauncher safe name or FTB numeric ID.
    pub id: String,
    pub name: String,
    pub author: String,
    pub summary: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub website: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub total: usize,
    /// Offset of the next page; `None` when this is the last page.
    pub next_offset: Option<usize>,
}

/// Release channel of a version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseType {
    Release,
    Beta,
    Alpha,
    Archived,
}

impl ReleaseType {
    pub(crate) fn parse(text: &str) -> Self {
        match text.to_ascii_lowercase().as_str() {
            "beta" => ReleaseType::Beta,
            "alpha" => ReleaseType::Alpha,
            "archived" => ReleaseType::Archived,
            _ => ReleaseType::Release,
        }
    }
}

/// One installable version of a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectVersion {
    pub id: String,
    pub name: String,
    /// Newest Minecraft version this file supports; empty if unknown.
    pub mc_version: String,
    pub game_versions: Vec<String>,
    /// Lowercase loader names (`fabric`, `forge`, ...).
    pub loaders: Vec<String>,
    pub release_type: ReleaseType,
    /// Unix seconds; 0 if the platform does not say.
    pub published: i64,
    pub file_name: String,
    /// False when there is no file or its author disallows third-party downloads.
    pub downloadable: bool,
}

/// Filter for [`Browser::versions`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionFilter {
    #[serde(default)]
    pub mc_version: Option<String>,
    /// Ignored unless the project kind is [`ProjectKind::Mod`].
    #[serde(default)]
    pub loader: Option<ModLoader>,
}

/// Markup used by [`ProjectDetail::description`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DescriptionFormat {
    Markdown,
    Html,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GalleryImage {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLink {
    /// One of `website`, `source`, `issues`, `wiki`, `discord`.
    pub kind: String,
    pub url: String,
}

/// Full project page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetail {
    pub id: String,
    pub name: String,
    pub summary: String,
    /// Long description, cut to [`MAX_DESCRIPTION`] characters.
    pub description: String,
    pub description_format: DescriptionFormat,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub follows: u64,
    pub categories: Vec<String>,
    /// Supported Minecraft versions, oldest first.
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub license: Option<String>,
    /// Unix seconds; 0 if the platform does not say.
    pub updated: i64,
    pub gallery: Vec<GalleryImage>,
    pub links: Vec<ProjectLink>,
}

/// Maximum characters kept from a project description.
pub const MAX_DESCRIPTION: usize = 40_000;

/// A file to download for a project version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFile {
    pub url: String,
    /// File name with any directory parts removed.
    pub file_name: String,
    pub sha1: Option<String>,
}

/// Entry point for browsing every platform.
///
/// CurseForge requests need an API key; without one they fail with
/// [`HexoError::MissingApiKey`].
#[derive(Clone)]
pub struct Browser {
    client: reqwest::Client,
    curseforge_key: Option<String>,
}

impl Default for Browser {
    fn default() -> Self {
        Self::new()
    }
}

impl Browser {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .build()
                .unwrap_or_default(),
            curseforge_key: None,
        }
    }

    /// Set the CurseForge API key. A blank key is treated as no key.
    pub fn with_curseforge_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.curseforge_key = Some(key.trim().to_string()).filter(|k| !k.is_empty());
        self
    }

    /// Search one page of projects.
    pub async fn search(&self, query: &SearchQuery) -> Result<SearchPage> {
        let text = query.query.trim();
        let mc = trimmed(query.mc_version.as_deref());
        let loader = loader_for(query.kind, query.loader.as_ref());
        match query.platform {
            Platform::Modrinth => {
                self.modrinth_search(query.kind, text, query.sort, mc, loader, query.offset)
                    .await
            }
            Platform::CurseForge => {
                self.curseforge_search(query.kind, text, query.sort, mc, loader, query.offset)
                    .await
            }
            Platform::AtLauncher | Platform::Ftb => {
                require_modpack(query.platform, query.kind)?;
                self.catalog_search(query.platform, text, query.sort, mc, query.offset)
                    .await
            }
        }
    }

    /// Versions of a project, newest first.
    pub async fn versions(
        &self,
        platform: Platform,
        kind: ProjectKind,
        project_id: &str,
        filter: &VersionFilter,
    ) -> Result<Vec<ProjectVersion>> {
        let mc = trimmed(filter.mc_version.as_deref());
        let loader = loader_for(kind, filter.loader.as_ref());
        let mut versions = match platform {
            Platform::Modrinth => {
                self.modrinth_versions(slug_id(project_id)?, mc, loader)
                    .await?
            }
            Platform::CurseForge => {
                self.curseforge_versions(numeric_id(project_id)?, mc, loader)
                    .await?
            }
            Platform::AtLauncher | Platform::Ftb => {
                require_modpack(platform, kind)?;
                let mut versions = self.catalog_versions(platform, project_id).await?;
                if let Some(mc) = mc {
                    versions.retain(|v| v.mc_version == mc);
                }
                versions
            }
        };
        versions.sort_by(|a, b| b.published.cmp(&a.published));
        Ok(versions)
    }

    /// Full project page.
    pub async fn detail(
        &self,
        platform: Platform,
        kind: ProjectKind,
        project_id: &str,
    ) -> Result<ProjectDetail> {
        match platform {
            Platform::Modrinth => self.modrinth_detail(kind, slug_id(project_id)?).await,
            Platform::CurseForge => self.curseforge_detail(numeric_id(project_id)?).await,
            Platform::AtLauncher | Platform::Ftb => {
                require_modpack(platform, kind)?;
                self.catalog_detail(platform, project_id).await
            }
        }
    }

    /// The file to download for a version. For Modrinth modpacks this is the `.mrpack`.
    ///
    /// ATLauncher and FTB packs have no single file; install them with
    /// [`crate::install_atlauncher_pack_files`] or [`crate::install_ftb_pack_files`].
    /// CurseForge files whose authors disallow third-party downloads fail with
    /// [`HexoError::DownloadBlocked`].
    pub async fn file(
        &self,
        platform: Platform,
        kind: ProjectKind,
        project_id: &str,
        version_id: &str,
    ) -> Result<RemoteFile> {
        match platform {
            Platform::Modrinth => self.modrinth_file(kind, slug_id(version_id)?).await,
            Platform::CurseForge => {
                self.curseforge_file(numeric_id(project_id)?, numeric_id(version_id)?)
                    .await
            }
            Platform::AtLauncher | Platform::Ftb => Err(HexoError::UnsupportedPlatform(format!(
                "{:?} packs have no single download file",
                platform
            ))),
        }
    }

    pub(crate) async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        get_json(self.client.get(url)).await
    }
}

pub(crate) async fn get_json<T: DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T> {
    Ok(request.send().await?.error_for_status()?.json().await?)
}

/// Deserialize `null` as the type's default value.
pub(crate) fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

fn loader_for(kind: ProjectKind, loader: Option<&ModLoader>) -> Option<ModLoader> {
    loader
        .filter(|l| kind.uses_loader() && **l != ModLoader::Unknown)
        .cloned()
}

fn require_modpack(platform: Platform, kind: ProjectKind) -> Result<()> {
    if kind == ProjectKind::Modpack {
        Ok(())
    } else {
        Err(HexoError::UnsupportedPlatform(format!(
            "{:?} only publishes modpacks",
            platform
        )))
    }
}

/// Lowercase loader name used by Modrinth and in [`ProjectVersion::loaders`].
pub(crate) fn loader_name(loader: &ModLoader) -> Option<&'static str> {
    match loader {
        ModLoader::Fabric => Some("fabric"),
        ModLoader::Forge => Some("forge"),
        ModLoader::NeoForge => Some("neoforge"),
        ModLoader::Quilt => Some("quilt"),
        ModLoader::Unknown => None,
    }
}

/// Validate a Modrinth project/version ID or slug before putting it in a URL.
pub(crate) fn slug_id(value: &str) -> Result<&str> {
    let value = value.trim();
    let ok = !value.is_empty()
        && !value.contains("..")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(value)
    } else {
        Err(HexoError::InvalidId(value.to_string()))
    }
}

pub(crate) fn numeric_id(value: &str) -> Result<u64> {
    value
        .trim()
        .parse()
        .map_err(|_| HexoError::InvalidId(value.to_string()))
}

/// Strip directory parts so a remote file name cannot escape its target folder.
pub(crate) fn safe_file_name(raw: &str, fallback: &str) -> String {
    let name = raw
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('.');
    if name.is_empty() {
        fallback.to_string()
    } else {
        name.to_string()
    }
}

pub(crate) fn truncate(text: String) -> String {
    let Some((end, _)) = text.char_indices().nth(MAX_DESCRIPTION) else {
        return text;
    };
    let cut = text[..end].rfind('\n').unwrap_or(end);
    format!("{}…", text[..cut].trim_end())
}

pub(crate) fn dedup(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        if !value.is_empty() && !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

pub(crate) fn push_link(links: &mut Vec<ProjectLink>, kind: &str, url: Option<String>) {
    if let Some(url) = url.filter(|u| !u.trim().is_empty()) {
        links.push(ProjectLink {
            kind: kind.to_string(),
            url,
        });
    }
}

/// Parse the date part (and time, if present) of an ISO-8601 timestamp into Unix seconds.
/// Returns 0 for unparsable input.
pub(crate) fn iso_to_unix(text: &str) -> i64 {
    let part = |range: std::ops::Range<usize>| text.get(range).and_then(|s| s.parse::<i64>().ok());
    let (Some(year), Some(month), Some(day)) = (part(0..4), part(5..7), part(8..10)) else {
        return 0;
    };
    let seconds = part(11..13).unwrap_or(0) * 3600
        + part(14..16).unwrap_or(0) * 60
        + part(17..19).unwrap_or(0);

    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) * 86_400 + seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(iso_to_unix("1970-01-01T00:00:00Z"), 0);
        assert_eq!(iso_to_unix("2024-02-29T12:00:00.123Z"), 1_709_208_000);
        assert_eq!(iso_to_unix("garbage"), 0);
    }

    #[test]
    fn long_description_is_cut_on_a_line_boundary() {
        let long = format!("{}\n{}", "a".repeat(100), "b".repeat(MAX_DESCRIPTION));
        let cut = truncate(long);
        assert_eq!(cut, format!("{}…", "a".repeat(100)));
        assert_eq!(truncate("short".into()), "short");
    }

    #[test]
    fn file_names_stay_in_the_target_folder() {
        assert_eq!(safe_file_name("../../evil.jar", "x.jar"), "evil.jar");
        assert_eq!(safe_file_name("C:\\a\\b.zip", "x.zip"), "b.zip");
        assert_eq!(safe_file_name("..", "x.jar"), "x.jar");
    }

    #[test]
    fn ids_are_validated() {
        assert!(slug_id("AANobbMI").is_ok());
        assert!(slug_id("../x").is_err());
        assert!(slug_id("a/b").is_err());
        assert_eq!(numeric_id(" 42 ").unwrap(), 42);
        assert!(numeric_id("4a").is_err());
    }

    #[test]
    fn content_only_platforms_reject_other_kinds() {
        assert!(require_modpack(Platform::Ftb, ProjectKind::Mod).is_err());
        assert!(require_modpack(Platform::Ftb, ProjectKind::Modpack).is_ok());
        assert_eq!(
            loader_for(ProjectKind::Shader, Some(&ModLoader::Fabric)),
            None
        );
        assert_eq!(
            loader_for(ProjectKind::Mod, Some(&ModLoader::Unknown)),
            None
        );
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn modrinth_browse_smoke() {
        let browser = Browser::new();
        let mut query = SearchQuery::new(Platform::Modrinth, ProjectKind::Mod);
        query.query = "sodium".into();
        query.loader = Some(ModLoader::Fabric);
        let page = browser.search(&query).await.unwrap();
        assert!(!page.hits.is_empty());

        let filter = VersionFilter {
            mc_version: Some("1.21.1".into()),
            loader: Some(ModLoader::Fabric),
        };
        let versions = browser
            .versions(Platform::Modrinth, ProjectKind::Mod, "AANobbMI", &filter)
            .await
            .unwrap();
        assert!(versions
            .iter()
            .all(|v| v.game_versions.iter().any(|g| g == "1.21.1")));

        let detail = browser
            .detail(Platform::Modrinth, ProjectKind::Mod, "AANobbMI")
            .await
            .unwrap();
        assert_eq!(detail.description_format, DescriptionFormat::Markdown);
        assert!(!detail.description.is_empty());

        let file = browser
            .file(
                Platform::Modrinth,
                ProjectKind::Mod,
                "AANobbMI",
                &versions[0].id,
            )
            .await
            .unwrap();
        assert!(file.file_name.ends_with(".jar"));
    }

    #[tokio::test]
    #[ignore]
    async fn modpack_catalog_smoke() {
        let browser = Browser::new();
        let mut query = SearchQuery::new(Platform::AtLauncher, ProjectKind::Modpack);
        query.query = "sky".into();
        let page = browser.search(&query).await.unwrap();
        let first = &page.hits[0];
        let detail = browser
            .detail(Platform::AtLauncher, ProjectKind::Modpack, &first.id)
            .await
            .unwrap();
        assert_eq!(detail.id, first.id);
        let versions = browser
            .versions(
                Platform::AtLauncher,
                ProjectKind::Modpack,
                &first.id,
                &VersionFilter::default(),
            )
            .await
            .unwrap();
        assert!(!versions.is_empty());

        let ftb = browser
            .detail(Platform::Ftb, ProjectKind::Modpack, "1")
            .await
            .unwrap();
        assert!(!ftb.description.is_empty());
        assert!(ftb
            .links
            .iter()
            .any(|l| l.url.contains("feed-the-beast.com")));
        let ftb_versions = browser
            .versions(
                Platform::Ftb,
                ProjectKind::Modpack,
                "1",
                &VersionFilter::default(),
            )
            .await
            .unwrap();
        assert!(!ftb_versions.is_empty());

        assert!(browser
            .search(&SearchQuery::new(Platform::Ftb, ProjectKind::Mod))
            .await
            .is_err());
    }
}
