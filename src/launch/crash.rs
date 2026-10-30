//! Crash analysis: match the game output, the crash report and the JVM error report
//! against known failure patterns.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

/// A likely cause of a crash. Detail lists are copied from the logs and capped at
/// [`MAX_DETAILS`] entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CrashCause {
    /// Classes need a newer Java. `required_java` comes from the class file version.
    JavaTooOld { required_java: Option<u32> },
    /// An old loader (LaunchWrapper, old ASM) running on a Java it does not support.
    JavaTooNew,
    /// The heap ran out while running.
    OutOfMemory,
    /// The JVM could not reserve the requested heap (too large, or a 32-bit Java).
    HeapTooLarge,
    /// OpenGL context creation failed or the graphics driver crashed.
    GraphicsDriver,
    MissingDependencies { details: Vec<String> },
    IncompatibleMods { details: Vec<String> },
    DuplicateMods { details: Vec<String> },
    /// A Mixin failed to apply; `mods` holds the mod ids named by the error.
    MixinFailure { mods: Vec<String> },
    CorruptedModFile { files: Vec<String> },
    CorruptedConfig { files: Vec<String> },
    /// Mods the crash report blames.
    SuspectedMods { mods: Vec<String> },
    /// OpenJ9 cannot run modern Minecraft.
    OpenJ9,
    /// The JVM itself crashed; `frame` is the problematic native frame when known.
    NativeCrash { frame: Option<String> },
}

pub const MAX_DETAILS: usize = 8;

/// Crash files written by the run that started at `since`.
#[derive(Debug, Clone, Default)]
pub struct CrashFiles {
    /// Newest `crash-reports/*.txt`.
    pub crash_report: Option<PathBuf>,
    /// Newest `hs_err_pid*.log` in the game directory.
    pub jvm_report: Option<PathBuf>,
}

/// Find the crash report and JVM error report modified at or after `since`.
pub fn find_crash_files(game_dir: &Path, since: SystemTime) -> CrashFiles {
    CrashFiles {
        crash_report: newest(&game_dir.join("crash-reports"), since, |name| {
            name.ends_with(".txt")
        }),
        jvm_report: newest(game_dir, since, |name| {
            name.starts_with("hs_err_pid") && name.ends_with(".log")
        }),
    }
}

fn newest(dir: &Path, since: SystemTime, wanted: impl Fn(&str) -> bool) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| wanted(&e.file_name().to_string_lossy()))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            (modified >= since).then(|| (modified, e.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Analyze a crash. `log` is the game output; the reports are optional.
/// Causes come back in a fixed order, most specific first, without duplicates.
pub fn analyze_crash(
    log: &str,
    crash_report: Option<&str>,
    jvm_report: Option<&str>,
) -> Vec<CrashCause> {
    let text = [Some(log), crash_report, jvm_report]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    let lines: Vec<&str> = text.lines().collect();
    let mut causes = Vec::new();

    let missing = lines_matching(&lines, |l| l.contains("which is missing"));
    let forge_deps = indented_after(&lines, "Missing or unsupported mandatory dependencies");
    let (forge_missing, forge_wrong): (Vec<String>, Vec<String>) =
        forge_deps.into_iter().partition(|l| l.contains("[MISSING]"));
    let missing = merge(missing, forge_missing);
    if !missing.is_empty() {
        causes.push(CrashCause::MissingDependencies { details: missing });
    }

    let incompatible = merge(
        lines_matching(&lines, |l| {
            l.contains("is incompatible with")
                || l.contains("but only the wrong version is present")
                || l.contains("which is not supported")
        }),
        forge_wrong,
    );
    if !incompatible.is_empty() {
        causes.push(CrashCause::IncompatibleMods { details: incompatible });
    }

    let duplicates = indented_after_ci(&lines, "duplicate mod");
    if !duplicates.is_empty() || contains_ci(&text, "DuplicateModsFoundException") {
        causes.push(CrashCause::DuplicateMods { details: duplicates });
    }

    if let Some(mods) = mixin_failure(&lines) {
        causes.push(CrashCause::MixinFailure { mods });
    }

    let corrupted = capture_between(&lines, "File ", " is not a valid mod file");
    if !corrupted.is_empty()
        || text.contains("zip END header not found")
        || text.contains("Invalid or corrupt jarfile")
    {
        causes.push(CrashCause::CorruptedModFile { files: corrupted });
    }

    let configs = capture_between(&lines, "Failed loading config file ", " of type");
    if !configs.is_empty() {
        causes.push(CrashCause::CorruptedConfig { files: configs });
    }

    if let Some(required_java) = class_version_too_new(&text) {
        causes.push(CrashCause::JavaTooOld { required_java });
    }
    if text.contains("cannot be cast to class java.net.URLClassLoader")
        || text.contains("Unsupported class file major version")
        || text.contains("java.lang.NoSuchFieldException: ucp")
    {
        causes.push(CrashCause::JavaTooNew);
    }

    if text.contains("Could not reserve enough space for")
        || text.contains("Invalid maximum heap size")
        || text.contains("There is insufficient memory for the Java Runtime Environment")
    {
        causes.push(CrashCause::HeapTooLarge);
    } else if text.contains("java.lang.OutOfMemoryError") {
        causes.push(CrashCause::OutOfMemory);
    }

    let frame = jvm_report.and_then(problematic_frame);
    if is_graphics_failure(&text, frame.as_deref()) {
        causes.push(CrashCause::GraphicsDriver);
    }

    if text.contains("OpenJ9 is not supported") || text.contains("Eclipse OpenJ9 VM") {
        causes.push(CrashCause::OpenJ9);
    }

    let suspected = suspected_mods(&lines);
    if !suspected.is_empty() {
        causes.push(CrashCause::SuspectedMods { mods: suspected });
    }

    if jvm_report.is_some() && !causes.contains(&CrashCause::GraphicsDriver) {
        causes.push(CrashCause::NativeCrash { frame });
    }

    causes
}

fn contains_ci(text: &str, needle: &str) -> bool {
    text.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

fn clean(line: &str) -> String {
    let line = line.trim();
    let line = line.strip_prefix("- ").unwrap_or(line);
    let line = match line.find("]: ") {
        Some(i) if line.starts_with('[') => &line[i + 3..],
        _ => line,
    };
    line.trim().to_string()
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !value.is_empty() && list.len() < MAX_DETAILS && !list.contains(&value) {
        list.push(value);
    }
}

fn merge(mut a: Vec<String>, b: Vec<String>) -> Vec<String> {
    for value in b {
        push_unique(&mut a, value);
    }
    a
}

fn lines_matching(lines: &[&str], wanted: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines.iter().filter(|l| wanted(l)) {
        push_unique(&mut out, clean(line));
    }
    out
}

/// Indented lines following every line that contains `header`.
fn indented_after(lines: &[&str], header: &str) -> Vec<String> {
    indented_after_by(lines, |l| l.contains(header))
}

fn indented_after_ci(lines: &[&str], header: &str) -> Vec<String> {
    let header = header.to_ascii_lowercase();
    indented_after_by(lines, |l| l.to_ascii_lowercase().contains(&header))
}

fn indented_after_by(lines: &[&str], is_header: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !is_header(line) {
            continue;
        }
        for next in &lines[i + 1..] {
            let indented = next.starts_with(char::is_whitespace) || next.trim_start().starts_with("- ");
            if !indented || next.trim().is_empty() || next.trim_start().starts_with("at ") {
                break;
            }
            push_unique(&mut out, clean(next));
        }
    }
    out
}

fn capture_between(lines: &[&str], start: &str, end: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        let Some(from) = line.find(start) else { continue };
        let rest = &line[from + start.len()..];
        if let Some(to) = rest.find(end) {
            push_unique(&mut out, rest[..to].trim().to_string());
        }
    }
    out
}

fn mixin_failure(lines: &[&str]) -> Option<Vec<String>> {
    let mut found = false;
    let mut mods = Vec::new();
    for line in lines {
        let is_mixin = line.contains("Mixin apply")
            || line.contains("MixinApplyError")
            || line.contains("InvalidInjectionException")
            || line.contains("InjectionError")
            || line.contains("Critical injection failure");
        if !is_mixin {
            continue;
        }
        found = true;
        for marker in ["Mixin apply for mod ", "from mod "] {
            if let Some(i) = line.find(marker) {
                let id: String = line[i + marker.len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                    .collect();
                let id = id.trim_end_matches('.').to_string();
                push_unique(&mut mods, id);
            }
        }
    }
    found.then_some(mods)
}

/// `UnsupportedClassVersionError ... (class file version 65.0)` -> Some(Some(21)).
fn class_version_too_new(text: &str) -> Option<Option<u32>> {
    if !text.contains("UnsupportedClassVersionError") {
        return None;
    }
    let marker = "class file version ";
    let required = text.find(marker).and_then(|i| {
        let digits: String = text[i + marker.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse::<u32>().ok().filter(|v| *v > 44).map(|v| v - 44)
    });
    Some(required)
}

fn problematic_frame(jvm_report: &str) -> Option<String> {
    let mut lines = jvm_report.lines();
    lines.find(|l| l.contains("Problematic frame:"))?;
    let frame = lines.next()?.trim_start_matches('#').trim();
    (!frame.is_empty()).then(|| frame.to_string())
}

const GRAPHICS_PATTERNS: [&str; 7] = [
    "Pixel format not accelerated",
    "GLFW error 65542",
    "GLFW error 65543",
    "The driver does not appear to support OpenGL",
    "Couldn't set pixel format",
    "Failed to create the GLFW window",
    "WGL: The driver does not appear to support OpenGL",
];

const DRIVER_LIBRARIES: [&str; 7] = [
    "atio6axx", "atioglxx", "nvoglv", "ig9icd", "ig75icd", "ig7icd", "igxelpicd",
];

fn is_graphics_failure(text: &str, frame: Option<&str>) -> bool {
    GRAPHICS_PATTERNS.iter().any(|p| text.contains(p))
        || frame.is_some_and(|f| {
            let f = f.to_ascii_lowercase();
            DRIVER_LIBRARIES.iter().any(|d| f.contains(d))
        })
}

fn suspected_mods(lines: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let Some(rest) = trimmed
            .strip_prefix("Suspected Mods:")
            .or_else(|| trimmed.strip_prefix("Suspected Mod:"))
            .or_else(|| trimmed.strip_prefix("Suspected Mod(s):"))
        else {
            continue;
        };
        let rest = rest.trim();
        if !rest.is_empty() {
            if !rest.eq_ignore_ascii_case("none") && !rest.eq_ignore_ascii_case("unknown") {
                push_unique(&mut out, rest.to_string());
            }
            continue;
        }
        for next in &lines[i + 1..] {
            if !next.starts_with(char::is_whitespace) || next.trim().is_empty() {
                break;
            }
            if next.contains("Version:") {
                push_unique(&mut out, next.trim().to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fabric_missing_dependency() {
        let log = "[main/ERROR]: Incompatible mods found!\n\
            net.fabricmc.loader.impl.FormattedException: Some of your mods are incompatible with the game or each other!\n\
            A potential solution has been determined:\n\
            \t - Install fabric-api, any version.\n\
            Unmet dependency listing:\n\
            \t - Mod 'Sodium Extra' (sodium-extra) 0.5.1 requires any version of fabric-api, which is missing!";
        assert_eq!(
            analyze_crash(log, None, None),
            vec![CrashCause::MissingDependencies {
                details: vec![
                    "Mod 'Sodium Extra' (sodium-extra) 0.5.1 requires any version of fabric-api, which is missing!"
                        .into()
                ]
            }]
        );
    }

    #[test]
    fn forge_dependencies_split_missing_and_wrong() {
        let log = "Missing or unsupported mandatory dependencies:\n\
            \tMod ID: 'architectury', Requested by: 'rei', Expected range: '[9.1.12,)', Actual version: '[MISSING]'\n\
            \tMod ID: 'minecraft', Requested by: 'create', Expected range: '[1.20.1]', Actual version: '1.20.4'\n\
            next line";
        let causes = analyze_crash(log, None, None);
        assert!(matches!(&causes[0], CrashCause::MissingDependencies { details } if details.len() == 1 && details[0].contains("architectury")));
        assert!(matches!(&causes[1], CrashCause::IncompatibleMods { details } if details[0].contains("'create'")));
    }

    #[test]
    fn java_versions() {
        let log = "java.lang.UnsupportedClassVersionError: net/minecraft/client/main/Main has been compiled by a more recent version of the Java Runtime (class file version 65.0), this version of the Java Runtime only recognizes class file versions up to 52.0";
        assert_eq!(
            analyze_crash(log, None, None),
            vec![CrashCause::JavaTooOld { required_java: Some(21) }]
        );
        let log = "java.lang.ClassCastException: class jdk.internal.loader.ClassLoaders$AppClassLoader cannot be cast to class java.net.URLClassLoader";
        assert_eq!(analyze_crash(log, None, None), vec![CrashCause::JavaTooNew]);
    }

    #[test]
    fn memory() {
        assert_eq!(
            analyze_crash("Error occurred during initialization of VM\nCould not reserve enough space for 8388608KB object heap", None, None),
            vec![CrashCause::HeapTooLarge]
        );
        assert_eq!(
            analyze_crash("java.lang.OutOfMemoryError: Java heap space", None, None),
            vec![CrashCause::OutOfMemory]
        );
    }

    #[test]
    fn mixin_and_suspected_mods() {
        let log = "org.spongepowered.asm.mixin.transformer.throwables.MixinTransformerError: An unexpected critical error was encountered\n\
            Caused by: org.spongepowered.asm.mixin.throwables.MixinApplyError: Mixin [sodium.mixins.json:core.MixinWindow] from mod sodium failed injection check";
        let report = "Suspected Mods: \n\tCreate (create), Version: 0.5.1\n\t\tIssue tracker URL: https://example.com\n\nStacktrace:";
        let causes = analyze_crash(log, Some(report), None);
        assert_eq!(
            causes,
            vec![
                CrashCause::MixinFailure { mods: vec!["sodium".into()] },
                CrashCause::SuspectedMods { mods: vec!["Create (create), Version: 0.5.1".into()] },
            ]
        );
    }

    #[test]
    fn duplicates_and_corrupted_files() {
        let log = "Found duplicate mods:\n\tMod ID: 'jei' from mod files: jei-1.jar, jei-2.jar\n\
            File mods/broken.jar is not a valid mod file\n\
            Failed loading config file create-common.toml of type COMMON for modid create";
        assert_eq!(
            analyze_crash(log, None, None),
            vec![
                CrashCause::DuplicateMods {
                    details: vec!["Mod ID: 'jei' from mod files: jei-1.jar, jei-2.jar".into()]
                },
                CrashCause::CorruptedModFile { files: vec!["mods/broken.jar".into()] },
                CrashCause::CorruptedConfig { files: vec!["create-common.toml".into()] },
            ]
        );
    }

    #[test]
    fn native_crashes() {
        let driver = "# A fatal error has been detected\n# Problematic frame:\n# C  [atio6axx.dll+0x1a2b3c]\n";
        assert_eq!(analyze_crash("", None, Some(driver)), vec![CrashCause::GraphicsDriver]);

        let other = "# Problematic frame:\n# V  [jvm.dll+0x123]\n";
        assert_eq!(
            analyze_crash("", None, Some(other)),
            vec![CrashCause::NativeCrash { frame: Some("V  [jvm.dll+0x123]".into()) }]
        );
    }

    #[test]
    fn clean_exit_has_no_causes() {
        assert!(analyze_crash("[Render thread/INFO]: Stopping!", None, None).is_empty());
    }

    #[test]
    fn finds_newest_reports_since_start() {
        let temp = tempfile::tempdir().unwrap();
        let before = SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::create_dir_all(temp.path().join("crash-reports")).unwrap();
        std::fs::write(temp.path().join("crash-reports/crash-1.txt"), "x").unwrap();
        std::fs::write(temp.path().join("hs_err_pid42.log"), "x").unwrap();
        std::fs::write(temp.path().join("latest.log"), "x").unwrap();

        let files = find_crash_files(temp.path(), before);
        assert_eq!(files.crash_report, Some(temp.path().join("crash-reports/crash-1.txt")));
        assert_eq!(files.jvm_report, Some(temp.path().join("hs_err_pid42.log")));

        let later = SystemTime::now() + std::time::Duration::from_secs(60);
        let files = find_crash_files(temp.path(), later);
        assert!(files.crash_report.is_none() && files.jvm_report.is_none());
    }
}
