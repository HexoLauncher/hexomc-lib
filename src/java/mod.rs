pub mod detector;
pub mod installer;

pub use detector::{find_java, JavaInfo};
pub use installer::download_java;
