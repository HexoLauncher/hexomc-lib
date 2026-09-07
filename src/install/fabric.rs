use crate::{
    download::{download_batch, DownloadTask},
    error::Result,
    install::vanilla::InstanceConfig,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

const FABRIC_META_BASE: &str = "https://meta.fabricmc.net/v2";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FabricLoaderVersion {
    pub version: String,
    pub stable: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FabricProfileJson {
    pub main_class: String,
    pub arguments: FabricArguments,
    pub libraries: Vec<FabricLibrary>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FabricArguments {
    pub jvm: Option<Vec<String>>,
    pub game: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FabricLibrary {
    pub name: String,
    pub url: String,
}

pub async fn get_fabric_loader_versions(mc_version: &str) -> Result<Vec<FabricLoaderVersion>> {
    let url = format!("{}/versions/loader/{}", FABRIC_META_BASE, mc_version);
    let client = reqwest::Client::new();

    #[derive(Deserialize)]
    struct Entry {
        loader: FabricLoaderVersion,
    }

    let entries: Vec<Entry> = client.get(&url).send().await?.json().await?;
    Ok(entries.into_iter().map(|e| e.loader).collect())
}

async fn fetch_fabric_profile(mc_version: &str, loader_version: &str) -> Result<FabricProfileJson> {
    let url = format!(
        "{}/versions/loader/{}/{}/profile/json",
        FABRIC_META_BASE, mc_version, loader_version
    );
    let client = reqwest::Client::new();
    let profile = client
        .get(&url)
        .send()
        .await?
        .json::<FabricProfileJson>()
        .await?;
    Ok(profile)
}

pub async fn install_fabric(
    mc_version: &str,
    loader_version: &str,
    instance_name: &str,
    base_dir: &Path,
) -> Result<()> {
    let instance_dir = base_dir.join("instance").join(instance_name);
    let lib_dir = base_dir.join("libraries");

    let mut config = InstanceConfig::load(&instance_dir).await?;
    let profile = fetch_fabric_profile(mc_version, loader_version).await?;

    let mut tasks: Vec<DownloadTask> = Vec::new();
    let mut new_libs = Vec::new();

    for fab_lib in &profile.libraries {
        let parsed = parse_fabric_lib(fab_lib)?;

        let artifact_path = lib_dir.join(&parsed.local_path);
        let sha1 = fetch_text(&parsed.sha1_url).await.unwrap_or_default();

        tasks.push(
            DownloadTask::new(&parsed.jar_url, &artifact_path).with_sha1(sha1.trim().to_string()),
        );

        new_libs.push(crate::install::vanilla::LibEntry {
            name: fab_lib.name.clone(),
            path: artifact_path,
            sha1: sha1.trim().to_string(),
        });
    }

    download_batch(tasks, 8, |_, _| {}).await?;

    // Prepend Fabric's JVM args.
    if let Some(jvm_args) = &profile.arguments.jvm {
        let trimmed: Vec<String> = jvm_args.iter().map(|s| s.trim().to_string()).collect();
        let mut new_args = trimmed;
        new_args.extend(config.start_args.drain(..));
        config.start_args = new_args;
    }

    if let Some(game_args) = &profile.arguments.game {
        config.start_args.extend(game_args.iter().cloned());
    }

    config.main_class = profile.main_class.clone();
    config.loader_type = crate::install::vanilla::LoaderType::Fabric;

    // Prepend the new libs so Fabric takes precedence.
    new_libs.extend(config.lib_list.drain(..));
    config.lib_list = new_libs;

    config.save(&instance_dir).await?;
    Ok(())
}

struct FabricLibParsed {
    jar_url: String,
    sha1_url: String,
    local_path: std::path::PathBuf,
}

fn parse_fabric_lib(lib: &FabricLibrary) -> Result<FabricLibParsed> {
    let parts: Vec<&str> = lib.name.split(':').collect();
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];

    let base = format!("{}{}/{}/{}", lib.url, group, artifact, version);
    let jar_name = format!("{}-{}.jar", artifact, version);
    let jar_url = format!("{}/{}", base, jar_name);
    let sha1_url = format!("{}/{}.sha1", base, jar_name);

    let local_path = std::path::PathBuf::from(&group)
        .join(artifact)
        .join(version)
        .join(&jar_name);

    Ok(FabricLibParsed {
        jar_url,
        sha1_url,
        local_path,
    })
}

async fn fetch_text(url: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let text = client.get(url).send().await?.text().await?;
    Ok(text)
}
