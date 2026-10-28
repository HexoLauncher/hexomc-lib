pub mod curseforge;
pub mod detector;
pub mod modrinth;
pub mod resolve;
pub mod updater;

pub use curseforge::CurseForgeClient;
pub use detector::{
    detect_mods, read_mod_icon, read_mod_info, DependencyKind, ModDependency, ModInfo, ModLoader,
};
pub use modrinth::{ModrinthClient, MrProject, MrVersion};
pub use resolve::{
    is_compatible, match_local, resolve_graph_with, scan_local_mods, sha256_file,
    DependencyRelation, LocalMatch, LocalMod, RemoteDependency, ResolvedMod,
};
pub use updater::{check_updates, update_mod, ModUpdate, UpdateSource};
