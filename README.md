# hexo-mc-lib

Minecraft 啟動器核心的 Rust 函式庫（library，非執行檔）。提供組裝一個啟動器所需的底層能力：

- **版本** — 查詢 Mojang version manifest 與各版本 JSON
- **安裝** — Vanilla / Fabric / Forge / NeoForge 安裝
- **Java** — 偵測本機 Java，或下載 Adoptium Temurin JRE
- **模組** — 模組偵測，以及 Modrinth / CurseForge 查詢與自動更新
- **資源** — 資源包 / 光影 / 地圖偵測

## 安裝

```toml
[dependencies]
hexo-mc-lib = "0.1"
```

## 快速開始

安裝並以離線模式啟動 Minecraft：

```rust
use hexo_mc_lib::*;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let base_dir = PathBuf::from("./mc_data");
    let instance_name = "1.21.1-vanilla";

    let java = find_java(21).expect("找不到 Java 21");

    install_with_loader(
        "1.21.1",
        instance_name,
        &base_dir,
        &VanillaInstaller,
        no_progress(),
    )
    .await?;

    let opts = LaunchOptions::offline(instance_name, java.path.clone(), "HexoPlayer");
    let mut child = launch(&opts, &base_dir).await?;
    child.wait()?;

    Ok(())
}
```

完整範例見 [`examples/`](examples/)：

```bash
cargo run --example launch_vanilla   # 也有 launch_fabric、launch_forge、launch_neoforge
```

## 磁碟結構

啟動器根目錄（`base_dir`）內含共用的 `assets/`、`libraries/`，以及各實例的 `instance/{instance_name}/`。同一個 MC 版本可用不同 loader 建立不同實例（例如 `1.21.1-vanilla` 與 `1.21.1-fabric`）而不衝突。

## 測試

```bash
cargo test
```
