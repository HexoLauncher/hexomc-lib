use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::browse::{
    loader_name, numeric_id, slug_id, Browser, Platform, ProjectKind, ReleaseType, RemoteFile,
    VersionFilter,
};
use crate::error::{HexoError, Result};
use crate::mods::detector::{read_mod_info, ModLoader};

/// Maximum number of projects in one dependency graph.
pub const MAX_GRAPH_SIZE: usize = 256;

/// How a mod relates to one of its dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyRelation {
    Required,
    Optional,
    Incompatible,
    /// Shipped inside the mod's own jar; never downloaded.
    Embedded,
}

/// A dependency declared by a remote mod version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDependency {
    pub project_id: Option<String>,
    /// An exact version the dependency is pinned to.
    pub version_id: Option<String>,
    pub relation: DependencyRelation,
}

/// One mod version chosen by the resolver.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedMod {
    pub project_id: String,
    pub version_id: String,
    pub name: String,
    pub file: RemoteFile,
    pub dependencies: Vec<RemoteDependency>,
}

fn problem(message: impl Into<String>) -> HexoError {
    HexoError::Dependency(message.into())
}

/// Whether a file lists both the Minecraft version and the loader (case-insensitive).
pub fn is_compatible(
    mc_version: &str,
    loader: &ModLoader,
    game_versions: &[String],
    loaders: &[String],
) -> bool {
    let Some(name) = loader_name(loader) else {
        return false;
    };
    game_versions.iter().any(|v| v == mc_version)
        && loaders.iter().any(|v| v.eq_ignore_ascii_case(name))
}

fn modrinth_relation(kind: &str) -> Option<DependencyRelation> {
    match kind {
        "required" => Some(DependencyRelation::Required),
        "optional" => Some(DependencyRelation::Optional),
        "incompatible" => Some(DependencyRelation::Incompatible),
        "embedded" => Some(DependencyRelation::Embedded),
        _ => None,
    }
}

/// CurseForge relation types: 1 embedded library, 2 optional, 3 required, 4 tool,
/// 5 incompatible, 6 include.
fn curseforge_relation(kind: u32) -> Option<DependencyRelation> {
    match kind {
        1 | 6 => Some(DependencyRelation::Embedded),
        2 => Some(DependencyRelation::Optional),
        3 => Some(DependencyRelation::Required),
        5 => Some(DependencyRelation::Incompatible),
        _ => None,
    }
}

impl Browser {
    /// Resolve one mod version for the given Minecraft version and loader.
    ///
    /// With `version_id` unset, picks the newest downloadable release (falling back to
    /// betas and alphas). On Modrinth `project_id` may be empty when `version_id` is set.
    pub async fn resolve_mod(
        &self,
        platform: Platform,
        project_id: &str,
        version_id: Option<&str>,
        mc_version: &str,
        loader: &ModLoader,
    ) -> Result<ResolvedMod> {
        let loader_label =
            loader_name(loader).ok_or_else(|| problem("a mod loader is required"))?;
        let incompatible = || {
            problem(format!(
                "no compatible file for Minecraft {mc_version} / {loader_label}"
            ))
        };

        let version_id = match version_id {
            Some(id) => id.to_string(),
            None => {
                let filter = VersionFilter {
                    mc_version: Some(mc_version.to_string()),
                    loader: Some(loader.clone()),
                };
                let mut versions = self
                    .versions(platform, ProjectKind::Mod, project_id, &filter)
                    .await?;
                versions.sort_by(|a, b| {
                    (b.release_type == ReleaseType::Release)
                        .cmp(&(a.release_type == ReleaseType::Release))
                        .then(b.published.cmp(&a.published))
                });
                versions
                    .into_iter()
                    .find(|v| v.downloadable)
                    .ok_or_else(|| {
                        problem(format!(
                            "{project_id}: no downloadable version for {mc_version}"
                        ))
                    })?
                    .id
            }
        };

        match platform {
            Platform::Modrinth => {
                let version = self.modrinth_version(slug_id(&version_id)?).await?;
                if !project_id.is_empty() && version.project_id != project_id {
                    return Err(problem("the requested version belongs to another project"));
                }
                if !is_compatible(mc_version, loader, &version.game_versions, &version.loaders) {
                    return Err(incompatible());
                }
                let file = version
                    .primary_file()
                    .ok_or_else(|| problem("this version has no file"))?
                    .remote("download.jar");
                Ok(ResolvedMod {
                    project_id: version.project_id.clone(),
                    version_id: version.id.clone(),
                    name: version.name.clone(),
                    file,
                    dependencies: version
                        .dependencies
                        .iter()
                        .filter_map(|d| {
                            Some(RemoteDependency {
                                project_id: d.project_id.clone(),
                                version_id: d.version_id.clone(),
                                relation: modrinth_relation(&d.dependency_type)?,
                            })
                        })
                        .collect(),
                })
            }
            Platform::CurseForge => {
                let mod_id = numeric_id(project_id)?;
                let file = self
                    .curseforge_file_info(mod_id, numeric_id(&version_id)?)
                    .await?;
                if file.mod_id != mod_id {
                    return Err(problem("the requested file belongs to another project"));
                }
                if !is_compatible(mc_version, loader, &file.game_versions, &file.loaders()) {
                    return Err(incompatible());
                }
                let remote = self.curseforge_remote(mod_id, &file).await?;
                Ok(ResolvedMod {
                    project_id: mod_id.to_string(),
                    version_id,
                    name: file.display_name.clone(),
                    file: remote,
                    dependencies: file
                        .dependencies
                        .iter()
                        .filter_map(|d| {
                            Some(RemoteDependency {
                                project_id: Some(d.mod_id.to_string()),
                                version_id: None,
                                relation: curseforge_relation(d.relation_type)?,
                            })
                        })
                        .collect(),
                })
            }
            Platform::AtLauncher | Platform::Ftb => Err(HexoError::UnsupportedPlatform(format!(
                "{:?} only publishes modpacks",
                platform
            ))),
        }
    }

    /// Resolve a mod version and all of its required (and, with `include_optional`,
    /// optional) dependencies. The requested mod comes first in the result.
    pub async fn resolve_dependencies(
        &self,
        platform: Platform,
        project_id: &str,
        version_id: &str,
        mc_version: &str,
        loader: &ModLoader,
        include_optional: bool,
    ) -> Result<Vec<ResolvedMod>> {
        resolve_graph_with(
            project_id,
            version_id,
            include_optional,
            |project, version| async move {
                self.resolve_mod(platform, &project, version.as_deref(), mc_version, loader)
                    .await
            },
        )
        .await
    }
}

fn add_pin(pins: &mut BTreeMap<String, String>, project: &str, version: &str) -> Result<()> {
    if pins.get(project).is_some_and(|id| id != version) {
        return Err(problem(format!(
            "{project}: dependencies require conflicting versions"
        )));
    }
    pins.insert(project.to_string(), version.to_string());
    Ok(())
}

/// The resolver behind [`Browser::resolve_dependencies`], with the lookup injected.
///
/// `fetch(project, version)` resolves one project; `version` is `None` when any compatible
/// version will do, and `project` is empty when only a version ID is known. Exact version
/// pins from dependencies win over unpinned choices; the walk repeats until the pins stop
/// changing. Fails on conflicting pins, incompatible pairs, or graphs larger than
/// [`MAX_GRAPH_SIZE`].
pub async fn resolve_graph_with<F, Fut>(
    project_id: &str,
    version_id: &str,
    include_optional: bool,
    mut fetch: F,
) -> Result<Vec<ResolvedMod>>
where
    F: FnMut(String, Option<String>) -> Fut,
    Fut: Future<Output = Result<ResolvedMod>>,
{
    let root = || BTreeMap::from([(project_id.to_string(), version_id.to_string())]);
    let wanted = |relation: DependencyRelation| {
        relation == DependencyRelation::Required
            || (include_optional && relation == DependencyRelation::Optional)
    };

    let mut pins = root();
    let mut cache: HashMap<(String, Option<String>), ResolvedMod> = HashMap::new();
    for _ in 0..128 {
        let mut next_pins = root();
        let mut queue = VecDeque::from([project_id.to_string()]);
        let mut visited = HashSet::new();
        let mut selected = Vec::new();
        while let Some(project) = queue.pop_front() {
            if !visited.insert(project.clone()) {
                continue;
            }
            if visited.len() > MAX_GRAPH_SIZE {
                return Err(problem(format!(
                    "the dependency graph exceeds {MAX_GRAPH_SIZE} projects"
                )));
            }
            let key = (
                project.clone(),
                next_pins
                    .get(&project)
                    .or_else(|| pins.get(&project))
                    .cloned(),
            );
            if !cache.contains_key(&key) {
                let resolved = fetch(project.clone(), key.1.clone())
                    .await
                    .map_err(|e| problem(format!("{project}: {e}")))?;
                cache.insert(key.clone(), resolved);
            }
            let dependencies = cache[&key].dependencies.clone();
            for dep in dependencies.iter().filter(|d| wanted(d.relation)) {
                let dep_project = match (&dep.project_id, &dep.version_id) {
                    (Some(id), _) => id.clone(),
                    (None, Some(id)) => fetch(String::new(), Some(id.clone())).await?.project_id,
                    _ => {
                        return Err(problem(format!(
                            "{project}: a dependency has no project or version"
                        )))
                    }
                };
                if let Some(id) = &dep.version_id {
                    add_pin(&mut next_pins, &dep_project, id)?;
                }
                queue.push_back(dep_project);
            }
            selected.push(key);
        }
        if next_pins != pins {
            pins = next_pins;
            continue;
        }
        for key in &selected {
            for dep in &cache[key].dependencies {
                if dep.relation == DependencyRelation::Incompatible
                    && selected.iter().any(|other| {
                        let node = &cache[other];
                        dep.project_id
                            .as_ref()
                            .is_some_and(|p| p == &node.project_id)
                            && dep
                                .version_id
                                .as_ref()
                                .is_none_or(|v| v == &node.version_id)
                    })
                {
                    return Err(problem(format!(
                        "{}: the selected dependencies include an incompatible mod",
                        cache[key].name
                    )));
                }
            }
        }
        return Ok(selected
            .into_iter()
            .filter_map(|key| cache.remove(&key))
            .collect());
    }
    Err(problem("dependency versions did not converge"))
}

/// A jar in an instance's `mods` folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMod {
    pub file_name: String,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    /// Mod ID from the jar's metadata, if it could be read.
    pub mod_id: Option<String>,
}

/// How a downloaded mod relates to the installed ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status", content = "file")]
pub enum LocalMatch {
    NotInstalled,
    /// The identical file is already installed and enabled.
    Installed,
    /// A different file (or a disabled copy) has the same name, hash or mod ID.
    Conflict(String),
}

/// Lowercase hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(hex::encode(hash.finalize()))
}

/// Every `.jar` and `.jar.disabled` in `mods_dir`, sorted by file name. A missing folder
/// yields an empty list.
pub fn scan_local_mods(mods_dir: &Path) -> Result<Vec<LocalMod>> {
    if !mods_dir.exists() {
        return Ok(Vec::new());
    }
    let mut mods = Vec::new();
    for entry in std::fs::read_dir(mods_dir)? {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        let lower = file_name.to_ascii_lowercase();
        if !lower.ends_with(".jar") && !lower.ends_with(".jar.disabled") {
            continue;
        }
        mods.push(LocalMod {
            sha256: sha256_file(&entry.path())?,
            mod_id: read_mod_info(&entry.path())
                .filter(|m| m.loader != ModLoader::Unknown)
                .map(|m| m.mod_id),
            file_name,
        });
    }
    mods.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(mods)
}

/// Compare a downloaded mod with the installed ones by hash, file name and mod ID.
pub fn match_local(file_name: &str, sha256: &str, mod_id: &str, locals: &[LocalMod]) -> LocalMatch {
    let matches: Vec<_> = locals
        .iter()
        .filter(|m| {
            m.sha256 == sha256
                || m.file_name.eq_ignore_ascii_case(file_name)
                || m.mod_id.as_deref() == Some(mod_id)
        })
        .collect();
    if let Some(conflict) = matches
        .iter()
        .find(|m| m.sha256 != sha256 || m.file_name.to_ascii_lowercase().ends_with(".disabled"))
    {
        return LocalMatch::Conflict(conflict.file_name.clone());
    }
    if matches.is_empty() {
        LocalMatch::NotInstalled
    } else {
        LocalMatch::Installed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(project: &str, version: Option<&str>, relation: DependencyRelation) -> RemoteDependency {
        RemoteDependency {
            project_id: Some(project.to_string()),
            version_id: version.map(str::to_string),
            relation,
        }
    }

    fn node(project: &str, version: &str, dependencies: Vec<RemoteDependency>) -> ResolvedMod {
        ResolvedMod {
            project_id: project.to_string(),
            version_id: version.to_string(),
            name: project.to_string(),
            dependencies,
            file: RemoteFile {
                url: String::new(),
                file_name: format!("{project}.jar"),
                sha1: None,
            },
        }
    }

    use DependencyRelation::*;

    #[tokio::test]
    async fn dependency_cycles_are_deduplicated_and_optional_mods_are_opt_in() {
        for optional in [false, true] {
            let result = resolve_graph_with("main", "1", optional, |project, _| {
                std::future::ready(Ok(match project.as_str() {
                    "main" => node(
                        "main",
                        "1",
                        vec![
                            dep("lib", None, Required),
                            dep("extra", None, Optional),
                            dep("embedded", None, Embedded),
                        ],
                    ),
                    "lib" => node("lib", "2", vec![dep("main", Some("1"), Required)]),
                    "extra" => node("extra", "1", vec![dep("lib", None, Required)]),
                    _ => panic!("unexpected dependency: {project}"),
                }))
            })
            .await
            .unwrap();
            assert_eq!(result.len(), if optional { 3 } else { 2 });
            assert_eq!(result.iter().filter(|m| m.project_id == "lib").count(), 1);
            assert_eq!(result[0].project_id, "main");
        }
    }

    #[tokio::test]
    async fn a_later_exact_dependency_replaces_an_unpinned_choice() {
        let result = resolve_graph_with("main", "1", false, |project, version| {
            std::future::ready(Ok(match project.as_str() {
                "main" => node(
                    "main",
                    "1",
                    vec![dep("lib", None, Required), dep("other", None, Required)],
                ),
                "lib" => node("lib", version.as_deref().unwrap_or("latest"), vec![]),
                "other" => node("other", "1", vec![dep("lib", Some("pinned"), Required)]),
                _ => panic!("unexpected dependency"),
            }))
        })
        .await
        .unwrap();
        let lib = result.iter().find(|m| m.project_id == "lib").unwrap();
        assert_eq!(lib.version_id, "pinned");
    }

    #[tokio::test]
    async fn conflicting_pins_and_incompatible_projects_are_rejected() {
        let conflict = resolve_graph_with("main", "1", false, |_, _| {
            std::future::ready(Ok(node(
                "main",
                "1",
                vec![
                    dep("lib", Some("1"), Required),
                    dep("lib", Some("2"), Required),
                ],
            )))
        })
        .await;
        assert!(matches!(conflict, Err(HexoError::Dependency(_))));

        let incompatible = resolve_graph_with("main", "1", false, |project, _| {
            std::future::ready(Ok(if project == "main" {
                node(
                    "main",
                    "1",
                    vec![dep("lib", None, Required), dep("lib", None, Incompatible)],
                )
            } else {
                node("lib", "1", vec![])
            }))
        })
        .await;
        assert!(matches!(incompatible, Err(HexoError::Dependency(_))));
    }

    #[tokio::test]
    async fn exact_dependencies_do_not_require_a_latest_version() {
        let result = resolve_graph_with("main", "1", false, |project, version| {
            std::future::ready(match project.as_str() {
                "main" => Ok(node(
                    "main",
                    "1",
                    vec![dep("lib", Some("archived"), Required)],
                )),
                "lib" if version.as_deref() == Some("archived") => {
                    Ok(node("lib", "archived", vec![]))
                }
                _ => Err(problem("no latest version available")),
            })
        })
        .await
        .unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].version_id, "archived");
    }

    #[test]
    fn both_game_version_and_loader_must_match() {
        let games = vec!["1.21.1".to_string()];
        let loaders = vec!["Fabric".to_string()];
        assert!(is_compatible(
            "1.21.1",
            &ModLoader::Fabric,
            &games,
            &loaders
        ));
        assert!(!is_compatible("1.21", &ModLoader::Fabric, &games, &loaders));
        assert!(!is_compatible(
            "1.21.1",
            &ModLoader::Forge,
            &games,
            &loaders
        ));
        assert!(!is_compatible(
            "1.21.1",
            &ModLoader::Unknown,
            &games,
            &loaders
        ));
    }

    #[test]
    fn curseforge_relations_are_mapped() {
        assert_eq!(curseforge_relation(1), Some(Embedded));
        assert_eq!(curseforge_relation(2), Some(Optional));
        assert_eq!(curseforge_relation(3), Some(Required));
        assert_eq!(curseforge_relation(4), None);
        assert_eq!(curseforge_relation(5), Some(Incompatible));
        assert_eq!(modrinth_relation("embedded"), Some(Embedded));
        assert_eq!(modrinth_relation("other"), None);
    }

    #[test]
    fn installed_disabled_and_conflicting_mods_are_distinguished() {
        let mut locals = vec![LocalMod {
            file_name: "renamed.jar".into(),
            sha256: "same".into(),
            mod_id: Some("lib".into()),
        }];
        assert_eq!(
            match_local("lib.jar", "same", "lib", &locals),
            LocalMatch::Installed
        );
        assert_eq!(
            match_local("lib.jar", "different", "lib", &locals),
            LocalMatch::Conflict("renamed.jar".into())
        );
        locals[0].file_name = "renamed.jar.disabled".into();
        assert_eq!(
            match_local("lib.jar", "same", "lib", &locals),
            LocalMatch::Conflict("renamed.jar.disabled".into())
        );
        assert_eq!(
            match_local("new.jar", "new", "new", &locals),
            LocalMatch::NotInstalled
        );
    }

    #[test]
    fn local_scan_hashes_jars_and_skips_other_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.jar"), b"b").unwrap();
        std::fs::write(dir.path().join("a.jar.disabled"), b"a").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        let mods = scan_local_mods(dir.path()).unwrap();
        let names: Vec<_> = mods.iter().map(|m| m.file_name.as_str()).collect();
        assert_eq!(names, ["a.jar.disabled", "b.jar"]);
        assert_eq!(
            mods[1].sha256,
            "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d"
        );
        assert!(scan_local_mods(&dir.path().join("missing"))
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn modrinth_live_dependency_smoke() {
        let browser = Browser::new();
        let root = browser
            .resolve_mod(
                Platform::Modrinth,
                "YL57xq9U",
                None,
                "1.21.1",
                &ModLoader::Fabric,
            )
            .await
            .unwrap();
        let graph = browser
            .resolve_dependencies(
                Platform::Modrinth,
                &root.project_id,
                &root.version_id,
                "1.21.1",
                &ModLoader::Fabric,
                false,
            )
            .await
            .unwrap();
        assert!(graph.len() > 1);
        assert!(graph.iter().any(|m| m.project_id == "AANobbMI"));
    }
}
