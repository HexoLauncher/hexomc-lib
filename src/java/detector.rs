use std::path::{Path, PathBuf};
use std::process::Command;

use super::scan::java_search_paths;

#[derive(Debug, Clone)]
pub struct JavaInfo {
    pub path: PathBuf,
    pub version: u32,
}

/// Find a Java runtime for `required_version`: the exact major version if installed,
/// otherwise the nearest newer one. Searches the same locations as
/// [`super::scan::scan_java`], but synchronously and without a launcher root.
pub fn find_java(required_version: u32) -> Option<JavaInfo> {
    let found: Vec<JavaInfo> = java_search_paths(None)
        .iter()
        .filter_map(|path| probe_java(path))
        .collect();

    found
        .iter()
        .find(|info| info.version == required_version)
        .or_else(|| {
            found
                .iter()
                .filter(|info| info.version > required_version)
                .min_by_key(|info| info.version)
        })
        .cloned()
}

pub fn probe_java(path: &Path) -> Option<JavaInfo> {
    let output = Command::new(path).arg("-version").output().ok()?;

    let text = String::from_utf8_lossy(&output.stderr);
    let version = parse_java_version(&text)?;
    Some(JavaInfo {
        path: path.to_path_buf(),
        version,
    })
}

fn parse_java_version(output: &str) -> Option<u32> {
    for line in output.lines() {
        if line.contains("version") {
            let start = line.find('"')? + 1;
            let end = line.rfind('"')?;
            let ver_str = &line[start..end];

            let first = ver_str.split('.').next()?;

            if first == "1" {
                let major: u32 = ver_str.split('.').nth(1)?.parse().ok()?;
                return Some(major);
            } else {
                let major: u32 = first.parse().ok()?;
                return Some(major);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_java_version;

    #[test]
    fn parse_java8() {
        let output = r#"java version "1.8.0_392"
Java(TM) SE Runtime Environment (build 1.8.0_392-b08)
Java HotSpot(TM) 64-Bit Server VM (build 25.392-b08, mixed mode)"#;
        assert_eq!(parse_java_version(output), Some(8));
    }

    #[test]
    fn parse_java11() {
        let output = r#"openjdk version "11.0.21" 2023-10-17
OpenJDK Runtime Environment Temurin-11.0.21+9 (build 11.0.21+9)
OpenJDK 64-Bit Server VM Temurin-11.0.21+9 (build 11.0.21+9, mixed mode)"#;
        assert_eq!(parse_java_version(output), Some(11));
    }

    #[test]
    fn parse_java17() {
        let output = r#"openjdk version "17.0.9" 2023-10-17
OpenJDK Runtime Environment Temurin-17.0.9+9 (build 17.0.9+9)
OpenJDK 64-Bit Server VM Temurin-17.0.9+9 (build 17.0.9+9, mixed mode)"#;
        assert_eq!(parse_java_version(output), Some(17));
    }

    #[test]
    fn parse_java21() {
        let output = r#"openjdk version "21" 2023-09-19
OpenJDK Runtime Environment (build 21+35)
OpenJDK 64-Bit Server VM (build 21+35, mixed mode, sharing)"#;
        assert_eq!(parse_java_version(output), Some(21));
    }

    #[test]
    fn parse_invalid() {
        assert_eq!(parse_java_version("no version here"), None);
    }
}
