use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
};

use sha1::{Digest, Sha1};

use crate::{
    download::{download_batch, DownloadTask},
    error::{HexoError, Result},
    install::vanilla::{InstanceConfig, LibEntry},
};

/// A library that replaces a Mojang-provided one on Apple Silicon.
#[derive(Debug, Clone, PartialEq)]
struct Lib {
    name: String,
    /// Relative to the `libraries/` dir.
    path: String,
    url: String,
    sha1: String,
}

impl Lib {
    /// `url: None` means Mojang's library server, at the maven `path`.
    fn new(name: &str, path: &str, url: Option<&str>, sha1: &str) -> Self {
        let url = url.map_or_else(|| format!("{MOJANG_LIBRARIES}{path}"), str::to_owned);
        Self { name: name.into(), path: path.into(), url, sha1: sha1.into() }
    }

    fn entry(&self, lib_dir: &Path) -> LibEntry {
        LibEntry { name: self.name.clone(), path: lib_dir.join(&self.path), sha1: self.sha1.clone() }
    }
}

const MOJANG_LIBRARIES: &str = "https://libraries.minecraft.net/";

/// A [`Lib`] spelled out at compile time; see [`Lib::new`] for `url`.
#[derive(Debug)]
struct StaticLib {
    name: &'static str,
    path: &'static str,
    url: Option<&'static str>,
    sha1: &'static str,
}

impl StaticLib {
    fn lib(&self) -> Lib {
        Lib::new(self.name, self.path, self.url, self.sha1)
    }
}

const LWJGL2: StaticLib = StaticLib {
    name: "org.lwjgl.lwjgl:lwjgl:2.9.4-nightly-20150209",
    path: "org/lwjgl/lwjgl/lwjgl/2.9.4-nightly-20150209/lwjgl-2.9.4-nightly-20150209.jar",
    url: None,
    sha1: "697517568c68e78ae0b4544145af031c81082dfe",
};

const LWJGL2_UTIL: StaticLib = StaticLib {
    name: "org.lwjgl.lwjgl:lwjgl_util:2.9.4-nightly-20150209",
    path: "org/lwjgl/lwjgl/lwjgl_util/2.9.4-nightly-20150209/lwjgl_util-2.9.4-nightly-20150209.jar",
    url: None,
    sha1: "d51a7c040a721d13efdfbd34f8b257b2df882ad0",
};

/// arm64 `liblwjgl.dylib` + `openal.dylib`. Stored under an `-arm64` classifier
/// so it never collides with Mojang's x86_64 jar of the same version.
const LWJGL2_NATIVES: StaticLib = StaticLib {
    name: "org.lwjgl.lwjgl:lwjgl-platform:2.9.4-nightly-20150209",
    path: "org/lwjgl/lwjgl/lwjgl-platform/2.9.4-nightly-20150209/lwjgl-platform-2.9.4-nightly-20150209-natives-osx-arm64.jar",
    url: Some("https://github.com/MinecraftMachina/lwjgl/releases/download/2.9.4-20150209-mmachina.2/lwjgl-platform-2.9.4-nightly-20150209-natives-osx.jar"),
    sha1: "eff546c0b319d6ffc7a835652124c18089c67f36",
};

const JINPUT_NATIVES: StaticLib = StaticLib {
    name: "net.java.jinput:jinput-platform:2.0.5",
    path: "net/java/jinput/jinput-platform/2.0.5/jinput-platform-2.0.5-natives-osx-arm64.jar",
    url: Some("https://github.com/r58Playz/jinput-m1/raw/main/plugins/OSX/bin/jinput-platform-2.0.5.jar"),
    sha1: "5189eb40db3087fb11ca063b68fa4f4c20b199dd",
};

/// MinecraftMachina's build of `java-objc-bridge` 1.1, with a universal
/// `libjcocoa.dylib` bundled in the class jar. Mojang's own 1.1 (from 1.19)
/// targets Java 11, and 1.12.x / 1.13.x crash at startup when the narrator can't
/// load it on Java 8; this build targets Java 8. Stored under its own version so
/// it never collides with Mojang's 1.1.
const OBJC_BRIDGE: StaticLib = StaticLib {
    name: "ca.weblite:java-objc-bridge:1.1.0-mmachina.1",
    path: "ca/weblite/java-objc-bridge/1.1.0-mmachina.1/java-objc-bridge-1.1.0-mmachina.1.jar",
    url: Some("https://github.com/MinecraftMachina/Java-Objective-C-Bridge/releases/download/1.1.0-mmachina.1/java-objc-bridge-1.1.jar"),
    sha1: "369a83621e3c65496348491e533cb97fe5f2f37d",
};

/// JNA 5.10.0 (from 1.18.2), the replacement for 1.12 – 1.16.5's JNA 4.4.0, whose
/// bundled dispatch library is x86_64 only. objc-bridge loads through JNA, and
/// 1.12.x / 1.13.x don't survive that failing. Darwin arm64 support arrived in JNA 5.7.
const JNA: StaticLib = StaticLib {
    name: "net.java.dev.jna:jna:5.10.0",
    path: "net/java/dev/jna/jna/5.10.0/jna-5.10.0.jar",
    url: None,
    sha1: "7cf4c87dd802db50721db66947aa237d7ad09418",
};

/// Replaces `net.java.dev.jna:platform:3.4.0`, which must match the JNA version.
const JNA_PLATFORM: StaticLib = StaticLib {
    name: "net.java.dev.jna:jna-platform:5.10.0",
    path: "net/java/dev/jna/jna-platform/5.10.0/jna-platform-5.10.0.jar",
    url: None,
    sha1: "fbed7d9669dba47714ad0d4f4454290a997aee69",
};

const STATIC_LIBS: [&StaticLib; 7] =
    [&LWJGL2, &LWJGL2_UTIL, &LWJGL2_NATIVES, &JINPUT_NATIVES, &OBJC_BRIDGE, &JNA, &JNA_PLATFORM];

const LWJGL3_VERSION: &str = "3.3.1";

/// LWJGL 3.3.1 modules used by 1.13 – 1.18.2: (artifact, class jar sha1,
/// `natives-macos-arm64` jar sha1), as listed in Mojang's 1.19 version json.
const LWJGL3_MODULES: [(&str, &str, &str); 7] = [
    ("lwjgl", "ae58664f88e18a9bb2c77b063833ca7aaec484cb", "71d0d5e469c9c95351eb949064497e3391616ac9"),
    ("lwjgl-jemalloc", "a817bcf213db49f710603677457567c37d53e103", "e577b87d8ad2ade361aaea2fcf226c660b15dee8"),
    ("lwjgl-openal", "2623a6b8ae1dfcd880738656a9f0243d2e6840bd", "23d55e7490b57495320f6c9e1936d78fd72c4ef8"),
    ("lwjgl-opengl", "831a5533a21a5f4f81bbc51bb13e9899319b5411", "eafe34b871d966292e8db0f1f3d6b8b110d4e91d"),
    ("lwjgl-glfw", "cbac1b8d30cb4795149c1ef540f912671a8616d0", "cac0d3f712a3da7641fa174735a5f315de7ffe0a"),
    ("lwjgl-stb", "b119297cf8ed01f247abe8685857f8e7fcf5980f", "fcf073ed911752abdca5f0b00a53cfdf17ff8e8b"),
    ("lwjgl-tinyfd", "0ff1914111ef2e3e0110ef2dabc8d8cdaad82347", "972ecc17bad3571e81162153077b4d47b7b9eaa9"),
];

fn lwjgl3_jar(module: &str) -> Option<Lib> {
    let (_, sha1, _) = LWJGL3_MODULES.iter().find(|(m, _, _)| *m == module)?;
    let v = LWJGL3_VERSION;
    Some(Lib::new(
        &format!("org.lwjgl:{module}:{v}"),
        &format!("org/lwjgl/{module}/{v}/{module}-{v}.jar"),
        None,
        sha1,
    ))
}

fn lwjgl3_natives(module: &str) -> Option<Lib> {
    let (_, _, sha1) = LWJGL3_MODULES.iter().find(|(m, _, _)| *m == module)?;
    let v = LWJGL3_VERSION;
    Some(Lib::new(
        &format!("org.lwjgl:{module}:{v}:natives-macos-arm64"),
        &format!("org/lwjgl/{module}/{v}/{module}-{v}-natives-macos-arm64.jar"),
        None,
        sha1,
    ))
}

/// Every override, for finding the ones a config references.
fn all_overrides() -> Vec<Lib> {
    let lwjgl3 = LWJGL3_MODULES.iter().flat_map(|(m, _, _)| lwjgl3_jar(m).into_iter().chain(lwjgl3_natives(m)));
    STATIC_LIBS.iter().map(|l| l.lib()).chain(lwjgl3).collect()
}

/// Which list of an [`InstanceConfig`] an entry is in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum List {
    /// `lib_list`, the classpath.
    Class,
    /// `natives`, extracted into `natives/`.
    Native,
}

/// What a [`Rule`] matches on the entry's classifier.
#[derive(Debug, Clone, Copy)]
enum Classifier {
    Any,
    Absent,
    Present,
    Prefix(&'static str),
}

/// The library a [`Rule`] swaps in.
#[derive(Debug, Clone, Copy)]
enum Target {
    Static(&'static StaticLib),
    /// LWJGL 3.3.1's class jar for the matched module.
    Lwjgl3Jar,
    /// LWJGL 3.3.1's `natives-macos-arm64` jar for the matched module.
    Lwjgl3Natives,
}

impl Target {
    /// `None` for an LWJGL module 3.3.1 doesn't have.
    fn resolve(self, artifact: &str) -> Option<Lib> {
        match self {
            Target::Static(lib) => Some(lib.lib()),
            Target::Lwjgl3Jar => lwjgl3_jar(artifact),
            Target::Lwjgl3Natives => lwjgl3_natives(artifact),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Action {
    /// Replace the entry, in the same list.
    Replace(Target),
    /// Remove the entry and put the target on the classpath instead (LWJGL 3.3
    /// loads its natives from the classpath).
    ToClasspath(Target),
    /// Remove the entry; the replacement class jar already carries the native.
    Drop,
}

struct Rule {
    list: List,
    /// `group:artifact`; an artifact of `*` matches any.
    id: &'static str,
    /// Version prefixes, any of which matches.
    versions: &'static [&'static str],
    classifier: Classifier,
    action: Action,
}

const fn rule(list: List, id: &'static str, versions: &'static [&'static str], classifier: Classifier, action: Action) -> Rule {
    Rule { list, id, versions, classifier, action }
}

const OLD_LWJGL3: &[&str] = &["3.1.", "3.2."];

/// The first rule matching an entry decides what happens to it; entries no rule
/// matches are left alone.
#[rustfmt::skip]
const RULES: &[Rule] = {
    use Action::*;
    use Classifier::*;
    use List::*;
    use Target::*;
    &[
        rule(Class,  "org.lwjgl.lwjgl:lwjgl",           &["2."],       Any,     Replace(Static(&LWJGL2))),
        rule(Class,  "org.lwjgl.lwjgl:lwjgl_util",      &["2."],       Any,     Replace(Static(&LWJGL2_UTIL))),
        rule(Class,  "org.lwjgl:*",                     OLD_LWJGL3,    Absent,  Replace(Lwjgl3Jar)),
        // Some importers list macOS natives as classpath entries.
        rule(Class,  "org.lwjgl:*",                     OLD_LWJGL3,    Prefix("natives-macos"), ToClasspath(Lwjgl3Natives)),
        rule(Class,  "ca.weblite:java-objc-bridge",     &["1.0"],      Absent,  Replace(Static(&OBJC_BRIDGE))),
        rule(Class,  "ca.weblite:java-objc-bridge",     &["1.0"],      Present, Drop),
        rule(Class,  "net.java.dev.jna:jna",            &["3.", "4."], Any,     Replace(Static(&JNA))),
        rule(Class,  "net.java.dev.jna:platform",       &["3."],       Any,     Replace(Static(&JNA_PLATFORM))),
        rule(Native, "org.lwjgl.lwjgl:lwjgl-platform",  &["2."],       Any,     Replace(Static(&LWJGL2_NATIVES))),
        rule(Native, "net.java.jinput:jinput-platform", &["2.0."],     Any,     Replace(Static(&JINPUT_NATIVES))),
        rule(Native, "org.lwjgl:*",                     OLD_LWJGL3,    Any,     ToClasspath(Lwjgl3Natives)),
        rule(Native, "ca.weblite:java-objc-bridge",     &["1.0"],      Any,     Drop),
    ]
};

impl Rule {
    fn matches(&self, list: List, c: &Coordinate) -> bool {
        let (group, artifact) = self.id.split_once(':').expect("rule id is group:artifact");
        self.list == list
            && group == c.group
            && (artifact == "*" || artifact == c.artifact)
            && self.versions.iter().any(|v| c.version.starts_with(v))
            && match self.classifier {
                Classifier::Any => true,
                Classifier::Absent => c.classifier.is_none(),
                Classifier::Present => c.classifier.is_some(),
                Classifier::Prefix(p) => c.classifier.is_some_and(|cls| cls.starts_with(p)),
            }
    }
}

/// A matched [`Rule`], resolved for one entry.
#[derive(Debug, PartialEq)]
enum Fix {
    Replace(Lib),
    ToClasspath(Lib),
    Drop,
}

fn fix_for(list: List, name: &str) -> Option<Fix> {
    let c = parse(name)?;
    let rule = RULES.iter().find(|r| r.matches(list, &c))?;
    match rule.action {
        Action::Replace(target) => target.resolve(c.artifact).map(Fix::Replace),
        Action::ToClasspath(target) => target.resolve(c.artifact).map(Fix::ToClasspath),
        Action::Drop => Some(Fix::Drop),
    }
}

/// Whether this build runs on Apple Silicon, where old natives need replacing.
pub fn is_arm64_mac() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

/// What [`ensure_arm64_lwjgl`] did to an instance.
#[derive(Debug, Default)]
pub struct Arm64Fixes {
    /// The config was rewritten; the caller should save it.
    pub config_changed: bool,
    /// Extra JVM args the launch needs (e.g. the patched GLFW path).
    pub jvm_args: Vec<String>,
}

/// On Apple Silicon, point `config`'s x86_64-only LWJGL / jinput / objc-bridge
/// entries at arm64 builds, download any that are missing, and prepare the
/// patched GLFW (see [`patched_glfw`]). A no-op on every other platform.
pub async fn ensure_arm64_lwjgl(config: &mut InstanceConfig, lib_dir: &Path) -> Result<Arm64Fixes> {
    if !is_arm64_mac() {
        return Ok(Arm64Fixes::default());
    }
    let config_changed = rewrite_for_arm64(config, lib_dir);

    let tasks: Vec<DownloadTask> = all_overrides()
        .into_iter()
        .map(|o| {
            let path = lib_dir.join(&o.path);
            (o, path)
        })
        .filter(|(_, path)| {
            !path.exists()
                && (config.lib_list.iter().any(|e| &e.path == path)
                    || config.natives.iter().any(|e| &e.path == path))
        })
        .map(|(o, path)| DownloadTask::new(o.url, path).with_sha1(o.sha1))
        .collect();
    download_batch(tasks, 8, |_, _| {}).await?;

    let mut jvm_args = Vec::new();
    if let Some(glfw) = patched_glfw(config, lib_dir)? {
        jvm_args.push(format!("-Dorg.lwjgl.glfw.libname={}", glfw.display()));
    }
    Ok(Arm64Fixes { config_changed, jvm_args })
}

/// `libglfw.dylib` inside Mojang's `lwjgl-glfw-3.3.1-natives-macos-arm64.jar`.
const GLFW_DYLIB_ENTRY: &str = "macos/arm64/org/lwjgl/glfw/libglfw.dylib";
const GLFW_DYLIB_SHA1: &str = "d94528289a934fa1fca6f37a26aacedf3308891a";
/// File offset of Cocoa's `_glfwSetWindowIconCocoa`, whose whole body is
/// `_glfwInputError(GLFW_FEATURE_UNAVAILABLE, "Cocoa: Regular windows do not have icons on macOS")`.
const GLFW_SET_ICON_OFFSET: usize = 0x14568;
/// `adr x1, <message>` — the function's first instruction.
const GLFW_SET_ICON_ORIGINAL: [u8; 4] = [0x41, 0x58, 0x08, 0x10];
/// `ret`
const ARM64_RET: [u8; 4] = [0xc0, 0x03, 0x5f, 0xd6];

/// LWJGL 3.3's GLFW (3.4) raises `GLFW_FEATURE_UNAVAILABLE` when a window icon is
/// set on macOS, and 1.13 – 1.17.1 call `glfwSetWindowIcon` during startup with an
/// error callback that aborts window creation (`GLFW error 65548`). GLFW 3.3
/// silently ignored the call, so restore that: copy Mojang's arm64 `libglfw.dylib`,
/// turn `_glfwSetWindowIconCocoa` into a bare `ret`, ad-hoc re-sign it, and have
/// LWJGL load it via `org.lwjgl.glfw.libname`.
///
/// Returns the patched dylib's path, or `None` when `config` doesn't use that GLFW
/// build (LWJGL 2, or 3.3.2+ from 1.20.3 on).
fn patched_glfw(config: &InstanceConfig, lib_dir: &Path) -> Result<Option<PathBuf>> {
    let Some(natives) = lwjgl3_natives("lwjgl-glfw") else {
        return Ok(None);
    };
    let jar = lib_dir.join(&natives.path);
    if !config.lib_list.iter().any(|e| e.path == jar) {
        return Ok(None);
    }
    let out = jar.with_file_name("hexo-arm64-patched").join("libglfw.dylib");
    if std::fs::read(&out).is_ok_and(|d| d.get(GLFW_SET_ICON_OFFSET..GLFW_SET_ICON_OFFSET + 4) == Some(&ARM64_RET[..])) {
        return Ok(Some(out));
    }

    let mut dylib = Vec::new();
    {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&jar)?)
            .map_err(|e| HexoError::Other(format!("{}: {e}", jar.display())))?;
        let mut entry = archive
            .by_name(GLFW_DYLIB_ENTRY)
            .map_err(|e| HexoError::Other(format!("{}: {e}", jar.display())))?;
        entry.read_to_end(&mut dylib)?;
    }
    patch_set_window_icon(&mut dylib)?;

    let dir = out.parent().expect("has a parent");
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("libglfw.dylib.tmp");
    std::fs::write(&tmp, &dylib)?;
    // The edit invalidates the linker's ad-hoc signature, and arm64 macOS refuses
    // to map unsigned code. `-i` keeps the identifier independent of the tmp name.
    let status = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "-i", "libglfw.3.4.dylib"])
        .arg(&tmp)
        .status()?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(HexoError::Other(format!("codesign failed for {}: {status}", tmp.display())));
    }
    std::fs::rename(&tmp, &out)?;
    Ok(Some(out))
}

/// Patch Mojang's GLFW 3.3.1 arm64 dylib in place, refusing any other build.
fn patch_set_window_icon(dylib: &mut [u8]) -> Result<()> {
    let sha1 = hex::encode(Sha1::digest(&*dylib));
    let site = GLFW_SET_ICON_OFFSET..GLFW_SET_ICON_OFFSET + 4;
    if sha1 != GLFW_DYLIB_SHA1 || dylib.get(site.clone()) != Some(&GLFW_SET_ICON_ORIGINAL[..]) {
        return Err(HexoError::Other(format!("unexpected libglfw.dylib build (sha1 {sha1})")));
    }
    dylib[site].copy_from_slice(&ARM64_RET);
    Ok(())
}

/// Apply every matching [`Rule`], then drop duplicate paths (vanilla and legacy
/// Forge both list LWJGL, which now resolve to the same jar). Platform-independent
/// so it can be tested anywhere.
fn rewrite_for_arm64(config: &mut InstanceConfig, lib_dir: &Path) -> bool {
    let mut changed = false;
    let mut to_classpath = Vec::new();

    config.lib_list.retain_mut(|entry| match fix_for(List::Class, &entry.name) {
        // Already on the classpath, so moving there is a plain replacement.
        Some(Fix::Replace(lib) | Fix::ToClasspath(lib)) => {
            changed |= apply(&lib, lib_dir, &mut entry.name, &mut entry.path, &mut entry.sha1);
            true
        }
        Some(Fix::Drop) => {
            changed = true;
            false
        }
        None => true,
    });

    config.natives.retain_mut(|entry| match fix_for(List::Native, &entry.name) {
        Some(Fix::Replace(lib)) => {
            changed |= apply(&lib, lib_dir, &mut entry.name, &mut entry.path, &mut entry.sha1);
            true
        }
        Some(Fix::ToClasspath(lib)) => {
            to_classpath.push(lib.entry(lib_dir));
            changed = true;
            false
        }
        Some(Fix::Drop) => {
            changed = true;
            false
        }
        None => true,
    });
    config.lib_list.extend(to_classpath);

    let (libs, natives) = (config.lib_list.len(), config.natives.len());
    let mut seen = HashSet::new();
    config.lib_list.retain(|e| seen.insert(e.path.clone()));
    let mut seen = HashSet::new();
    config.natives.retain(|e| seen.insert(e.path.clone()));
    changed || libs != config.lib_list.len() || natives != config.natives.len()
}

fn apply(lib: &Lib, lib_dir: &Path, name: &mut String, path: &mut PathBuf, sha1: &mut String) -> bool {
    let new_path = lib_dir.join(&lib.path);
    if *path == new_path && *name == lib.name {
        return false;
    }
    *name = lib.name.clone();
    *path = new_path;
    *sha1 = lib.sha1.clone();
    true
}

/// `group:artifact:version[:classifier]`.
struct Coordinate<'a> {
    group: &'a str,
    artifact: &'a str,
    version: &'a str,
    classifier: Option<&'a str>,
}

fn parse(name: &str) -> Option<Coordinate<'_>> {
    let mut parts = name.split(':');
    Some(Coordinate {
        group: parts.next()?,
        artifact: parts.next()?,
        version: parts.next()?,
        classifier: parts.next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::vanilla::{LoaderType, NativeEntry};

    fn lib(name: &str, path: &str) -> LibEntry {
        LibEntry { name: name.into(), path: PathBuf::from(path), sha1: String::new() }
    }

    fn native(name: &str, path: &str) -> NativeEntry {
        NativeEntry { name: name.into(), path: PathBuf::from(path), sha1: String::new() }
    }

    fn config(version: &str, lib_list: Vec<LibEntry>, natives: Vec<NativeEntry>) -> InstanceConfig {
        InstanceConfig {
            version_id: version.into(),
            loader_type: LoaderType::Vanilla,
            main_class: "net.minecraft.client.main.Main".into(),
            start_args: vec![],
            lib_list,
            natives,
            assets_id: version.into(),
            java_version: 8,
        }
    }

    /// Roughly what a Forge 1.7.10 install leaves behind: Forge's LWJGL entries
    /// prepended to vanilla's.
    fn forge_1710_config() -> InstanceConfig {
        config(
            "1.7.10",
            vec![
                lib("net.minecraft:launchwrapper:1.12", "/l/launchwrapper.jar"),
                lib("org.lwjgl.lwjgl:lwjgl:2.9.1", "/l/forge-lwjgl-2.9.1.jar"),
                lib("org.lwjgl.lwjgl:lwjgl:2.9.1", "/l/lwjgl-2.9.1.jar"),
                lib("org.lwjgl.lwjgl:lwjgl_util:2.9.1", "/l/lwjgl_util-2.9.1.jar"),
                lib("org.lwjgl:lwjgl:3.3.3", "/l/lwjgl3.jar"),
            ],
            vec![
                native("org.lwjgl.lwjgl:lwjgl-platform:2.9.1", "/l/forge-platform.jar"),
                native("org.lwjgl.lwjgl:lwjgl-platform:2.9.1", "/l/lwjgl-platform-2.9.1-natives-osx.jar"),
                native("net.java.jinput:jinput-platform:2.0.5", "/l/jinput-platform-2.0.5-natives-osx.jar"),
            ],
        )
    }

    /// What a vanilla 1.16.5 install records on macOS.
    fn vanilla_1165_config() -> InstanceConfig {
        let mut libs = vec![lib("com.mojang:brigadier:1.0.17", "/l/brigadier.jar")];
        let mut natives = vec![];
        for (module, _, _) in LWJGL3_MODULES.iter().filter(|(m, _, _)| *m != "lwjgl-tinyfd") {
            libs.push(lib(&format!("org.lwjgl:{module}:3.2.1"), &format!("/l/{module}-3.2.1.jar")));
            natives.push(native(
                &format!("org.lwjgl:{module}:3.2.1"),
                &format!("/l/{module}-3.2.1-natives-macos.jar"),
            ));
        }
        libs.push(lib("ca.weblite:java-objc-bridge:1.0.0", "/l/objc-1.0.0.jar"));
        libs.push(lib("net.java.dev.jna:jna:4.4.0", "/l/jna-4.4.0.jar"));
        libs.push(lib("net.java.dev.jna:platform:3.4.0", "/l/platform-3.4.0.jar"));
        natives.push(native("ca.weblite:java-objc-bridge:1.0.0", "/l/objc-1.0.0-natives-osx.jar"));
        config("1.16.5", libs, natives)
    }

    fn names(entries: &[LibEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn lwjgl2_is_rewritten_and_deduplicated() {
        let lib_dir = Path::new("/l");
        let mut config = forge_1710_config();
        assert!(rewrite_for_arm64(&mut config, lib_dir));

        assert_eq!(
            names(&config.lib_list),
            [
                "net.minecraft:launchwrapper:1.12",
                LWJGL2.name,
                LWJGL2_UTIL.name,
                "org.lwjgl:lwjgl:3.3.3",
            ]
        );
        assert_eq!(config.lib_list[1].path, lib_dir.join(LWJGL2.path));

        let natives: Vec<_> = config.natives.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            natives,
            [lib_dir.join(LWJGL2_NATIVES.path), lib_dir.join(JINPUT_NATIVES.path)]
        );
    }

    #[test]
    fn lwjgl3_2_moves_to_3_3_1_with_arm64_natives_on_classpath() {
        let lib_dir = Path::new("/l");
        let mut config = vanilla_1165_config();
        assert!(rewrite_for_arm64(&mut config, lib_dir));

        assert!(config.natives.is_empty(), "{:?}", config.natives);
        let names = names(&config.lib_list);
        assert_eq!(names[0], "com.mojang:brigadier:1.0.17");
        assert!(names.contains(&"org.lwjgl:lwjgl:3.3.1"));
        assert!(names.contains(&"org.lwjgl:lwjgl-glfw:3.3.1:natives-macos-arm64"));
        assert!(names.contains(&"ca.weblite:java-objc-bridge:1.1.0-mmachina.1"));
        assert!(names.contains(&"net.java.dev.jna:jna:5.10.0"));
        assert!(names.contains(&"net.java.dev.jna:jna-platform:5.10.0"));
        assert!(!names.iter().any(|n| n.contains("3.2.1") || n.contains("objc-bridge:1.0")));
        // 6 modules × (class jar + natives) + brigadier + objc-bridge + 2 JNA.
        assert_eq!(names.len(), 6 * 2 + 4);
        let glfw = config.lib_list.iter().find(|e| e.name == "org.lwjgl:lwjgl-glfw:3.3.1").unwrap();
        assert_eq!(glfw.path, lib_dir.join("org/lwjgl/lwjgl-glfw/3.3.1/lwjgl-glfw-3.3.1.jar"));
    }

    #[test]
    fn rewrite_is_idempotent() {
        let lib_dir = Path::new("/l");
        for mut config in [forge_1710_config(), vanilla_1165_config()] {
            rewrite_for_arm64(&mut config, lib_dir);
            assert!(!rewrite_for_arm64(&mut config, lib_dir));
        }
    }

    #[test]
    fn arm64_ready_libraries_are_untouched() {
        for name in [
            "org.lwjgl:lwjgl:3.3.1",
            "org.lwjgl:lwjgl:3.3.3:natives-macos-arm64",
            "ca.weblite:java-objc-bridge:1.1",
            "net.java.jinput:jinput:2.0.5",
            "net.java.dev.jna:jna:5.8.0",
        ] {
            assert!(fix_for(List::Class, name).is_none(), "{name}");
            assert!(fix_for(List::Native, name).is_none(), "{name}");
        }
    }

    #[test]
    fn unknown_glfw_build_is_not_patched() {
        let mut dylib = vec![0u8; GLFW_SET_ICON_OFFSET + 4];
        dylib[GLFW_SET_ICON_OFFSET..].copy_from_slice(&GLFW_SET_ICON_ORIGINAL);
        let before = dylib.clone();
        assert!(patch_set_window_icon(&mut dylib).is_err());
        assert_eq!(dylib, before);
    }

    #[test]
    fn glfw_patch_needs_the_override_jar() {
        let config = forge_1710_config();
        assert!(patched_glfw(&config, Path::new("/l")).unwrap().is_none());
    }

    #[test]
    fn every_override_has_a_distinct_path() {
        let all = all_overrides();
        let paths: HashSet<_> = all.iter().map(|o| o.path.clone()).collect();
        assert_eq!(paths.len(), all.len());
    }
}
