use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::{safe_join, ModpackInstallResult, ModpackUpdateResult};
use crate::download::downloader::verify_task;
use crate::download::DownloadTask;
use crate::error::{HexoError, Result};
use crate::mods::detector::{read_mod_info, ModLoader};

const STATE_FILE: &str = "modpack_state.json";

#[derive(Default, Deserialize, Serialize)]
struct PackState {
    managed_files: BTreeSet<String>,
    #[serde(default)]
    sha1: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pending_disabled: BTreeSet<String>,
}

impl PackState {
    fn load(instance: &Path) -> Result<Self> {
        match fs::read(safe_join(instance, STATE_FILE)?) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn record(&mut self, path: String, hash: Option<String>) {
        if let Some(hash) = hash {
            self.sha1.insert(path.clone(), hash);
        }
        self.managed_files.insert(path);
    }
}

pub(super) fn sha1_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha1::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(hex::encode(digest.finalize()));
        }
        digest.update(&buffer[..count]);
    }
}

fn path_key(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => parts.push(name.to_str().ok_or_else(|| {
                HexoError::Other(format!("non-UTF-8 pack path: {}", path.display()))
            })?),
            _ => {
                return Err(HexoError::Other(format!(
                    "invalid pack path: {}",
                    path.display()
                )))
            }
        }
    }
    if parts.is_empty() {
        return Err(HexoError::Other("empty pack path".into()));
    }
    Ok(parts.join("/"))
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn staged_files(root: &Path, dir: &Path, found: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            staged_files(root, &path, found)?;
        } else if entry.file_type()?.is_file() {
            let relative = path.strip_prefix(root).expect("entry is inside staging");
            found.insert(path_key(relative)?, path);
        }
    }
    Ok(())
}

fn mod_path(relative: &str) -> bool {
    relative
        .strip_prefix("mods/")
        .is_some_and(|name| !name.contains('/') && name.to_ascii_lowercase().ends_with(".jar"))
}

fn mod_id(path: &Path) -> Option<String> {
    let info = read_mod_info(path)?;
    (info.loader != ModLoader::Unknown).then(|| info.mod_id.to_ascii_lowercase())
}

fn disabled_path(game: &Path, relative: &str) -> Result<PathBuf> {
    safe_join(game, &format!("{relative}.disabled"))
}

pub(super) async fn reuse_downloads(
    tasks: &[DownloadTask],
    staged: &Path,
    installed: Option<&Path>,
) -> Result<()> {
    let Some(installed) = installed else {
        return Ok(());
    };
    for task in tasks {
        if task.sha1.is_none() && task.sha256.is_none() {
            continue;
        }
        let Ok(relative) = task.path.strip_prefix(staged) else {
            continue;
        };
        let relative = path_key(relative)?;
        let mut candidate = task.clone();
        candidate.path = safe_join(installed, &relative)?;
        if !candidate.path.exists() && mod_path(&relative) {
            candidate.path = disabled_path(installed, &relative)?;
        }
        if verify_task(&candidate).await {
            if let Some(parent) = task.path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(candidate.path, &task.path).await?;
        }
    }
    Ok(())
}

fn matches_hash(path: &Path, hash: Option<&str>) -> Result<bool> {
    match hash {
        Some(hash) if path.is_file() => Ok(sha1_file(path)?.eq_ignore_ascii_case(hash)),
        _ => Ok(false),
    }
}

pub(super) fn missing_files(instance: &Path) -> Result<Vec<String>> {
    let state = PackState::load(instance)?;
    let game = safe_join(instance, ".minecraft")?;
    let mut missing = Vec::new();
    for relative in state.managed_files {
        if !safe_join(&game, &relative)?.is_file() && !disabled_path(&game, &relative)?.is_file() {
            missing.push(relative);
        }
    }
    Ok(missing)
}

#[derive(Default)]
struct LocalMods {
    personal: HashMap<String, PathBuf>,
    disabled: BTreeSet<String>,
}

impl LocalMods {
    fn scan(game: &Path, previous: &PackState) -> Result<Self> {
        let mut mods = Self::default();
        let dir = safe_join(game, "mods")?;
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(mods),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let actual = format!("mods/{}", entry.file_name().to_string_lossy());
            let disabled = actual.strip_suffix(".disabled");
            let relative = disabled.unwrap_or(&actual);
            if !mod_path(relative) {
                continue;
            }
            let path = safe_join(game, &actual)?;
            let Some(id) = mod_id(&path) else { continue };
            if matches_hash(&path, previous.sha1.get(relative).map(String::as_str))? {
                if disabled.is_some() {
                    mods.disabled.insert(id);
                }
            } else {
                mods.personal.insert(id, path);
            }
        }
        Ok(mods)
    }
}

struct FileChanges {
    backup: Option<tempfile::TempDir>,
    entries: Vec<(PathBuf, Option<PathBuf>)>,
    committed: bool,
}

impl FileChanges {
    fn new(instance: &Path) -> Result<Self> {
        Ok(Self {
            backup: Some(
                tempfile::Builder::new()
                    .prefix(".pack-rollback-")
                    .tempdir_in(instance)?,
            ),
            entries: Vec::new(),
            committed: false,
        })
    }

    fn replace(&mut self, target: &Path, source: Option<&Path>) -> Result<()> {
        let parent = target.parent().expect("a pack file has a parent");
        fs::create_dir_all(parent)?;
        let saved = if target.is_file() {
            let backup = self.backup.as_ref().expect("transaction is active").path();
            let saved = backup.join(self.entries.len().to_string());
            fs::rename(target, &saved)?;
            Some(saved)
        } else {
            None
        };
        self.entries.push((target.to_path_buf(), saved));
        if let Some(source) = source {
            fs::rename(source, target)?;
        }
        Ok(())
    }
}

impl Drop for FileChanges {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut failed = false;
        for (target, saved) in self.entries.iter().rev() {
            if target.is_file() {
                failed |= fs::remove_file(target).is_err();
            }
            if let Some(saved) = saved {
                failed |= fs::rename(saved, target).is_err();
            }
        }
        if failed {
            if let Some(backup) = self.backup.take() {
                eprintln!("modpack rollback files kept at {}", backup.keep().display());
            }
        }
    }
}

pub(super) fn apply(
    instance: &Path,
    staged_game: &Path,
    mut install: ModpackInstallResult,
) -> Result<ModpackUpdateResult> {
    let state_path = safe_join(instance, STATE_FILE)?;
    let game = safe_join(instance, ".minecraft")?;
    let previous = PackState::load(instance)?;
    fs::create_dir_all(&game)?;
    let mut current = PackState::default();
    let mut incoming = BTreeMap::new();
    staged_files(staged_game, staged_game, &mut incoming)?;
    let local_mods = LocalMods::scan(&game, &previous)?;
    let mut changes = FileChanges::new(instance)?;
    let mut preserved = BTreeSet::new();
    let mut removed_files = Vec::new();

    for (relative, source) in incoming {
        let target = safe_join(&game, &relative)?;
        let disabled = disabled_path(&game, &relative)?;
        let id = if mod_path(&relative) {
            mod_id(&source)
        } else {
            None
        };
        let preserve_disabled = disabled.exists()
            || previous.pending_disabled.contains(&relative)
            || id
                .as_ref()
                .is_some_and(|id| local_mods.disabled.contains(id));
        let target = if !target.exists() && preserve_disabled {
            disabled
        } else {
            target
        };
        if let Some(personal) = id.as_ref().and_then(|id| local_mods.personal.get(id)) {
            if !same_path(personal, &target) {
                preserved.insert(personal.clone());
                continue;
            }
        }
        let hash = sha1_file(&source)?;
        let old_hash = previous.sha1.get(&relative).map(String::as_str);
        if !target.exists() || matches_hash(&target, old_hash)? {
            if !matches_hash(&target, Some(&hash))? {
                changes.replace(&target, Some(&source))?;
            }
            current.record(relative, Some(hash));
        } else if matches_hash(&target, Some(&hash))? {
            let baseline = previous.managed_files.contains(&relative).then_some(hash);
            current.record(relative, baseline);
        } else {
            preserved.insert(target);
            current.record(relative, old_hash.map(str::to_owned));
        }
    }

    let mut manual = Vec::new();
    for mut download in std::mem::take(&mut install.manual_downloads) {
        let Ok(relative) = download.dest.strip_prefix(staged_game) else {
            install.skipped.push(download.name);
            continue;
        };
        let relative = path_key(relative)?;
        if current.managed_files.contains(&relative) {
            continue;
        }
        let target = safe_join(&game, &relative)?;
        let disabled = disabled_path(&game, &relative)?;
        let is_disabled = !target.exists()
            && (disabled.exists() || previous.pending_disabled.contains(&relative));
        let installed = if is_disabled { &disabled } else { &target };
        let hash = download.sha1.as_deref();
        if !installed.exists() {
            download.dest = installed.clone();
            if is_disabled {
                current.pending_disabled.insert(relative.clone());
            }
            current.record(relative, download.sha1.clone());
            manual.push(download);
        } else if matches_hash(installed, hash)? {
            let baseline = previous
                .managed_files
                .contains(&relative)
                .then(|| download.sha1.clone())
                .flatten();
            current.record(relative, baseline);
        } else if hash.is_some()
            && matches_hash(installed, previous.sha1.get(&relative).map(String::as_str))?
        {
            changes.replace(installed, None)?;
            download.dest = installed.clone();
            if is_disabled {
                current.pending_disabled.insert(relative.clone());
            }
            current.record(relative, download.sha1.clone());
            manual.push(download);
        } else {
            preserved.insert(installed.clone());
            current.record(relative.clone(), previous.sha1.get(&relative).cloned());
        }
    }
    install.manual_downloads = manual;

    for relative in previous.managed_files.difference(&current.managed_files) {
        #[cfg(windows)]
        if current
            .managed_files
            .iter()
            .any(|path| path.eq_ignore_ascii_case(relative))
        {
            continue;
        }
        for path in [safe_join(&game, relative)?, disabled_path(&game, relative)?] {
            if matches_hash(&path, previous.sha1.get(relative).map(String::as_str))? {
                changes.replace(&path, None)?;
                removed_files.push(path);
            } else if path.exists() {
                preserved.insert(path);
            }
        }
    }

    let new_state = staged_game
        .parent()
        .expect("the staged game has a parent")
        .join(STATE_FILE);
    fs::write(&new_state, serde_json::to_vec_pretty(&current)?)?;
    changes.replace(&state_path, Some(&new_state))?;
    changes.committed = true;

    Ok(ModpackUpdateResult {
        install,
        removed_files,
        preserved_files: preserved.into_iter().collect(),
    })
}
