# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`hexomc-lib` is a Rust library (not a binary) providing the core of a Minecraft launcher: version manifest queries, vanilla/Fabric/Forge/NeoForge installation, game launching, Java detection/download, Microsoft authentication (Device Code Flow), mod detection + Modrinth/CurseForge lookup + auto-update, modpack installation (Modrinth `.mrpack`, CurseForge, ATLauncher), and resource-pack/shader/map detection.

Write only English doc comments (`///`, `//!`) — no inline `//` comments and no Chinese in new code, including error and progress strings. All Rust sources (`src/`, `examples/`, `tests/`) are English-only; keep them that way. `README.md` is still Chinese by design.

## Commands

```bash
cargo build                       # build the library
cargo test --lib                  # unit tests only (offline)
cargo test --test integration     # integration tests (needs network)
cargo test -- --ignored           # network-gated unit tests marked #[ignore]
cargo test <name>                 # run a single test by name substring
cargo run --example launch_vanilla    # also: launch_fabric, launch_forge, launch_neoforge, launch_with_output
cargo run --example launch_modpack -- <pack.mrpack|pack.zip>   # install + launch a modpack
cargo clippy --all-targets        # lint
```

Integration tests (`tests/integration.rs`) are **not** `#[ignore]`d: they hit live Mojang/Fabric/Forge/NeoForge/ATLauncher APIs and run under plain `cargo test`, so use `cargo test --lib` offline. A few network unit tests in `version::manifest` are `#[ignore]`d and need `--ignored`. The examples are the primary end-to-end exercise of the install→launch pipeline and write to `./mc_data`.

## Architecture

The public surface is re-exported flat from [src/lib.rs](src/lib.rs) — check it first to see what's stable API vs. internal. Modules:

- **`version`** — fetches and parses Mojang's version manifest and per-version JSON, including library/JVM rule evaluation (`check_library_rule`, `check_jvm_rule`).
- **`download`** — [src/download/downloader.rs](src/download/downloader.rs) is the shared download engine: `DownloadTask` (with optional SHA1), `download_file` (retries, skips if SHA1 already matches), and `download_batch` (concurrent via `futures::stream`). All installers route through this.
- **`install`** — one module per loader (`vanilla`, `fabric`, `forge`, `neoforge`) plus `loader.rs`, the DI layer.
- **`launch`** — assembles classpath, extracts natives, substitutes `${placeholder}` args, writes a Java `@argfile`, and spawns the process.
- **`modpack`** — modpack installers built on top of `install` (see below).
- **`java`**, **`auth`**, **`mods`**, **`assets`** — supporting subsystems (Java detect/install, Microsoft Device Code Flow, mod detection + Modrinth/CurseForge clients + updater, resource-pack/shader/map detection).

### The install pipeline (most important to understand)

Installation is built around the `LoaderInstaller` trait in [src/install/loader.rs](src/install/loader.rs) — a dependency-injection abstraction so callers only touch `&dyn LoaderInstaller`. Key rules baked into `install_with_loader`:

- **Vanilla's `install` is the full install.** Fabric/Forge/NeoForge installers only *overlay* on top of an already-installed vanilla instance; `install_with_loader` runs `VanillaInstaller` first for any non-vanilla loader.
- **`instance_name` is chosen by the caller, not derived from the version.** It names the `instance/{name}/` subdirectory and exists specifically so the same MC version with different loaders (e.g. `1.21.1-vanilla` vs `1.21.1-fabric`) don't collide. Every install/launch function threads this through.
- Progress is reported via `ProgressFn = Arc<dyn Fn(usize, usize, &str) + Send + Sync>` — cloneable across async boundaries. Use `no_progress()` when unneeded.

### Modpacks

[src/modpack/mod.rs](src/modpack/mod.rs) holds the shared flow; each format module only parses its metadata into a `ModpackInfo` (MC version, `LoaderType`, bare loader version) and lists files:

- **`mrpack`** — Modrinth `.mrpack` (`modrinth.index.json`, `overrides/` then `client-overrides/`; files with client env `unsupported` are skipped).
- **`cfpack`** — CurseForge zip (`manifest.json` + overrides). Download URLs come from `CurseForgeClient::get_files`/`get_mods` (API key required); `classId` picks `mods`/`resourcepacks`/`shaderpacks`. Files with a null `downloadUrl` are returned as `ManualDownload`, never fetched via a CDN workaround.
- **`atpack`** — ATLauncher packs have no file format: `Configs.json` / `Configs.zip` are fetched from `download.nodecdn.net/containers/atl/packs/{safeName}/versions/{version}/`, the version list from `api.atlauncher.com/v1/pack/{safeName}`. That API is behind Cloudflare and returns 403 without a User-Agent. Files carry MD5 (checked after download, since `DownloadTask` only verifies SHA1).

`install_pack_loader` reuses `install_with_loader`; for Forge/NeoForge it installs vanilla first so a `None` `java_path` can be resolved with `find_java(InstanceConfig.java_version)`. Modpacks give bare Forge versions (`47.2.0`), which are mapped to the MC-prefixed strings from `get_forge_versions`. Quilt, LegacyFabric and NeoForge 1.20.1 (old `forge` artifact) return `UnsupportedLoader`. Every pack-provided path goes through `safe_join` / zip `enclosed_name` so a pack can't write outside the instance. Results report `manual_downloads` and `skipped` (unsupported ATLauncher entry types) instead of failing.

### On-disk layout

A launcher root (`base_dir`, e.g. `./mc_data`) contains shared `assets/` and `libraries/`, plus per-instance `instance/{instance_name}/`. Each instance dir holds `instance_config.json` (the serialized `InstanceConfig`: main class, `start_args` with placeholders, library/native lists, `assets_id`, `java_version`) and a `natives/` dir. `InstanceConfig::save`/`load` is the contract between the install and launch phases — launch reads this file, never the loader APIs.

### Launch specifics

[src/launch/launcher.rs](src/launch/launcher.rs) canonicalizes `base_dir` and strips the Windows `\\?\` extended-path prefix (Java can't parse it) and normalizes to forward slashes. Args are passed via a written `@argfile` to avoid OS command-length limits. Auth token/UUID/XUID on `LaunchOptions` are optional — absent means offline mode (`LaunchOptions::offline(...)`).

All three entry points share `prepare_command` (builds the java `Command` without spawning): `launch` inherits stdio and returns `std::process::Child`; `launch_with_output` / `launch_with_channel` pipe stdout+stderr and return a `GameProcess` ([src/launch/output.rs](src/launch/output.rs)), which delivers `OutputLine { kind, line }` via an `OutputFn` callback or an unbounded channel. `GameProcess::wait` waits for the process *and* drains the reader tasks, so no line is lost.

## Error handling

All fallible functions return `Result<T> = std::result::Result<T, HexoError>`. `HexoError` ([src/error.rs](src/error.rs)) has `#[from]` conversions for reqwest/io/serde_json/zip, so use `?` freely. Prefer the specific variants (`ChecksumMismatch`, `VersionNotFound`, `JavaNotFound`, `ProcessorFailed`, etc.) over `HexoError::Other`.
