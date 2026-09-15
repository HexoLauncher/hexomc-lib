use hexomc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

/// Capture the game's stdout / stderr live through a callback with `launch_with_output`.
/// To pull the lines from a channel instead, use `launch_with_channel` and `rx.recv().await`.
#[tokio::main]
async fn main() -> Result<()> {
    let mc_version = "1.21.1";

    let instance_name = "1.21.1-vanilla";
    let base_dir = PathBuf::from("./mc_data");

    println!("=== Detecting Java ===");
    let java = find_java(21).expect("Java 21 not found, please install it first");
    println!("Java: {} (version {})", java.path.display(), java.version);

    println!(
        "\n=== Installing Minecraft {} (instance: {}) ===",
        mc_version, instance_name
    );

    let progress: ProgressFn = Arc::new(|done, total, desc| {
        if total > 0 {
            println!("  [{:>4}/{:<4}] {}", done, total, desc);
        }
    });

    install_with_loader(
        mc_version,
        instance_name,
        &base_dir,
        &VanillaInstaller,
        progress,
    )
    .await?;

    println!("\nInstallation complete!");

    println!("\n=== Launching Minecraft {} (offline mode) ===", mc_version);

    let opts = LaunchOptions::offline(instance_name, java.path.clone(), "HexoPlayer");

    let on_output: OutputFn = Arc::new(|out: OutputLine| match out.kind {
        OutputKind::Stdout => println!("[MC] {}", out.line),
        OutputKind::Stderr => eprintln!("[MC/err] {}", out.line),
    });

    let mut game = launch_with_output(&opts, &base_dir, on_output).await?;
    println!("Minecraft started, PID: {:?}", game.id());
    println!("Waiting for the game to exit...");

    let status = game.wait().await?;
    println!("Minecraft exited with: {}", status);

    Ok(())
}
