use hexomc_lib::{
    fetch_version_manifest, install::fabric::get_fabric_loader_versions,
    install::forge::get_forge_versions, install::neoforge::get_neoforge_versions,
};

#[tokio::test]
async fn manifest_has_latest_release() {
    let manifest = fetch_version_manifest().await.unwrap();
    assert!(!manifest.latest.release.is_empty());
    println!("最新 release: {}", manifest.latest.release);
    println!("最新 snapshot: {}", manifest.latest.snapshot);
    println!("共 {} 個版本", manifest.versions.len());
}

#[tokio::test]
async fn manifest_contains_1_21_4() {
    let manifest = fetch_version_manifest().await.unwrap();
    let found = manifest.versions.iter().any(|v| v.id == "1.21.4");
    assert!(found, "找不到 1.21.4");
}

#[tokio::test]
async fn fabric_loaders_for_1_21_4() {
    let loaders = get_fabric_loader_versions("1.21.4").await.unwrap();
    assert!(!loaders.is_empty(), "沒有 Fabric loader 版本");
    println!("最新 Fabric loader: {}", loaders[0].version);
}

#[tokio::test]
async fn forge_versions_for_1_20_1() {
    let versions = get_forge_versions("1.20.1").await.unwrap();
    assert!(!versions.is_empty(), "沒有 Forge 版本");
    println!("Forge 推薦版本: {}", versions[0]);
}

#[tokio::test]
async fn neoforge_versions_for_1_21_1() {
    let versions = get_neoforge_versions("1.21.1").await.unwrap();
    assert!(!versions.is_empty(), "沒有 NeoForge 版本");
    println!("最新 NeoForge: {}", versions.last().unwrap());
}
