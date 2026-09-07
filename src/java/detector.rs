use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct JavaInfo {
    pub path: PathBuf,
    pub version: u32,
}

pub fn find_java(required_version: u32) -> Option<JavaInfo> {
    let candidates = collect_java_candidates();

    for path in &candidates {
        if let Some(info) = probe_java(path) {
            if info.version == required_version {
                return Some(info);
            }
        }
    }

    let mut best: Option<JavaInfo> = None;
    for path in &candidates {
        if let Some(info) = probe_java(path) {
            if info.version >= required_version {
                if best.as_ref().map_or(true, |b| info.version < b.version) {
                    best = Some(info);
                }
            }
        }
    }
    best
}

/// All candidate java executable paths (existence not checked).
fn collect_java_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        candidates.push(PathBuf::from(&java_home).join("bin").join(java_exe()));
    }

    if let Ok(path) = which::which("java") {
        candidates.push(path);
    }

    candidates.extend(scan_well_known_dirs());

    candidates
}

fn scan_well_known_dirs() -> Vec<PathBuf> {
    let mut found = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let roots = [
            r"C:\Program Files\Java",
            r"C:\Program Files\Eclipse Adoptium",
            r"C:\Program Files\Microsoft",
            r"C:\Program Files\Zulu",
        ];
        for root in &roots {
            found.extend(scan_dir_for_java(Path::new(root)));
        }
    }

    #[cfg(target_os = "macos")]
    {
        let root = Path::new("/Library/Java/JavaVirtualMachines");
        if root.exists() {
            if let Ok(entries) = std::fs::read_dir(root) {
                for e in entries.flatten() {
                    let java = e
                        .path()
                        .join("Contents")
                        .join("Home")
                        .join("bin")
                        .join("java");
                    if java.exists() {
                        found.push(java);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let roots = ["/usr/lib/jvm", "/usr/local/lib/jvm"];
        for root in &roots {
            found.extend(scan_dir_for_java(Path::new(root)));
        }
    }

    found
}

fn scan_dir_for_java(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if !root.exists() {
        return found;
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let java = e.path().join("bin").join(java_exe());
            if java.exists() {
                found.push(java);
            }
        }
    }
    found
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

fn java_exe() -> &'static str {
    if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    }
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
