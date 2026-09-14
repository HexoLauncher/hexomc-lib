use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;
use tokio::{fs, sync::mpsc};

use crate::{
    error::{HexoError, Result},
    install::vanilla::InstanceConfig,
    launch::output::{GameProcess, OutputFn, OutputLine},
};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Instance ID, maps to the `instance/{id}/` directory.
    pub instance_id: String,
    /// Path to the java executable.
    pub java_path: PathBuf,
    /// Player name (offline mode).
    pub player_name: String,
    /// Microsoft auth token (online; None = offline mode).
    pub auth_token: Option<String>,
    /// Player UUID.
    pub auth_uuid: Option<String>,
    /// Xbox XUID.
    pub xuid: Option<String>,
    /// Extra JVM args (e.g. `-Xmx4G`).
    pub jvm_extra_args: Vec<String>,
}

impl LaunchOptions {
    /// Build offline-mode options.
    pub fn offline(instance_id: impl Into<String>, java_path: PathBuf, player_name: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            java_path,
            player_name: player_name.into(),
            auth_token: None,
            auth_uuid: None,
            xuid: None,
            jvm_extra_args: vec![
                "-Xmx2G".to_string(),
                "-Xms512M".to_string(),
                "-Dfile.encoding=UTF-8".to_string(),
                "-Dstdout.encoding=UTF-8".to_string(),
                "-Dstderr.encoding=UTF-8".to_string(),
            ],
        }
    }
}

/// Launch Minecraft, returning the process handle.
///
/// stdout/stderr are inherited from the parent process. Use
/// [`launch_with_output`] or [`launch_with_channel`] to capture them instead.
///
/// The `@argfile` written into `.minecraft/` is deleted a few seconds after the
/// JVM starts, so the caller must stay alive at least that long for the cleanup
/// to happen.
pub async fn launch(options: &LaunchOptions, base_dir: &Path) -> Result<Child> {
    let Prepared { mut command, argfile } = prepare_command(options, base_dir).await?;

    finish_spawn(command.spawn(), argfile)
}

/// Launch Minecraft with stdout/stderr piped, delivering them line by line to
/// `on_output`.
///
/// The callback is invoked from tokio tasks (one per stream), so lines from
/// stdout and stderr may interleave; `OutputLine::kind` says which is which.
/// [`GameProcess::wait`] waits for the process *and* for every buffered line to
/// be delivered.
pub async fn launch_with_output(
    options: &LaunchOptions,
    base_dir: &Path,
    on_output: OutputFn,
) -> Result<GameProcess> {
    let Prepared { mut command, argfile } = prepare_command(options, base_dir).await?;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let child = finish_spawn(tokio::process::Command::from(command).spawn(), argfile)?;

    Ok(GameProcess::new(child, on_output))
}

/// Same as [`launch_with_output`], but pushes the lines into a channel instead
/// of a callback — convenient when the consumer is a UI loop or another task.
///
/// The channel is unbounded, so the game never blocks on a slow consumer; drop
/// the receiver if the output is no longer wanted.
pub async fn launch_with_channel(
    options: &LaunchOptions,
    base_dir: &Path,
) -> Result<(GameProcess, mpsc::UnboundedReceiver<OutputLine>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let sink: OutputFn = Arc::new(move |line| {
        let _ = tx.send(line);
    });

    let process = launch_with_output(options, base_dir, sink).await?;
    Ok((process, rx))
}

/// Name of the `@argfile`, written into `.minecraft/` (cwd of the game).
const ARGFILE_NAME: &str = "tempcmd.txt";

/// How long to wait before deleting the `@argfile`. The JVM reads it while
/// starting up, so it can't be removed immediately after spawn.
const ARGFILE_CLEANUP_DELAY: Duration = Duration::from_secs(10);

/// A java `Command` ready to spawn, plus the `@argfile` it depends on (None when
/// args were passed on the command line instead).
struct Prepared {
    command: Command,
    argfile: Option<PathBuf>,
}

/// Hand back the spawned child, scheduling the `@argfile` for deletion once the
/// JVM has had time to read it. A failed spawn never read it, so it goes now.
fn finish_spawn<T>(spawned: std::io::Result<T>, argfile: Option<PathBuf>) -> Result<T> {
    match spawned {
        Ok(child) => {
            if let Some(path) = argfile {
                tokio::spawn(remove_argfile_after(path, ARGFILE_CLEANUP_DELAY));
            }
            Ok(child)
        }
        Err(e) => {
            if let Some(path) = argfile {
                let _ = std::fs::remove_file(path);
            }
            Err(HexoError::Io(e))
        }
    }
}

/// Delete the `@argfile` once `delay` has passed; a missing file is not an error.
async fn remove_argfile_after(path: PathBuf, delay: Duration) {
    tokio::time::sleep(delay).await;
    let _ = fs::remove_file(&path).await;
}

/// Build the fully-configured java `Command` (classpath, natives, argfile, cwd)
/// without spawning it — shared by every `launch*` entry point.
async fn prepare_command(options: &LaunchOptions, base_dir: &Path) -> Result<Prepared> {
    // Make base_dir absolute (the argfile is read from minecraft_dir, so a relative
    // base_dir would misresolve) and strip the Windows `\\?\` extended-path prefix
    // left by canonicalize, which Java can't parse.
    let canon = base_dir.canonicalize()?;
    let base_str = canon.to_string_lossy().replace('\\', "/");
    let base_str = base_str.strip_prefix("//?/").unwrap_or(&base_str).to_string();
    let base_dir_buf = PathBuf::from(base_str);
    let base_dir = base_dir_buf.as_path();
    let instance_dir = base_dir.join("instance").join(&options.instance_id);
    let config = InstanceConfig::load(&instance_dir).await?;

    let natives_dir = instance_dir.join("natives");
    extract_natives(&config, &natives_dir)?;

    let classpath = build_classpath(&config, &instance_dir);
    let argdata = build_argdata(&options, base_dir, &instance_dir, &config, &classpath);

    let mut final_args: Vec<String> = Vec::new();
    final_args.extend(options.jvm_extra_args.iter().cloned());

    // Old-format start_args (e.g. 1.7.10) contain only game args — no -cp, natives,
    // or ${mainClass} — so we supply the JVM section ourselves.
    let is_legacy = !config.start_args.iter().any(|a| a == "${mainClass}");
    if is_legacy {
        for jvm_arg in [
            "-Djava.library.path=${natives_directory}",
            "-cp",
            "${classpath}",
            "${mainClass}",
        ] {
            final_args.push(replace_placeholders(jvm_arg, &argdata, &config.main_class));
        }
    }

    for arg in &config.start_args {
        let replaced = replace_placeholders(arg, &argdata, &config.main_class);
        final_args.push(replaced);
    }

    let minecraft_dir = instance_dir.join(".minecraft");
    let mut command = Command::new(&options.java_path);
    command
        .current_dir(&minecraft_dir)
        .env("JAVA_TOOL_OPTIONS", "-Dfile.encoding=UTF-8");

    // @argfile is Java 9+ only; Java 8 (e.g. Forge 1.7.10) treats "@tempcmd.txt" as
    // the main-class name, so pass args on the command line there instead. The
    // argfile also avoids OS command-length limits; it's relative because cwd is
    // already minecraft_dir.
    let argfile = if config.java_version >= 9 {
        let argfile_path = minecraft_dir.join(ARGFILE_NAME);
        write_argfile(&final_args, &argfile_path).await?;
        command.arg(format!("@{}", ARGFILE_NAME));
        Some(argfile_path)
    } else {
        command.args(&final_args);
        None
    };

    Ok(Prepared { command, argfile })
}

fn extract_natives(config: &InstanceConfig, natives_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(natives_dir)?;

    for native in &config.natives {
        if !native.path.exists() {
            continue;
        }
        let file = std::fs::File::open(&native.path)?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| HexoError::Other(e.to_string()))?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| HexoError::Other(e.to_string()))?;
            let name = entry.name().to_string();
            if name.starts_with("META-INF") || name.ends_with('/') {
                continue;
            }
            let out_path = natives_dir.join(&name);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
        }
    }

    Ok(())
}

fn build_classpath(config: &InstanceConfig, instance_dir: &Path) -> String {
    let sep = classpath_sep();
    let mut seen = std::collections::HashSet::new();
    let mut parts: Vec<String> = config
        .lib_list
        .iter()
        .filter_map(|lib| {
            // Make relative paths absolute; once Java's cwd is .minecraft/ a relative
            // path would break.
            let abs = lib.path
                .canonicalize()
                .unwrap_or_else(|_| lib.path.clone());
            let s = path_str(&abs);
            // Dedupe (Forge/NeoForge libs may overlap with vanilla libs).
            if seen.insert(s.clone()) { Some(s) } else { None }
        })
        .collect();

    // client.jar goes last.
    let client_jar_path = instance_dir.join(format!("{}.jar", config.version_id));
    parts.push(path_str(&client_jar_path));

    parts.join(sep)
}

fn build_argdata(
    opts: &LaunchOptions,
    base_dir: &Path,
    instance_dir: &Path,
    config: &InstanceConfig,
    classpath: &str,
) -> HashMap<String, String> {
    let minecraft_dir = instance_dir.join(".minecraft");
    let mut map = HashMap::new();

    let auth_token = opts.auth_token.as_deref().unwrap_or("-1");
    let offline_uuid;
    let auth_uuid = match opts.auth_uuid.as_deref() {
        Some(u) => u,
        None => {
            // Generate offline UUID the same way the vanilla launcher does:
            // UUID v3 (MD5) of "OfflinePlayer:<name>" in the DNS namespace.
            let key = format!("OfflinePlayer:{}", opts.player_name);
            offline_uuid = Uuid::new_v3(&Uuid::NAMESPACE_DNS, key.as_bytes())
                .to_string()
                .replace('-', "");
            &offline_uuid
        }
    };
    let xuid = opts.xuid.as_deref().unwrap_or("-1");
    let user_type = if opts.auth_token.is_some() { "msa" } else { "mojang" };

    map.insert("auth_player_name".into(), opts.player_name.clone());
    map.insert("version_name".into(), config.version_id.clone());
    map.insert("game_directory".into(), path_str(&minecraft_dir));
    map.insert("assets_root".into(), path_str(&base_dir.join("assets")));
    map.insert("assets_index_name".into(), config.assets_id.clone());
    map.insert("auth_uuid".into(), auth_uuid.to_string());
    map.insert("auth_access_token".into(), auth_token.to_string());
    map.insert("clientid".into(), "-1".into());
    map.insert("auth_xuid".into(), xuid.to_string());
    map.insert("user_type".into(), user_type.to_string());
    // Old versions (1.7.10) need valid JSON for --userProperties or parsing fails.
    map.insert("user_properties".into(), "{}".into());
    map.insert("version_type".into(), "release".into());
    map.insert("natives_directory".into(), path_str(&instance_dir.join("natives")));
    map.insert("launcher_name".into(), "HexoLauncher".into());
    map.insert("launcher_version".into(), env!("CARGO_PKG_VERSION").to_string());
    map.insert("classpath".into(), classpath.to_string());
    map.insert("library_directory".into(), path_str(&base_dir.join("libraries")));
    map.insert("classpath_separator".into(), classpath_sep().to_string());

    map
}

/// Replace `${key}` placeholders, including the special `${mainClass}`.
fn replace_placeholders(
    arg: &str,
    data: &HashMap<String, String>,
    main_class: &str,
) -> String {
    if arg == "${mainClass}" {
        return main_class.to_string();
    }

    let mut result = arg.to_string();
    while let Some(start) = result.find("${") {
        let end = match result[start..].find('}') {
            Some(i) => start + i,
            None => break,
        };
        let key = &result[start + 2..end];
        if let Some(value) = data.get(key) {
            result = format!("{}{}{}", &result[..start], value, &result[end + 1..]);
        } else {
            break; // Unknown key: leave as-is to avoid an infinite loop.
        }
    }
    result
}

async fn write_argfile(args: &[String], path: &Path) -> Result<()> {
    // @argfile format: one arg per line, quoted if it contains whitespace.
    let content = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{}\"", a)
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    fs::write(path, content).await?;
    Ok(())
}

fn classpath_sep() -> &'static str {
    if cfg!(target_os = "windows") { ";" } else { ":" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_data() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("auth_player_name".into(), "Steve".into());
        m.insert("version_name".into(), "1.21.4".into());
        m.insert("classpath".into(), "/path/to/libs.jar".into());
        m
    }

    #[test]
    fn replace_simple_placeholder() {
        let data = make_data();
        let result = replace_placeholders("--username ${auth_player_name}", &data, "net.mc.Main");
        assert_eq!(result, "--username Steve");
    }

    #[test]
    fn replace_main_class() {
        let data = make_data();
        let result = replace_placeholders("${mainClass}", &data, "cpw.mods.bootstraplauncher.BootstrapLauncher");
        assert_eq!(result, "cpw.mods.bootstraplauncher.BootstrapLauncher");
    }

    #[test]
    fn replace_multiple_placeholders_in_one_arg() {
        let mut data = make_data();
        data.insert("game_directory".into(), "/home/user/.minecraft".into());
        let result = replace_placeholders("${version_name}:${game_directory}", &data, "Main");
        assert_eq!(result, "1.21.4:/home/user/.minecraft");
    }

    #[test]
    fn unknown_placeholder_preserved() {
        let data = make_data();
        let result = replace_placeholders("${unknown_key}", &data, "Main");
        assert_eq!(result, "${unknown_key}");
    }

    #[test]
    fn no_placeholder_passthrough() {
        let data = make_data();
        let result = replace_placeholders("-Xmx2G", &data, "Main");
        assert_eq!(result, "-Xmx2G");
    }

    #[tokio::test]
    async fn argfile_removed_after_delay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ARGFILE_NAME);
        std::fs::write(&path, "-Xmx2G").unwrap();

        remove_argfile_after(path.clone(), Duration::from_millis(10)).await;

        assert!(!path.exists());
    }

    #[tokio::test]
    async fn argfile_removed_when_spawn_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ARGFILE_NAME);
        std::fs::write(&path, "-Xmx2G").unwrap();

        let failed = Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        let result: Result<Child> = finish_spawn(failed, Some(path.clone()));

        assert!(result.is_err());
        assert!(!path.exists());
    }
}

fn path_str(p: &Path) -> String {
    // Normalize backslashes, then strip the Windows `//?/` extended-path prefix
    // (from canonicalize), which Java can't parse.
    let s = p.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = s.strip_prefix("//?/") {
        stripped.to_string()
    } else {
        s
    }
}
