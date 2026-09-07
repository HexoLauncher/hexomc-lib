use async_trait::async_trait;
use std::{path::{Path, PathBuf}, sync::Arc};

use crate::error::Result;
use crate::install::vanilla::LoaderType;

/// Progress callback. `Arc` so it can be cloned across async boundaries and
/// satisfy the `Clone + 'static` bounds of `install_vanilla` / `download_batch`.
pub type ProgressFn = Arc<dyn Fn(usize, usize, &str) + Send + Sync + 'static>;

pub fn no_progress() -> ProgressFn {
    Arc::new(|_, _, _| {})
}

/// Unified interface for all loaders (dependency-injection layer): callers only
/// deal with `&dyn LoaderInstaller` and never need to know the concrete loader.
///
/// ```ignore
/// let loader: Box<dyn LoaderInstaller> = Box::new(FabricInstaller::latest());
/// loader.install("1.21.4", &base_dir, no_progress()).await?;
/// ```
#[async_trait]
pub trait LoaderInstaller: Send + Sync {
    fn loader_type(&self) -> LoaderType;

    /// Versions of this loader available for the given MC version.
    /// Vanilla returns an empty vec (versions come from the MC manifest).
    async fn available_versions(&self, mc_version: &str) -> Result<Vec<String>>;

    /// Install this loader. Vanilla does the full install; others overlay on top
    /// of an installed vanilla. `instance_name` is caller-chosen to avoid
    /// same-version/different-loader collisions.
    async fn install(
        &self,
        mc_version: &str,
        instance_name: &str,
        base_dir: &Path,
        progress: ProgressFn,
    ) -> Result<()>;
}

pub struct VanillaInstaller;

#[async_trait]
impl LoaderInstaller for VanillaInstaller {
    fn loader_type(&self) -> LoaderType {
        LoaderType::Vanilla
    }

    async fn available_versions(&self, _mc_version: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn install(
        &self,
        mc_version: &str,
        instance_name: &str,
        base_dir: &Path,
        progress: ProgressFn,
    ) -> Result<()> {
        crate::install::vanilla::install_vanilla(mc_version, instance_name, base_dir, move |d, t, s| {
            progress(d, t, s)
        })
        .await?;
        Ok(())
    }
}

pub struct FabricInstaller {
    /// None uses the latest stable loader.
    pub loader_version: Option<String>,
}

impl FabricInstaller {
    pub fn new(loader_version: impl Into<String>) -> Self {
        Self { loader_version: Some(loader_version.into()) }
    }

    pub fn latest() -> Self {
        Self { loader_version: None }
    }
}

#[async_trait]
impl LoaderInstaller for FabricInstaller {
    fn loader_type(&self) -> LoaderType {
        LoaderType::Fabric
    }

    async fn available_versions(&self, mc_version: &str) -> Result<Vec<String>> {
        let versions = crate::install::fabric::get_fabric_loader_versions(mc_version).await?;
        Ok(versions.into_iter().map(|v| v.version).collect())
    }

    async fn install(
        &self,
        mc_version: &str,
        instance_name: &str,
        base_dir: &Path,
        _progress: ProgressFn,
    ) -> Result<()> {
        let loader_ver = match &self.loader_version {
            Some(v) => v.clone(),
            None => {
                let versions =
                    crate::install::fabric::get_fabric_loader_versions(mc_version).await?;
                versions
                    .into_iter()
                    .find(|v| v.stable)
                    .map(|v| v.version)
                    .unwrap_or_default()
            }
        };
        crate::install::fabric::install_fabric(mc_version, &loader_ver, instance_name, base_dir).await
    }
}

pub struct ForgeInstaller {
    /// None uses the newest version.
    pub forge_version: Option<String>,
    pub java_path: PathBuf,
}

impl ForgeInstaller {
    pub fn new(java_path: impl Into<PathBuf>) -> Self {
        Self { forge_version: None, java_path: java_path.into() }
    }

    pub fn with_version(java_path: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self { forge_version: Some(version.into()), java_path: java_path.into() }
    }
}

#[async_trait]
impl LoaderInstaller for ForgeInstaller {
    fn loader_type(&self) -> LoaderType {
        LoaderType::Forge
    }

    async fn available_versions(&self, mc_version: &str) -> Result<Vec<String>> {
        crate::install::forge::get_forge_versions(mc_version).await
    }

    async fn install(
        &self,
        mc_version: &str,
        instance_name: &str,
        base_dir: &Path,
        _progress: ProgressFn,
    ) -> Result<()> {
        crate::install::forge::install_forge(
            mc_version,
            self.forge_version.as_deref(),
            instance_name,
            &self.java_path,
            base_dir,
        )
        .await
    }
}

pub struct NeoForgeInstaller {
    /// None uses the newest version.
    pub neoforge_version: Option<String>,
    pub java_path: PathBuf,
}

impl NeoForgeInstaller {
    pub fn new(java_path: impl Into<PathBuf>) -> Self {
        Self { neoforge_version: None, java_path: java_path.into() }
    }

    pub fn with_version(java_path: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self { neoforge_version: Some(version.into()), java_path: java_path.into() }
    }
}

#[async_trait]
impl LoaderInstaller for NeoForgeInstaller {
    fn loader_type(&self) -> LoaderType {
        LoaderType::NeoForge
    }

    async fn available_versions(&self, mc_version: &str) -> Result<Vec<String>> {
        crate::install::neoforge::get_neoforge_versions(mc_version).await
    }

    async fn install(
        &self,
        mc_version: &str,
        instance_name: &str,
        base_dir: &Path,
        _progress: ProgressFn,
    ) -> Result<()> {
        crate::install::neoforge::install_neoforge(
            mc_version,
            self.neoforge_version.as_deref(),
            instance_name,
            &self.java_path,
            base_dir,
        )
        .await
    }
}

/// Build the installer matching a `LoaderType`.
pub fn create_loader(
    loader_type: LoaderType,
    java_path: Option<PathBuf>,
) -> Box<dyn LoaderInstaller> {
    match loader_type {
        LoaderType::Vanilla  => Box::new(VanillaInstaller),
        LoaderType::Fabric   => Box::new(FabricInstaller::latest()),
        LoaderType::Forge    => Box::new(ForgeInstaller::new(java_path.unwrap_or_default())),
        LoaderType::NeoForge => Box::new(NeoForgeInstaller::new(java_path.unwrap_or_default())),
    }
}

/// Full install: vanilla first, then overlay the injected loader.
///
/// `instance_name` (caller-chosen) names the subdirectory under `instance/` and
/// avoids same-version/different-loader collisions, e.g. `"1.21.1-vanilla"` vs
/// `"1.21.1-fabric"`.
///
/// ```ignore
/// install_with_loader("1.21.4", "1.21.4-fabric", &base_dir, &FabricInstaller::latest(), no_progress()).await?;
///
/// let loader = ForgeInstaller::new("/usr/lib/jvm/java-17/bin/java");
/// install_with_loader("1.20.1", "1.20.1-forge", &base_dir, &loader, no_progress()).await?;
/// ```
pub async fn install_with_loader(
    mc_version: &str,
    instance_name: &str,
    base_dir: &Path,
    loader: &dyn LoaderInstaller,
    progress: ProgressFn,
) -> Result<()> {
    // For non-vanilla loaders, ensure vanilla is installed first (same instance_name).
    if loader.loader_type() != LoaderType::Vanilla {
        VanillaInstaller
            .install(mc_version, instance_name, base_dir, progress.clone())
            .await?;
    }

    // Overlay the loader (vanilla's own install is already complete).
    loader.install(mc_version, instance_name, base_dir, progress).await
}
