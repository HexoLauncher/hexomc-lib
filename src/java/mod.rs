pub mod detector;
pub mod installer;
pub mod scan;

pub use detector::{find_java, JavaInfo};
pub use scan::{java_search_paths, pick_java, probe_java_runtime, scan_java, JavaRuntime};
pub use installer::{download_java, download_java_with_progress};
