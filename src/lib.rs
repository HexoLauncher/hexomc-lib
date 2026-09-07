pub mod assets;
pub mod auth;
pub mod download;
pub mod error;
pub mod install;
pub mod java;
pub mod launch;
pub mod mods;
pub mod version;

pub use error::{HexoError, Result};

pub use version::manifest::{
    fetch_version_manifest, fetch_version_json,
    VersionEntry, VersionManifest, VersionJson,
};

pub use download::{download_file, download_batch, DownloadTask};

pub use install::vanilla::{install_vanilla, InstanceConfig, LoaderType, LibEntry, NativeEntry};
pub use install::fabric::{install_fabric, get_fabric_loader_versions};
pub use install::forge::{install_forge, get_forge_versions};
pub use install::neoforge::{install_neoforge, get_neoforge_versions};

pub use install::loader::{
    LoaderInstaller, ProgressFn, no_progress,
    VanillaInstaller, FabricInstaller, ForgeInstaller, NeoForgeInstaller,
    create_loader, install_with_loader,
};

pub use java::detector::{find_java, JavaInfo};
pub use java::installer::download_java;

pub use launch::launcher::{launch, LaunchOptions};

pub use auth::microsoft::{
    request_device_code, poll_device_code, refresh_token,
    AuthResult, DeviceCodeInfo,
};

pub use mods::detector::{detect_mods, ModInfo, ModLoader};
pub use mods::curseforge::CurseForgeClient;
pub use mods::modrinth::{ModrinthClient, MrProject, MrVersion};
pub use mods::updater::{check_updates, update_mod, ModUpdate, UpdateSource};

pub use assets::detector::{
    detect_resource_packs, detect_shader_packs, detect_maps,
    ResourcePack, ShaderPack, WorldMap,
};
