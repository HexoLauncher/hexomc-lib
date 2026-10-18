use std::{
    error::Error,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use hexomc_lib::{
    download::verify_sha1, download_java, install::vanilla::parse_start_args, install_with_loader,
    launch_with_output, no_progress, FabricInstaller, ForgeInstaller, GameProcess, InstanceConfig,
    LaunchOptions, LoaderInstaller, LoaderType, OutputFn, VanillaInstaller, VersionJson,
};
use serde_json::json;
use tokio::time::{sleep, timeout, Instant};

type TestResult = std::result::Result<(), Box<dyn Error + Send + Sync>>;

struct RunningGame(GameProcess);

impl Drop for RunningGame {
    fn drop(&mut self) {
        let _ = self.0.child_mut().start_kill();
    }
}

fn version_json() -> serde_json::Value {
    json!({
        "id": "test-version",
        "mainClass": "net.minecraft.client.main.Main",
        "assetIndex": {"id": "test-assets", "sha1": "", "url": "", "size": 0},
        "assets": "test-assets",
        "javaVersion": {"majorVersion": 8},
        "libraries": [],
        "downloads": {"client": {"sha1": "", "size": 0, "url": ""}}
    })
}

#[test]
fn legacy_metadata_without_java_version_defaults_to_java_8() {
    let mut value = version_json();
    value.as_object_mut().unwrap().remove("javaVersion");
    let version: VersionJson = serde_json::from_value(value).unwrap();
    assert_eq!(version.java_version.major_version, 8);
}

#[test]
fn explicit_java_version_is_preserved() {
    let mut value = version_json();
    value["javaVersion"] = json!({"majorVersion": 25});
    let version: VersionJson = serde_json::from_value(value).unwrap();
    assert_eq!(version.java_version.major_version, 25);
}

#[derive(Default)]
struct StartupLog {
    pending_report: bool,
    crashed: bool,
}

impl StartupLog {
    fn observe(&mut self, line: &str) {
        if line.contains("---- Minecraft Crash Report ----") {
            self.pending_report = true;
        }
        if self.pending_report {
            if let Some(description) = line.trim().strip_prefix("Description:") {
                self.pending_report = false;
                if description.trim() != "Loading screen debug info" {
                    self.crashed = true;
                }
            }
        }
        if [
            "Minecraft has crashed!",
            "#@!@# Game crashed!",
            "Exception in thread \"main\"",
            "Reported exception thrown!",
        ]
        .iter()
        .any(|marker| line.contains(marker))
        {
            self.crashed = true;
        }
    }
}

#[test]
fn forge_splash_diagnostics_are_not_a_startup_crash() {
    let mut log = StartupLog::default();
    for line in [
        "[STDOUT]: [cpw.mods.fml.client.SplashProgress:start:188]: ---- Minecraft Crash Report ----",
        "// I feel sad now :(",
        "Time: 10/18/26 7:18 PM",
        "Description: Loading screen debug info",
        "This is just a prompt for computer specs to be printed. THIS IS NOT A ERROR",
        "[FML]: Completed early MinecraftForge initialization",
    ] {
        log.observe(line);
    }
    assert!(!log.crashed);
    log.observe("---- Minecraft Crash Report ----");
    log.observe("Description: Initializing game");
    assert!(log.crashed);
}

#[test]
fn fatal_startup_markers_are_detected_without_a_crash_report() {
    for marker in [
        "Minecraft has crashed!",
        "#@!@# Game crashed! Crash report saved to: report.txt",
        "Exception in thread \"main\" java.lang.NoClassDefFoundError",
        "Reported exception thrown!",
    ] {
        let mut log = StartupLog::default();
        log.observe(marker);
        assert!(log.crashed, "Missed fatal marker: {marker}");
    }
}

#[test]
fn legacy_launch_arguments_preserve_order_and_placeholders() {
    let mut value = version_json();
    value["minecraftArguments"] = json!(
        "--username ${auth_player_name} --session ${auth_session} --gameDir ${game_directory}"
    );
    let version: VersionJson = serde_json::from_value(value).unwrap();
    assert_eq!(
        parse_start_args(&version),
        [
            "--username",
            "${auth_player_name}",
            "--session",
            "${auth_session}",
            "--gameDir",
            "${game_directory}"
        ]
    );
}

#[test]
fn modern_launch_arguments_separate_jvm_main_class_and_game() {
    let mut value = version_json();
    value["arguments"] = json!({
        "jvm": ["-cp", "${classpath}", {"rules": [{"action": "allow"}], "value": ["-Dtest=true"]}],
        "game": ["--username", "${auth_player_name}", {
            "rules": [{"action": "allow", "features": {"is_demo_user": true}}],
            "value": "--demo"
        }]
    });
    let version: VersionJson = serde_json::from_value(value).unwrap();
    assert_eq!(
        parse_start_args(&version),
        [
            "-cp",
            "${classpath}",
            "-Dtest=true",
            "${mainClass}",
            "--username",
            "${auth_player_name}"
        ]
    );
}

#[tokio::test]
async fn instance_configs_round_trip_without_overwriting_other_loaders() {
    let root = tempfile::tempdir().unwrap();
    let mut snapshots = Vec::new();
    for (name, loader_type) in [
        ("vanilla", LoaderType::Vanilla),
        ("fabric", LoaderType::Fabric),
        ("forge", LoaderType::Forge),
    ] {
        let directory = root.path().join(format!("1.12.2-{name}"));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let config = InstanceConfig {
            version_id: "1.12.2".into(),
            loader_type,
            main_class: format!("test.{name}.Main"),
            start_args: vec!["${auth_player_name}".into()],
            lib_list: vec![],
            natives: vec![],
            assets_id: "1.12".into(),
            java_version: 8,
        };
        config.save(&directory).await.unwrap();
        snapshots.push((directory, serde_json::to_value(config).unwrap()));
    }
    for (directory, expected) in snapshots {
        let restored = InstanceConfig::load(&directory).await.unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), expected);
    }
}

async fn verify_installation(
    root: &Path,
    instance: &str,
    version: &str,
    loader: LoaderType,
) -> std::result::Result<InstanceConfig, Box<dyn Error + Send + Sync>> {
    let directory = root.join("instance").join(instance);
    let config = InstanceConfig::load(&directory).await?;
    assert_eq!(config.version_id, version);
    assert_eq!(config.loader_type, loader);
    assert!(!config.main_class.is_empty());
    assert!(!config.start_args.is_empty());
    assert!(config.java_version >= 8);
    assert!(directory.join(".minecraft/mods").is_dir());
    assert!(directory.join(".minecraft/config").is_dir());
    assert!(directory.join(format!("{version}.jar")).metadata()?.len() > 0);
    assert!(root
        .join("assets/indexes")
        .join(format!("{}.json", config.assets_id))
        .is_file());
    for (path, checksum) in config
        .lib_list
        .iter()
        .map(|lib| (&lib.path, &lib.sha1))
        .chain(
            config
                .natives
                .iter()
                .map(|native| (&native.path, &native.sha1)),
        )
    {
        assert!(
            path.metadata()?.len() > 0,
            "Empty library: {}",
            path.display()
        );
        if !checksum.is_empty() {
            assert!(
                verify_sha1(path, checksum).await,
                "Invalid library: {}",
                path.display()
            );
        }
    }
    Ok(config)
}

/// SDL3 needs PID matching; LWJGL2 windows may only expose the legacy title.
async fn visible_minecraft_window(pid: u32) -> bool {
    tokio::process::Command::new("xdotool")
        .args(["search", "--onlyvisible", "--any", "--pid", &pid.to_string(), "--name", "^Minecraft"])
        .output()
        .await
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

async fn save_window_diagnostics(path: &Path, pid: u32) {
    let pid = pid.to_string();
    let mut log = String::new();
    for (command, args) in [
        ("xwininfo", vec!["-root", "-tree"]),
        ("xdotool", vec!["search", "--onlyvisible", "--pid", &pid]),
    ] {
        log.push_str(&format!("{command} {}\n", args.join(" ")));
        let mut child = tokio::process::Command::new(command);
        child.args(args).kill_on_drop(true);
        match timeout(Duration::from_secs(5), child.output()).await {
            Ok(Ok(output)) => {
                log.push_str(&format!("status: {}\n", output.status));
                log.push_str(&String::from_utf8_lossy(&output.stdout));
                log.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            Ok(Err(error)) => log.push_str(&format!("{error}\n")),
            Err(_) => log.push_str("Diagnostic command timed out\n"),
        }
    }
    eprintln!("{log}");
    let _ = tokio::fs::write(path, log).await;
}

async fn verify_startup(root: &Path, instance: &str, stage: &str, java: PathBuf) -> TestResult {
    let diagnostics = root.join("diagnostics");
    tokio::fs::create_dir_all(&diagnostics).await?;
    let output = Arc::new(Mutex::new(std::fs::File::create(
        diagnostics.join(format!("{instance}-{stage}.log")),
    )?));
    let startup_log = Arc::new(Mutex::new(StartupLog::default()));
    let crash_output = startup_log.clone();
    let sink: OutputFn = Arc::new(move |line| {
        crash_output.lock().unwrap().observe(&line.line);
        println!("{:?}: {}", line.kind, line.line);
        writeln!(output.lock().unwrap(), "{:?}: {}", line.kind, line.line).unwrap();
    });
    let mut options = LaunchOptions::offline(instance, java, "HexoCITest");
    options
        .jvm_extra_args
        .push("-Dorg.lwjgl.opengl.Display.allowSoftwareOpenGL=true".into());
    let mut process = RunningGame(launch_with_output(&options, root, sink).await?);
    let pid = process.0.id().ok_or("Minecraft process has no PID")?;
    let result = timeout(Duration::from_secs(180), async {
        let mut visible_since = None;
        loop {
            if startup_log.lock().unwrap().crashed {
                return Err("Minecraft reported a startup crash".into());
            }
            if let Some(status) = process.0.try_wait()? {
                return Err(format!("Minecraft exited before startup completed: {status}").into());
            }
            if visible_minecraft_window(pid).await {
                let since = visible_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(30) {
                    return Ok::<(), Box<dyn Error + Send + Sync>>(());
                }
            } else {
                visible_since = None;
            }
            sleep(Duration::from_secs(1)).await;
        }
    })
    .await;
    if !matches!(&result, Ok(Ok(()))) {
        save_window_diagnostics(
            &diagnostics.join(format!("{instance}-{stage}-windows.log")), pid,
        ).await;
    }
    let kill_result = process.0.kill().await;
    let wait_result = process.0.wait().await;
    result.map_err(|_| "Minecraft did not display a stable window within 180 seconds")??;
    kill_result?;
    wait_result?;
    assert!(
        !startup_log.lock().unwrap().crashed,
        "Minecraft reported a startup crash"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires network, downloaded Java, Xvfb and xdotool"]
async fn install_and_launch() -> TestResult {
    let version = std::env::var("MC_TEST_VERSION")?;
    let loader_name = std::env::var("MC_TEST_LOADER")?;
    let loader_version = std::env::var("MC_TEST_LOADER_VERSION")?;
    assert!(["vanilla", "fabric", "forge"].contains(&loader_name.as_str()));
    let root = PathBuf::from(std::env::var("MC_TEST_DIR")?);
    tokio::fs::create_dir_all(&root).await?;
    let root = root.canonicalize()?;
    let instance = format!("{version}-{loader_name}");
    install_with_loader(&version, &instance, &root, &VanillaInstaller, no_progress()).await?;
    let vanilla = verify_installation(&root, &instance, &version, LoaderType::Vanilla).await?;
    let java = download_java(vanilla.java_version, &root).await?;
    verify_startup(&root, &instance, "vanilla", java.clone()).await?;
    let loader: Box<dyn LoaderInstaller> = match loader_name.as_str() {
        "vanilla" => return Ok(()),
        "fabric" => Box::new(FabricInstaller::new(loader_version)),
        "forge" => Box::new(ForgeInstaller::with_version(java.clone(), loader_version)),
        _ => unreachable!(),
    };
    install_with_loader(&version, &instance, &root, loader.as_ref(), no_progress()).await?;
    let installed = verify_installation(&root, &instance, &version, loader.loader_type()).await?;
    assert_ne!(installed.main_class, vanilla.main_class);
    assert!(installed.lib_list.len() > vanilla.lib_list.len());
    assert_eq!(installed.assets_id, vanilla.assets_id);
    verify_startup(&root, &instance, &loader_name, java).await
}
