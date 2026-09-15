use hexomc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let mc_version = "1.7.10";
    let base_dir = PathBuf::from("./mc_data");

    let instance_name = "1.7.10-forge";

    println!("=== Detecting Java ===");
    let java = find_java(8).expect("Java not found, please install Java 8 first");
    println!("Java: {} (version {})", java.path.display(), java.version);
    if java.version != 8 {
        panic!(
            "Forge 1.7.10 needs Java 8, but Java {} was detected. Install Java 8 and try again.",
            java.version
        );
    }

    println!("\n=== Querying Forge versions ===");
    let forge_versions = get_forge_versions(mc_version).await?;
    if let Some(recommended) = forge_versions.first() {
        println!("Recommended Forge version: {}", recommended);
    }
    println!("{} versions available", forge_versions.len());

    println!("\n=== Installing Minecraft {} + Forge ===", mc_version);

    let progress: ProgressFn = Arc::new(|done, total, desc| {
        if total > 0 {
            println!("  [{:>4}/{:<4}] {}", done, total, desc);
        }
    });

    let loader = ForgeInstaller::new(java.path.clone());

    install_with_loader(mc_version, instance_name, &base_dir, &loader, progress).await?;

    println!("\nInstallation complete!");

    println!("\n=== Launching Minecraft {} + Forge (offline mode) ===", mc_version);

    let opts = LaunchOptions::offline(instance_name, java.path.clone(), "HexoPlayer");

    let mut child = launch(&opts, &base_dir).await?;
    println!("Minecraft started, PID: {:?}", child.id());
    println!("Waiting for the game to exit...");

    let status = child.wait()?;
    println!("Minecraft exited with: {}", status);

    Ok(())
}
