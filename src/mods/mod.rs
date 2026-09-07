pub mod curseforge;
pub mod detector;
pub mod updater;

pub use curseforge::CurseForgeClient;
pub use detector::{detect_mods, ModInfo, ModLoader};
pub use updater::{check_updates, update_mod, ModUpdate};
