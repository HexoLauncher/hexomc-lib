use hexomc_lib::{
    fetch_ftb_version_manifest, get_ftb_pack,
    fetch_atlauncher_pack_config, fetch_version_manifest, get_atlauncher_pack_versions,
    install::fabric::get_fabric_loader_versions, install::forge::get_forge_versions,
    install::neoforge::get_neoforge_versions, LoaderType,
};

#[tokio::test]
async fn ftb_pack_and_version_manifest() {
    let pack = get_ftb_pack(134).await.unwrap();
    assert_eq!(pack.id, 134);
    assert!(!pack.name.is_empty());
    let manifest = fetch_ftb_version_manifest(134, 100422).await.unwrap();
    let info = manifest.info(&pack.name).unwrap();
    assert_eq!(info.mc_version, "1.21.1");
    assert_eq!(info.loader, LoaderType::NeoForge);
    assert!(!manifest.files.is_empty());
}

#[tokio::test]
async fn ftb_legacy_curseforge_references() {
    let manifest = fetch_ftb_version_manifest(35, 37).await.unwrap();
    let info = manifest.info("FTB Revelation").unwrap();
    assert_eq!(info.loader, LoaderType::Forge);
    assert_eq!(info.loader_version.as_deref(), Some("14.23.5.2846"));
    assert!(manifest.files.iter().any(|f| f.curseforge.is_some()));
}

#[tokio::test]
async fn ftb_missing_pack_and_version() {
    assert!(matches!(get_ftb_pack(999999999).await, Err(hexomc_lib::HexoError::VersionNotFound(_))));
    assert!(matches!(fetch_ftb_version_manifest(134, 999999999).await,
        Err(hexomc_lib::HexoError::VersionNotFound(_))));
}

#[tokio::test]
async fn atlauncher_pack_versions() {
    let versions = get_atlauncher_pack_versions("SkyFactory4").await.unwrap();
    assert!(versions.iter().any(|v| v.version == "4.2.4" && v.minecraft == "1.12.2"));
}

#[tokio::test]
async fn atlauncher_pack_config() {
    let config = fetch_atlauncher_pack_config("SkyFactory4", "4.2.4").await.unwrap();
    let info = config.info("SkyFactory4").unwrap();
    assert_eq!(info.mc_version, "1.12.2");
    assert_eq!(info.loader, LoaderType::Forge);
    assert_eq!(info.loader_version.as_deref(), Some("14.23.5.2860"));
    assert!(config.mods.len() > 100);
}

#[tokio::test]
async fn manifest_has_latest_release() {
    let manifest = fetch_version_manifest().await.unwrap();
    assert!(!manifest.latest.release.is_empty());
    println!("latest release: {}", manifest.latest.release);
    println!("latest snapshot: {}", manifest.latest.snapshot);
    println!("{} versions", manifest.versions.len());
}

#[tokio::test]
async fn manifest_contains_1_21_4() {
    let manifest = fetch_version_manifest().await.unwrap();
    let found = manifest.versions.iter().any(|v| v.id == "1.21.4");
    assert!(found, "1.21.4 not found");
}

#[tokio::test]
async fn fabric_loaders_for_1_21_4() {
    let loaders = get_fabric_loader_versions("1.21.4").await.unwrap();
    assert!(!loaders.is_empty(), "no Fabric loader versions");
    println!("latest Fabric loader: {}", loaders[0].version);
}

#[tokio::test]
async fn forge_versions_for_1_20_1() {
    let versions = get_forge_versions("1.20.1").await.unwrap();
    assert!(!versions.is_empty(), "no Forge versions");
    println!("recommended Forge version: {}", versions[0]);
}

#[tokio::test]
async fn neoforge_versions_for_1_21_1() {
    let versions = get_neoforge_versions("1.21.1").await.unwrap();
    assert!(!versions.is_empty(), "no NeoForge versions");
    println!("latest NeoForge: {}", versions.last().unwrap());
}
