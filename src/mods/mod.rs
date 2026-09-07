pub mod curseforge;
pub mod detector;
pub mod modrinth;
pub mod updater;

pub use curseforge::CurseForgeClient;
pub use detector::{detect_mods, ModInfo, ModLoader};
pub use modrinth::{ModrinthClient, MrProject, MrVersion};
pub use updater::{check_updates, update_mod, ModUpdate, UpdateSource};
