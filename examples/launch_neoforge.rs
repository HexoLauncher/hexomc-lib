use hexomc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let mc_version = "1.21.1";
    let base_dir = PathBuf::from("./mc_data");

    let instance_name = "1.21.1-neoforge";

    println!("=== Detecting Java ===");
    let java = find_java(21).expect("Java 21 not found, please install it first");
    println!("Java: {} (version {})", java.path.display(), java.version);

    println!("\n=== Querying NeoForge versions ===");
    let neoforge_versions = get_neoforge_versions(mc_version).await?;
    if let Some(latest) = neoforge_versions.last() {
        println!("Latest NeoForge version: {}", latest);
    }
    println!("{} versions available", neoforge_versions.len());

    println!("\n=== Installing Minecraft {} + NeoForge ===", mc_version);

    let progress: ProgressFn = Arc::new(|done, total, desc| {
        if total > 0 {
            println!("  [{:>4}/{:<4}] {}", done, total, desc);
        }
    });

    let loader = NeoForgeInstaller::new(java.path.clone());

    install_with_loader(mc_version, instance_name, &base_dir, &loader, progress).await?;

    println!("\nInstallation complete!");

    println!(
        "\n=== Launching Minecraft {} + NeoForge (offline mode) ===",
        mc_version
    );

    let opts = LaunchOptions::offline(instance_name, java.path.clone(), "HexoPlayer");

    let mut child = launch(&opts, &base_dir).await?;
    println!("Minecraft started, PID: {:?}", child.id());
    println!("Waiting for the game to exit...");

    let status = child.wait()?;
    println!("Minecraft exited with: {}", status);

    Ok(())
}
