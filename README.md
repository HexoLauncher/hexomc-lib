# hexomc-lib

Minecraft 啟動器核心



## 安裝

```toml
[dependencies]
hexomc-lib = "0.1"
```

## 快速開始


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
cargo run --example launch_vanilla
```
