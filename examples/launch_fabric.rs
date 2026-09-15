use hexomc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let mc_version = "1.18.2";
    let base_dir = PathBuf::from("./mc_data");
    let instance_name = "1.18.2-fabric";

    println!("=== Detecting Java ===");
    let java = find_java(21).expect("Java 21 not found, please install it first");
    println!("Java: {} (version {})", java.path.display(), java.version);

    println!("\n=== Querying Fabric loader versions ===");
    let fabric_versions = get_fabric_loader_versions(mc_version).await?;
    let latest_stable = fabric_versions.iter().find(|v| v.stable);
    if let Some(ver) = &latest_stable {
        println!("Latest stable Fabric loader: {}", ver.version);
    }
    println!("{} versions available", fabric_versions.len());

    println!("\n=== Installing Minecraft {} + Fabric ===", mc_version);

    let progress: ProgressFn = Arc::new(|done, total, desc| {
        if total > 0 {
            println!("  [{:>4}/{:<4}] {}", done, total, desc);
        }
    });

    install_with_loader(
        mc_version,
        instance_name,
        &base_dir,
        &FabricInstaller::latest(),
        progress,
    )
    .await?;

    println!("\nInstallation complete!");

    println!(
        "\n=== Launching Minecraft {} + Fabric (offline mode) ===",
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
