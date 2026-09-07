pub mod fabric;
pub mod forge;
pub mod loader;
pub mod neoforge;
pub mod vanilla;

pub use vanilla::{InstanceConfig, LibEntry, LoaderType, NativeEntry};
pub use loader::{
    LoaderInstaller, ProgressFn, no_progress,
    VanillaInstaller, FabricInstaller, ForgeInstaller, NeoForgeInstaller,
    create_loader, install_with_loader,
};
