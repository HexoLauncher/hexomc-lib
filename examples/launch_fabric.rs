use hexomc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let mc_version = "1.18.2";
    let base_dir = PathBuf::from("./mc_data");
    let instance_name = "1.18.2-fabric";

    println!("=== 偵測 Java ===");
    let java = find_java(21).expect("找不到 Java 21，請先安裝");
    println!("Java: {} (版本 {})", java.path.display(), java.version);

    println!("\n=== 查詢 Fabric loader 版本 ===");
    let fabric_versions = get_fabric_loader_versions(mc_version).await?;
    let latest_stable = fabric_versions.iter().find(|v| v.stable);
    if let Some(ver) = &latest_stable {
        println!("最新穩定版 Fabric loader: {}", ver.version);
    }
    println!("共 {} 個版本可用", fabric_versions.len());

    println!("\n=== 安裝 Minecraft {} + Fabric ===", mc_version);

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

    println!("\n安裝完成！");

    println!(
        "\n=== 啟動 Minecraft {} + Fabric (離線模式) ===",
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
