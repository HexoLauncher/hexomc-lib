use std::{
    collections::HashSet,
    fs::File,
    path::{Path, PathBuf},
};

use tokio::fs;
use zip::{ZipArchive, ZipWriter};

use crate::{
    download::{download_batch, download_file, mirror::get_text, DownloadTask},
    error::{HexoError, Result},
    install::{InstanceConfig, LibEntry, LoaderType},
};

pub(crate) const CLIENT_ENTRY: &str = "net.minecraftforge:legacy-client";
const FORGE_REPOSITORY: &str = "https://maven.minecraftforge.net/net/minecraftforge/forge";
const FML_REPOSITORY: &str = "https://files.prismlauncher.org/fmllibs";
const MODLOADER_ARCHIVE: &str = "https://b2.mcarchive.net/file/mcarchive";

pub(super) fn classifier(minecraft: &str) -> Option<&'static str> {
    match minecraft {
        "1.1" | "1.2.3" | "1.2.4" | "1.2.5" => Some("client"),
        "1.3.2" | "1.4.0" | "1.4.1" | "1.4.2" | "1.4.3" | "1.4.4" | "1.4.5" | "1.4.6" | "1.4.7"
        | "1.5" | "1.5.1" | "1.5.2" => Some("universal"),
        _ => None,
    }
}

pub(super) async fn install(
    minecraft: &str,
    forge: &str,
    classifier: &str,
    instance_name: &str,
    base_dir: &Path,
) -> Result<()> {
    let version_prefix = format!("{minecraft}-");
    if !forge.strip_prefix(&version_prefix).is_some_and(|version| {
        !version.is_empty()
            && version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
    }) {
        return Err(HexoError::InvalidId(forge.into()));
    }

    let instance = base_dir.join("instance").join(instance_name);
    let mut config = InstanceConfig::load(&instance).await?;
    if config.version_id != minecraft {
        return Err(HexoError::VersionNotFound(minecraft.into()));
    }
    let archive_name = format!("forge-{forge}-{classifier}.zip");
    let archive = base_dir
        .join("libraries/net/minecraftforge/forge")
        .join(forge)
        .join(&archive_name);
    let url = format!("{FORGE_REPOSITORY}/{forge}/{archive_name}");
    let checksum = get_text(&reqwest::Client::new(), &format!("{url}.sha1")).await?;
    let sha1 = checksum.trim();
    if sha1.len() != 40 || !sha1.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(HexoError::ChecksumMismatch {
            path: archive.display().to_string(),
        });
    }
    download_file(&DownloadTask::new(url, &archive).with_sha1(sha1)).await?;
    download_fml_libraries(minecraft, &instance.join(".minecraft/lib")).await?;

    let modloaders =
        download_legacy_modloaders(minecraft, &base_dir.join("libraries/legacy-modloader")).await?;
    if !modloaders.is_empty() {
        prepare_modloader_launch(&mut config, &base_dir.join("libraries")).await?;
    }
    let mut jars = vec![archive];
    jars.extend(modloaders);
    jars.push(instance.join(format!("{minecraft}.jar")));
    let modded = instance.join(format!("forge-{forge}-client.jar"));
    let merged = instance.join(format!(".forge-{}.jar", uuid::Uuid::new_v4()));
    let temporary = merged.clone();
    let result = tokio::task::spawn_blocking(move || merge_jars(&jars, &temporary))
        .await
        .map_err(|error| HexoError::Other(error.to_string()));
    let result = match result {
        Ok(Ok(())) => fs::rename(&merged, &modded).await.map_err(HexoError::from),
        Ok(Err(error)) | Err(error) => Err(error),
    };
    if result.is_err() {
        let _ = fs::remove_file(&merged).await;
    }
    result?;

    config.loader_type = LoaderType::Forge;
    config.lib_list.retain(|entry| entry.name != CLIENT_ENTRY);
    config.lib_list.push(LibEntry {
        name: CLIENT_ENTRY.into(),
        path: modded,
        sha1: String::new(),
    });
    config.save(&instance).await
}

fn merge_jars(jars: &[PathBuf], output: &Path) -> Result<()> {
    let mut output = ZipWriter::new(File::create_new(output)?);
    let mut written = HashSet::new();
    for path in jars {
        let mut input = ZipArchive::new(File::open(path)?)?;
        for index in 0..input.len() {
            let entry = input.by_index_raw(index)?;
            let name = entry.name()?;
            if entry.is_dir() || entry.enclosed_name().is_none() || name.starts_with("META-INF/") {
                continue;
            }
            if written.insert(name.into_owned()) {
                output.raw_copy_file(entry)?;
            }
        }
    }
    output.finish()?.sync_all()?;
    Ok(())
}

async fn download_legacy_modloaders(minecraft: &str, cache: &Path) -> Result<Vec<PathBuf>> {
    let (modloader, multiplayer_file, multiplayer) = match minecraft {
        "1.1" => (
            "b56d925adc210773e4b2390f8189e16456910da4bcd4276b492bb382ea04f079",
            "ModLoaderMp%201.1%20v2.1.zip",
            "40028c72b76d181189eaaa66d04a2014d89975e0e8a843379d3a19eb5d96213e",
        ),
        "1.2.3" => (
            "3d3fbbd5cefe409c9c7328a57c4cd854e72d7a6dce6e829e2120c0ad5e1a5f63",
            "ModLoaderMP%201.2.3%20v3.zip",
            "b6bc6d0a4fa21361429f39c2847fa47420e60d54263f9a4abc2fe74dea6814c8",
        ),
        "1.2.4" => (
            "0f9bb79149f95061bee539a3120d820043e84ed8747e9ca98756fc34e40b7d7d",
            "ModLoaderMP%201.2.4%20v1.zip",
            "30fc0c1b7dd9693c19fb419353bf9b0533d4d3248772ca93b6f5b78bbc70e5d6",
        ),
        _ => return Ok(Vec::new()),
    };
    let modloader_file = format!("ModLoader%20{minecraft}.zip");
    let tasks: Vec<_> = [
        (multiplayer_file, multiplayer),
        (&modloader_file, modloader),
    ]
    .into_iter()
    .map(|(file, sha256)| {
        let url = format!("{MODLOADER_ARCHIVE}/{sha256}/{file}");
        DownloadTask::new(url, cache.join(format!("{sha256}.zip"))).with_sha256(sha256)
    })
    .collect();
    let paths = tasks.iter().map(|task| task.path.clone()).collect();
    download_batch(tasks, 2, |_, _| {}).await?;
    Ok(paths)
}

/// The original wrapper gives old ModLoader releases a JAR URI that they cannot open as a file.
async fn prepare_modloader_launch(config: &mut InstanceConfig, libraries: &Path) -> Result<()> {
    let artifacts = [
        (
            "org.mcphackers:launchwrapper:1.2.2",
            "org/mcphackers/launchwrapper/1.2.2/launchwrapper-1.2.2.jar",
            "https://maven.glass-launcher.net/releases/",
            "6cfbe995b466b1b4e2e46edbdfc43c336ce6aaa20b85134c80597c4b049df982",
        ),
        (
            "org.ow2.asm:asm:9.9",
            "org/ow2/asm/asm/9.9/asm-9.9.jar",
            "https://maven.fabricmc.net/",
            "03d99a74ad1ee5c71334ef67437f4ef4fe3488caa7c96d8645abc73c8e2017d4",
        ),
        (
            "org.ow2.asm:asm-tree:9.9",
            "org/ow2/asm/asm-tree/9.9/asm-tree-9.9.jar",
            "https://maven.fabricmc.net/",
            "42178f3775c9c63f9e5e1446747d29b4eca4d91bd6e75e5c43cfa372a47d38c6",
        ),
        (
            "org.json:json:20250517",
            "org/json/json/20250517/json-20250517.jar",
            "https://repo.maven.apache.org/maven2/",
            "3ea61b2a06e31edf1c91134fe9106b0ebb16628be169f3db75bc7a2b06b45796",
        ),
    ];
    let mut entries = Vec::new();
    let mut tasks = Vec::new();
    for (name, relative_path, repository, checksum) in artifacts {
        let path = libraries.join(relative_path);
        tasks.push(
            DownloadTask::new(format!("{repository}{relative_path}"), &path).with_sha256(checksum),
        );
        entries.push(LibEntry {
            name: name.into(),
            path,
            sha1: String::new(),
        });
    }
    download_batch(tasks, 4, |_, _| {}).await?;
    config.lib_list.retain(|entry| {
        !entry.name.starts_with("net.minecraft:launchwrapper:")
            && !entries
                .iter()
                .any(|dependency| dependency.name == entry.name)
    });
    entries.append(&mut config.lib_list);
    config.lib_list = entries;
    config.main_class = "org.mcphackers.launchwrapper.Launch".into();
    config.start_args = [
        "--username",
        "${auth_player_name}",
        "--session",
        "${auth_session}",
        "--gameDir",
        "${game_directory}",
        "--assetsDir",
        "${assets_root}",
        "--assetIndex",
        "${assets_index_name}",
        "--disableSkinFix",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    Ok(())
}

async fn download_fml_libraries(minecraft: &str, path: &Path) -> Result<()> {
    const COMMON_1_3_AND_1_4: &[(&str, &str)] = &[
        ("argo-2.25.jar", "bb672829fde76cb163004752b86b0484bd0a7f4b"),
        (
            "guava-12.0.1.jar",
            "b8e78b9af7bf45900e14c6f958486b6ca682195f",
        ),
        (
            "asm-all-4.0.jar",
            "98308890597acb64047f7e896638e0d98753ae82",
        ),
    ];
    const COMMON_1_5: &[(&str, &str)] = &[
        (
            "argo-small-3.2.jar",
            "58912ea2858d168c50781f956fa5b59f0f7c6b51",
        ),
        (
            "guava-14.0-rc3.jar",
            "931ae21fa8014c3ce686aaa621eae565fefb1a6a",
        ),
        (
            "asm-all-4.1.jar",
            "054986e962b88d8660ae4566475658469595ef58",
        ),
        (
            "bcprov-jdk15on-148.jar",
            "960dea7c9181ba0b17e8bab0c06a43f0a5f04e65",
        ),
        (
            "scala-library.jar",
            "458d046151ad179c85429ed7420ffb1eaf6ddf85",
        ),
    ];
    let (common, extra) = match minecraft {
        "1.3.2" => (COMMON_1_3_AND_1_4, None),
        v if v.starts_with("1.4.") => (
            COMMON_1_3_AND_1_4,
            Some((
                "bcprov-jdk15on-147.jar",
                "b6f5d9926b0afbde9f4dbe3db88c5247be7794bb",
            )),
        ),
        "1.5" => (
            COMMON_1_5,
            Some((
                "deobfuscation_data_1.5.zip",
                "5f7c142d53776f16304c0bbe10542014abad6af8",
            )),
        ),
        "1.5.1" => (
            COMMON_1_5,
            Some((
                "deobfuscation_data_1.5.1.zip",
                "22e221a0d89516c1f721d6cab056a7e37471d0a6",
            )),
        ),
        "1.5.2" => (
            COMMON_1_5,
            Some((
                "deobfuscation_data_1.5.2.zip",
                "446e55cd986582c70fcf12cb27bc00114c5adfd9",
            )),
        ),
        _ => return Ok(()),
    };
    let tasks = common
        .iter()
        .copied()
        .chain(extra)
        .map(|(file, sha1)| {
            let url = format!("{FML_REPOSITORY}/{file}");
            DownloadTask::new(url, path.join(file)).with_sha1(sha1)
        })
        .collect();
    download_batch(tasks, 4, |_, _| {}).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use zip::write::SimpleFileOptions;

    #[test]
    fn legacy_forge_overlays_the_client_without_changing_the_vanilla_jar() {
        let directory = tempfile::tempdir().unwrap();
        let vanilla = directory.path().join("vanilla.jar");
        let forge = directory.path().join("forge.zip");
        let merged = directory.path().join("merged.jar");
        let create = |path: &Path, entries: &[(&str, &[u8])]| {
            let mut jar = ZipWriter::new(File::create(path).unwrap());
            for (name, data) in entries {
                jar.start_file(*name, SimpleFileOptions::default()).unwrap();
                jar.write_all(data).unwrap();
            }
            jar.finish().unwrap();
        };
        create(
            &vanilla,
            &[
                ("game.class", b"vanilla"),
                ("texture.png", b"texture"),
                ("META-INF/MOJANG.SF", b"signature"),
            ],
        );
        create(
            &forge,
            &[("game.class", b"forge"), ("forge.class", b"class")],
        );
        let original = std::fs::read(&vanilla).unwrap();
        merge_jars(&[forge, vanilla.clone()], &merged).unwrap();

        let mut jar = ZipArchive::new(File::open(merged).unwrap()).unwrap();
        for (name, expected) in [
            ("game.class", "forge"),
            ("texture.png", "texture"),
            ("forge.class", "class"),
        ] {
            let mut contents = String::new();
            jar.by_name(name)
                .unwrap()
                .read_to_string(&mut contents)
                .unwrap();
            assert_eq!(contents, expected);
        }
        assert_eq!(jar.len(), 3);
        assert!(jar.by_name("META-INF/MOJANG.SF").is_err());
        assert_eq!(std::fs::read(vanilla).unwrap(), original);
    }
}
