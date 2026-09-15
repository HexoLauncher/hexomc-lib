# hexomc-lib

Minecraft 啟動器核心

- **版本** — 查詢 Mojang version manifest 與各版本 JSON
- **安裝** — Vanilla / Fabric / Forge / NeoForge 安裝
- **Java** — 偵測本機 Java，或下載 Adoptium Temurin JRE
- **模組** — 模組偵測，以及 Modrinth / CurseForge 查詢與自動更新
- **模組包** — Modrinth `.mrpack` / CurseForge zip / ATLauncher / FTB 線上模組包安裝；FTB 使用 pack ID + version ID，支援選裝檔案，無法自動下載的 CurseForge 檔案會回傳手動下載清單
- **資源** — 資源包 / 光影 / 地圖偵測

## 安裝

```toml
[dependencies]
hexomc-lib = "0.1"
```

## 快速開始

### Java 快取與下載進度

`download_java(version, base_dir)` 會先重用 `java/{version}/jre` 裡已解壓的 Java，避免重複查詢、下載和解壓。
需要進度時，改用 `download_java_with_progress(version, base_dir, progress)`，其中 `progress` 是 `ProgressFn`。
在 `Downloading Java` 階段，兩個數值是「已下載 bytes、總 bytes」，可換算成 MB 或百分比；總大小為 `0` 表示未知，重試時已下載量會歸零。
其他階段以 `(0, 0, 階段文字)` 通知。下載進度到 100% 後仍需等待校驗、解壓及函式成功回傳。

安裝並以離線模式啟動 Minecraft：

```rust
use hexomc_lib::*;
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
