use hexomc_lib::*;
use std::path::PathBuf;
use std::sync::Arc;

/// 示範用 `launch_with_output` 以 callback 即時捕捉遊戲的 stdout / stderr。
/// 若想改用 channel 拉取，換成 `launch_with_channel` 並 `rx.recv().await`。
#[tokio::main]
async fn main() -> Result<()> {
    let mc_version = "1.21.1";

    let instance_name = "1.21.1-vanilla";
    let base_dir = PathBuf::from("./mc_data");

    println!("=== 偵測 Java ===");
    let java = find_java(21).expect("找不到 Java 21，請先安裝");
    println!("Java: {} (版本 {})", java.path.display(), java.version);

    println!(
        "\n=== 安裝 Minecraft {} (instance: {}) ===",
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

    println!("\n安裝完成！");

    println!("\n=== 啟動 Minecraft {} (離線模式) ===", mc_version);

    let opts = LaunchOptions::offline(instance_name, java.path.clone(), "HexoPlayer");

    let on_output: OutputFn = Arc::new(|out: OutputLine| match out.kind {
        OutputKind::Stdout => println!("[MC] {}", out.line),
        OutputKind::Stderr => eprintln!("[MC/err] {}", out.line),
    });

    let mut game = launch_with_output(&opts, &base_dir, on_output).await?;
    println!("MC 已啟動，PID: {:?}", game.id());
    println!("等待遊戲關閉...");

    let status = game.wait().await?;
    println!("MC 結束，退出碼: {}", status);

    Ok(())
}
