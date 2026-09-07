use hexo_mc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let mc_version = "1.7.10";
    let base_dir = PathBuf::from("./mc_data");

    let instance_name = "1.7.10-forge";

    println!("=== 偵測 Java ===");
    let java = find_java(8).expect("找不到 Java，請先安裝 Java 8");
    println!("Java: {} (版本 {})", java.path.display(), java.version);
    if java.version != 8 {
        panic!(
            "Forge 1.7.10 需要 Java 8，但偵測到 Java {}。請安裝 Java 8 後再試。",
            java.version
        );
    }

    println!("\n=== 查詢 Forge 版本 ===");
    let forge_versions = get_forge_versions(mc_version).await?;
    if let Some(recommended) = forge_versions.first() {
        println!("Forge 推薦版本: {}", recommended);
    }
    println!("共 {} 個版本可用", forge_versions.len());

    println!("\n=== 安裝 Minecraft {} + Forge ===", mc_version);

    let progress: ProgressFn = Arc::new(|done, total, desc| {
        if total > 0 {
            println!("  [{:>4}/{:<4}] {}", done, total, desc);
        }
    });

    let loader = ForgeInstaller::new(java.path.clone());

    install_with_loader(mc_version, instance_name, &base_dir, &loader, progress).await?;

    println!("\n安裝完成！");

    println!("\n=== 啟動 Minecraft {} + Forge (離線模式) ===", mc_version);

    let opts = LaunchOptions::offline(instance_name, java.path.clone(), "HexoPlayer");

    let mut child = launch(&opts, &base_dir).await?;
    println!("MC 已啟動，PID: {:?}", child.id());
    println!("等待遊戲關閉...");

    let status = child.wait()?;
    println!("MC 結束，退出碼: {}", status);

    Ok(())
}
