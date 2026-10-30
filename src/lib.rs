pub mod assets;
pub mod auth;
pub mod browse;
pub mod download;
pub mod error;
pub mod install;
pub mod java;
pub mod launch;
pub mod modpack;
pub mod mods;
pub mod version;

pub use error::{HexoError, Result};

pub use version::manifest::{
    fetch_version_manifest, fetch_version_json,
    VersionEntry, VersionManifest, VersionJson,
};

pub use download::{download_file, download_file_with_progress, download_batch, DownloadTask};
pub use download::mirror::{
    candidate_urls, download_source, mirror_url, set_download_source, DownloadSource,
};

pub use install::vanilla::{install_vanilla, InstanceConfig, LoaderType, LibEntry, NativeEntry};
pub use install::fabric::{install_fabric, get_fabric_loader_versions};
pub use install::forge::{install_forge, get_forge_versions};
pub use install::neoforge::{install_neoforge, get_neoforge_versions};

pub use install::loader::{
    LoaderInstaller, ProgressFn, no_progress,
    VanillaInstaller, FabricInstaller, ForgeInstaller, NeoForgeInstaller,
    create_loader, install_with_loader,
};

pub use java::detector::{find_java, probe_java, JavaInfo};
pub use java::scan::{
    java_search_paths, pick_java, probe_java_runtime, scan_java, JavaRuntime,
};
pub use java::installer::{download_java, download_java_with_progress};

pub use launch::launcher::{launch, launch_with_channel, launch_with_output, LaunchOptions};
pub use launch::crash::{analyze_crash, find_crash_files, CrashCause, CrashFiles};
pub use launch::output::{no_output, GameProcess, OutputFn, OutputKind, OutputLine};

pub use auth::microsoft::{
    AuthResult, DeviceCodeInfo, MicrosoftAuth, MsDeviceCodeResponse,
};
pub use auth::session::join_server;

pub use mods::detector::{
    detect_mods, read_mod_icon, read_mod_info, DependencyKind, ModDependency, ModInfo, ModLoader,
};
pub use mods::curseforge::CurseForgeClient;
pub use mods::modrinth::{ModrinthClient, MrProject, MrVersion};
pub use mods::updater::{check_updates, update_mod, ModUpdate, UpdateSource};
pub use mods::resolve::{
    is_compatible, match_local, resolve_graph_with, scan_local_mods, sha256_file,
    DependencyRelation, LocalMatch, LocalMod, RemoteDependency, ResolvedMod,
};

pub use browse::{
    Browser, DescriptionFormat, GalleryImage, Platform, ProjectDetail, ProjectKind,
    ProjectLink, ProjectVersion, ReleaseType, RemoteFile, SearchHit, SearchPage, SearchQuery,
    SortBy, VersionFilter,
};

pub use modpack::{
    detect_modpack_format, install_modpack, install_modpack_files, resolve_forge_version,
    update_modpack, ManualDownload, ModpackFormat, ModpackInfo, ModpackInstallResult,
    ModpackUpdateResult,
};
pub use modpack::mrpack::{install_mrpack, install_mrpack_files, read_mrpack_index, MrpackIndex};
pub use modpack::cfpack::{install_cfpack, install_cfpack_files, read_cf_manifest, CfManifest};
pub use modpack::hmclpack::{
    install_hmcl_pack, install_hmcl_pack_files, install_mcbbs_pack, install_mcbbs_pack_files,
    read_hmcl_pack, read_mcbbs_manifest, HmclManifest, HmclPack, HmclVersionJson, McbbsManifest,
};
pub use modpack::ftbpack::{
    fetch_ftb_version_manifest, get_ftb_pack, install_ftb_pack, install_ftb_pack_files,
    FtbCurseForgeRef, FtbFile, FtbPack, FtbTarget, FtbVersion, FtbVersionManifest,
};
pub use modpack::atpack::{
    fetch_atlauncher_pack_config, get_atlauncher_pack_versions, install_atlauncher_pack,
    install_atlauncher_pack_files,
    AtPackConfig, AtPackVersion,
};

pub use assets::detector::{
    detect_resource_packs, detect_shader_packs, detect_maps, detect_servers, dir_size,
    read_world, GameMode, ResourcePack, ServerEntry, ShaderPack, WorldMap,
};
