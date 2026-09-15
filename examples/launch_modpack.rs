//! ```bash
//! cargo run --example launch_modpack -- ./Fabulously.Optimized-6.4.0.mrpack
//! ```
use hexomc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let pack_path = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: cargo run --example launch_modpack -- <pack.mrpack|pack.zip>");
            std::process::exit(1);
        }
    };
    let base_dir = PathBuf::from("./mc_data");
    let instance_name = "modpack";

    println!("=== Detecting modpack format ===");
    let format = detect_modpack_format(&pack_path).await?;
    println!("Format: {:?}", format);

    if format == ModpackFormat::Modrinth {
        let index = read_mrpack_index(&pack_path).await?;
        println!("Pack: {} {}", index.name, index.version_id);
        println!("Files listed: {}", index.files.len());
        for (dep, version) in &index.dependencies {
            println!("  {} = {}", dep, version);
        }
    }

    let curseforge = std::env::var("CURSEFORGE_API_KEY")
        .ok()
        .map(|key| CurseForgeClient::new(&key));
    if format == ModpackFormat::CurseForge && curseforge.is_none() {
        eprintln!("CurseForge packs need CURSEFORGE_API_KEY");
        std::process::exit(1);
    }

    println!("\n=== Installing modpack ===");

    let progress: ProgressFn = Arc::new(|done, total, desc| {
        if total > 0 {
            println!("  [{:>4}/{:<4}] {}", done, total, desc);
        }
    });

    let result = install_modpack(
        &pack_path,
        instance_name,
        &base_dir,
        None,
        curseforge.as_ref(),
        progress,
    )
    .await?;

    println!(
        "\nInstalled {} ({} / {:?} {})",
        result.info.name,
        result.info.mc_version,
        result.info.loader,
        result.info.loader_version.as_deref().unwrap_or("latest"),
    );

    if !result.manual_downloads.is_empty() {
        println!("\n=== Files to download manually ===");
        for file in &result.manual_downloads {
            println!(
                "  {} -> {}{}",
                file.name,
                file.dest.display(),
                file.website
                    .as_deref()
                    .map(|w| format!(" ({})", w))
                    .unwrap_or_default(),
            );
        }
    }
    if !result.skipped.is_empty() {
        println!("\nSkipped {} unsupported entries", result.skipped.len());
    }

    println!("\n=== Launching (offline mode) ===");

    let instance_dir = base_dir.join("instance").join(instance_name);
    let required_java = InstanceConfig::load(&instance_dir).await?.java_version;
    let java = find_java(required_java)
        .unwrap_or_else(|| panic!("Java {} not found, please install it", required_java));
    println!("Java: {} (version {})", java.path.display(), java.version);

    let opts = LaunchOptions::offline(instance_name, java.path.clone(), "HexoPlayer");

    let mut child = launch(&opts, &base_dir).await?;
    println!("Minecraft started, PID: {:?}", child.id());

    let status = child.wait()?;
    println!("Minecraft exited with: {}", status);

    Ok(())
}
