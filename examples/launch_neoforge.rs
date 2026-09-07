use hexomc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let mc_version = "1.21.1";
    let base_dir = PathBuf::from("./mc_data");

    let instance_name = "1.21.1-neoforge";

    println!("=== 偵測 Java ===");
    let java = find_java(21).expect("找不到 Java 21，請先安裝");
    println!("Java: {} (版本 {})", java.path.display(), java.version);

    println!("\n=== 查詢 NeoForge 版本 ===");
    let neoforge_versions = get_neoforge_versions(mc_version).await?;
    if let Some(latest) = neoforge_versions.last() {
        println!("最新 NeoForge 版本: {}", latest);
    }
    println!("共 {} 個版本可用", neoforge_versions.len());

    println!("\n=== 安裝 Minecraft {} + NeoForge ===", mc_version);

    let progress: ProgressFn = Arc::new(|done, total, desc| {
        if total > 0 {
            println!("  [{:>4}/{:<4}] {}", done, total, desc);
        }
    });

    let loader = NeoForgeInstaller::new(java.path.clone());

    install_with_loader(mc_version, instance_name, &base_dir, &loader, progress).await?;

    println!("\n安裝完成！");

    println!(
        "\n=== 啟動 Minecraft {} + NeoForge (離線模式) ===",
        mc_version
    );

    let opts = LaunchOptions::offline(instance_name, java.path.clone(), "HexoPlayer");

    let mut child = launch(&opts, &base_dir).await?;
    println!("MC 已啟動，PID: {:?}", child.id());
    println!("等待遊戲關閉...");

    let status = child.wait()?;
    println!("MC 結束，退出碼: {}", status);

    Ok(())
}
