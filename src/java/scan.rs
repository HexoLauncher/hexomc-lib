use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use super::detector::JavaInfo;

const JAVA_EXE: &str = if cfg!(windows) { "java.exe" } else { "java" };
const PROBE_CONCURRENCY: usize = 8;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A Java runtime found by [`scan_java`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaRuntime {
    /// The `java` executable inside the runtime's real `java.home`.
    pub path: PathBuf,
    /// Major version (8, 17, 21, ...).
    pub version: u32,
    /// Full version string, e.g. `21.0.4` or `1.8.0_392`.
    pub full_version: String,
    pub vendor: String,
    /// `os.arch`, e.g. `amd64`, `aarch64`, `x86`.
    pub arch: String,
}

impl JavaRuntime {
    pub fn is_64bit(&self) -> bool {
        matches!(self.arch.as_str(), "amd64" | "x86_64" | "aarch64" | "arm64")
    }

    pub fn info(&self) -> JavaInfo {
        JavaInfo {
            path: self.path.clone(),
            version: self.version,
        }
    }
}

/// Probe every runtime found by [`java_search_paths`], newest version first (64-bit before
/// 32-bit), without duplicates. `base_dir` is the launcher root whose `java/` folder holds
/// runtimes installed by [`crate::download_java`].
pub async fn scan_java(base_dir: Option<&Path>) -> Vec<JavaRuntime> {
    let base_dir = base_dir.map(Path::to_path_buf);
    let paths = tokio::task::spawn_blocking(move || java_search_paths(base_dir.as_deref()))
        .await
        .unwrap_or_default();

    let mut found: Vec<JavaRuntime> = stream::iter(paths)
        .map(|path| async move { probe_java_runtime(&path).await })
        .buffer_unordered(PROBE_CONCURRENCY)
        .filter_map(|runtime| async move { runtime })
        .collect()
        .await;

    found.sort_by(|a, b| {
        b.version
            .cmp(&a.version)
            .then(b.is_64bit().cmp(&a.is_64bit()))
            .then(a.path.cmp(&b.path))
    });
    let mut seen = HashSet::new();
    found.retain(|r| seen.insert(path_key(&r.path)));
    found
}

/// The runtime for `required`: the exact major version if present, otherwise the nearest
/// newer one, preferring 64-bit.
pub fn pick_java(runtimes: &[JavaRuntime], required: u32) -> Option<&JavaRuntime> {
    runtimes.iter().find(|r| r.version == required).or_else(|| {
        runtimes
            .iter()
            .filter(|r| r.version > required)
            .min_by(|a, b| {
                a.version
                    .cmp(&b.version)
                    .then(b.is_64bit().cmp(&a.is_64bit()))
            })
    })
}

/// Run `java -XshowSettings:properties -version` and read the result. Returns `None` if the
/// file is not a working Java executable or does not answer within 5 seconds.
pub async fn probe_java_runtime(path: &Path) -> Option<JavaRuntime> {
    let mut command = tokio::process::Command::new(path);
    command
        .args(["-XshowSettings:properties", "-version"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let output = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    parse_runtime(path, &String::from_utf8_lossy(&output.stderr))
}

fn parse_runtime(path: &Path, text: &str) -> Option<JavaRuntime> {
    let props = parse_properties(text);
    let full_version = props
        .get("java.version")
        .cloned()
        .or_else(|| version_from_banner(text))?;
    let version = props
        .get("java.specification.version")
        .and_then(|v| major_version(v))
        .or_else(|| major_version(&full_version))?;
    let real_path = props
        .get("java.home")
        .map(|home| Path::new(home).join("bin").join(JAVA_EXE))
        .filter(|exe| exe.is_file());

    Some(JavaRuntime {
        path: real_path.unwrap_or_else(|| path.to_path_buf()),
        version,
        full_version,
        vendor: props.get("java.vendor").cloned().unwrap_or_default(),
        arch: props.get("os.arch").cloned().unwrap_or_default(),
    })
}

/// Existing `java` executables in every known location, without duplicates. Nothing is
/// executed; use [`scan_java`] to probe them.
pub fn java_search_paths(base_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut exes: Vec<PathBuf> = Vec::new();
    let mut roots: Vec<(PathBuf, u32)> = Vec::new();

    if let Some(home) = std::env::var_os("JAVA_HOME") {
        exes.push(PathBuf::from(home).join("bin").join(JAVA_EXE));
    }
    if let Some(path) = std::env::var_os("PATH") {
        exes.extend(std::env::split_paths(&path).map(|dir| dir.join(JAVA_EXE)));
    }

    if let Some(base) = base_dir {
        roots.push((base.join("java"), 3));
    }

    let home = dirs::home_dir();
    for (relative, depth) in [
        (".jdks", 1),
        (".gradle/jdks", 1),
        (".sdkman/candidates/java", 1),
        ("curseforge/minecraft/Install/runtime", 4),
    ] {
        if let Some(home) = &home {
            roots.push((home.join(relative), depth));
        }
    }

    let minecraft_dir = if cfg!(target_os = "linux") {
        home.as_ref().map(|h| h.join(".minecraft"))
    } else if cfg!(target_os = "macos") {
        dirs::data_dir().map(|d| d.join("minecraft"))
    } else {
        dirs::data_dir().map(|d| d.join(".minecraft"))
    };
    if let Some(dir) = minecraft_dir {
        roots.push((dir.join("runtime"), 4));
    }
    if let Some(data) = dirs::data_dir() {
        roots.push((data.join("PrismLauncher").join("java"), 3));
    }

    #[cfg(windows)]
    {
        const VENDOR_DIRS: &[&str] = &[
            "Java",
            "Eclipse Adoptium",
            "Eclipse Foundation",
            "AdoptOpenJDK",
            "Zulu",
            "Azul",
            "Microsoft",
            "BellSoft",
            "Amazon Corretto",
            "Semeru",
            "IBM",
            "OpenJDK",
            "GraalVM",
        ];
        let program_dirs = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .chain(dirs::data_local_dir().map(|d| d.join("Programs")));
        for dir in program_dirs {
            for vendor in VENDOR_DIRS {
                roots.push((dir.join(vendor), 2));
            }
        }
        if let Some(local) = dirs::data_local_dir() {
            roots.push((
                local
                    .join("Packages")
                    .join("Microsoft.4297127D64EC6_8wekyb3d8bbwe")
                    .join("LocalCache")
                    .join("Local")
                    .join("runtime"),
                4,
            ));
        }
        if let Some(home) = &home {
            roots.push((home.join("scoop").join("apps"), 2));
        }
        for location in installed_java_locations() {
            roots.push((location, 2));
        }
    }

    #[cfg(target_os = "macos")]
    {
        exes.extend(
            macos_java_homes()
                .into_iter()
                .map(|home| home.join("bin").join(JAVA_EXE)),
        );
        roots.push(("/Library/Java/JavaVirtualMachines".into(), 1));
        if let Some(home) = &home {
            roots.push((home.join("Library/Java/JavaVirtualMachines"), 1));
        }
        roots.push(("/opt/homebrew/opt".into(), 3));
        roots.push(("/usr/local/opt".into(), 3));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for dir in ["/usr/lib/jvm", "/usr/lib64/jvm", "/usr/local/lib/jvm"] {
            roots.push((dir.into(), 1));
        }
    }

    for (root, depth) in roots {
        find_java_executables(&root, depth, &mut exes);
    }

    let mut seen = HashSet::new();
    exes.retain(|path| path.is_file() && seen.insert(path_key(path)));
    exes
}

fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('/', "\\").to_lowercase()
    } else {
        text.to_string()
    }
}

/// Look for `bin/java` (or the macOS bundle's `Contents/Home/bin/java`) in `dir`, then in
/// its subfolders down to `depth` levels.
fn find_java_executables(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    for home in [dir.to_path_buf(), dir.join("Contents").join("Home")] {
        let exe = home.join("bin").join(JAVA_EXE);
        if exe.is_file() {
            out.push(exe);
            return;
        }
    }
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_java_executables(&path, depth - 1, out);
        }
    }
}

/// Install folders of Java-looking programs in the Windows uninstall registry.
#[cfg(windows)]
fn installed_java_locations() -> Vec<PathBuf> {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    const KEYWORDS: &[&str] = &[
        "java", "jdk", "jre", "openjdk", "temurin", "zulu", "liberica", "corretto", "graalvm",
    ];

    let mut locations = Vec::new();
    for (hive, view) in [
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
        (HKEY_CURRENT_USER, 0),
    ] {
        let Ok(uninstall) = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, KEY_READ | view)
        else {
            continue;
        };
        for name in uninstall.enum_keys().flatten() {
            let Ok(entry) = uninstall.open_subkey_with_flags(&name, KEY_READ | view) else {
                continue;
            };
            let label = ["DisplayName", "Publisher"]
                .into_iter()
                .filter_map(|field| entry.get_value::<String, _>(field).ok())
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            if !KEYWORDS.iter().any(|keyword| label.contains(keyword)) {
                continue;
            }
            if let Ok(location) = entry.get_value::<String, _>("InstallLocation") {
                let location = location.trim();
                if !location.is_empty() {
                    locations.push(PathBuf::from(location));
                }
            }
        }
    }
    locations
}

#[cfg(target_os = "macos")]
fn macos_java_homes() -> Vec<PathBuf> {
    let Ok(output) = std::process::Command::new("/usr/libexec/java_home")
        .arg("-V")
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| {
            line.find(" /")
                .map(|at| PathBuf::from(line[at + 1..].trim()))
        })
        .collect()
}

fn parse_properties(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once(" = ")?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn version_from_banner(text: &str) -> Option<String> {
    let line = text.lines().find(|line| line.contains(" version \""))?;
    let start = line.find('"')? + 1;
    let end = start + line[start..].find('"')?;
    Some(line[start..end].to_string())
}

/// Major version of a Java version string; `1.x` maps to `x`.
pub(crate) fn major_version(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn major_versions() {
        assert_eq!(major_version("1.8.0_392"), Some(8));
        assert_eq!(major_version("1.8"), Some(8));
        assert_eq!(major_version("17"), Some(17));
        assert_eq!(major_version("21.0.4"), Some(21));
        assert_eq!(major_version("25-ea"), Some(25));
        assert_eq!(major_version("abc"), None);
    }

    #[test]
    fn reads_settings_output() {
        let text = "Property settings:\n    java.home = /nonexistent/jdk-21\n    java.library.path = C:\\a\n        C:\\b\n    java.specification.version = 21\n    java.vendor = Eclipse Adoptium\n    java.version = 21.0.4\n    os.arch = amd64\n\nopenjdk version \"21.0.4\" 2024-07-16\n";
        let props = parse_properties(text);
        assert_eq!(props["java.library.path"], "C:\\a");
        assert_eq!(version_from_banner(text).as_deref(), Some("21.0.4"));

        let runtime = parse_runtime(Path::new("/bin/java"), text).unwrap();
        assert_eq!(runtime.version, 21);
        assert_eq!(runtime.full_version, "21.0.4");
        assert_eq!(runtime.vendor, "Eclipse Adoptium");
        assert_eq!(runtime.path, Path::new("/bin/java"));
        assert!(runtime.is_64bit());
    }

    #[test]
    fn old_runtimes_fall_back_to_the_banner() {
        let text = "java version \"1.8.0_392\"\nJava(TM) SE Runtime Environment\n";
        let runtime = parse_runtime(Path::new("java"), text).unwrap();
        assert_eq!(runtime.version, 8);
        assert_eq!(runtime.full_version, "1.8.0_392");
        assert!(parse_runtime(Path::new("java"), "not java").is_none());
    }

    fn runtime(version: u32, arch: &str, path: &str) -> JavaRuntime {
        JavaRuntime {
            path: PathBuf::from(path),
            version,
            full_version: version.to_string(),
            vendor: String::new(),
            arch: arch.to_string(),
        }
    }

    #[test]
    fn picks_exact_then_nearest_newer() {
        let list = vec![
            runtime(25, "amd64", "a"),
            runtime(21, "amd64", "b"),
            runtime(17, "x86", "d"),
            runtime(17, "amd64", "c"),
        ];
        let pick = |required| pick_java(&list, required).map(|r| r.path.clone());
        assert_eq!(pick(21), Some("b".into()));
        assert_eq!(pick(18), Some("b".into()));
        assert_eq!(pick(8), Some("c".into()));
        assert!(pick(26).is_none());
    }

    #[test]
    fn executables_are_found_in_nested_folders() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("vendor").join("jdk-21").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(JAVA_EXE), b"").unwrap();

        let mut out = Vec::new();
        find_java_executables(dir.path(), 1, &mut out);
        assert!(out.is_empty());
        find_java_executables(dir.path(), 2, &mut out);
        assert_eq!(out, [bin.join(JAVA_EXE)]);
    }

    #[tokio::test]
    #[ignore]
    async fn scan_this_machine() {
        for java in scan_java(None).await {
            println!(
                "Java {:<3} {:<12} {:<24} {:<8} {}",
                java.version,
                java.full_version,
                java.vendor,
                java.arch,
                java.path.display()
            );
        }
    }
}
