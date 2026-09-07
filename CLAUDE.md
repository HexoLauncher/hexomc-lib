# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`hexomc-lib` is a Rust library (not a binary) providing the core of a Minecraft launcher: version manifest queries, vanilla/Fabric/Forge/NeoForge installation, game launching, Java detection/download, Microsoft authentication (Device Code Flow), mod detection + CurseForge lookup + auto-update, and resource-pack/shader/map detection.

Source comments and error messages are in Traditional Chinese; follow that convention when adding to existing files.

## Commands

```bash
cargo build                       # build the library
cargo test                        # run unit tests (skips network-gated tests)
cargo test -- --ignored           # run integration tests in tests/integration.rs (needs network)
cargo test <name>                 # run a single test by name substring
cargo run --example launch_vanilla    # also: launch_fabric, launch_neoforge
cargo clippy                      # lint
```

Integration tests (`tests/integration.rs`) are marked `#[ignore = "需要網路"]` because they hit live Minecraft/Fabric/Forge/NeoForge APIs; they only run with `--ignored`. The examples are the primary end-to-end exercise of the install→launch pipeline and write to `./mc_data`.

## Architecture

The public surface is re-exported flat from [src/lib.rs](src/lib.rs) — check it first to see what's stable API vs. internal. Modules:

- **`version`** — fetches and parses Mojang's version manifest and per-version JSON, including library/JVM rule evaluation (`check_library_rule`, `check_jvm_rule`).
- **`download`** — [src/download/downloader.rs](src/download/downloader.rs) is the shared download engine: `DownloadTask` (with optional SHA1), `download_file` (retries, skips if SHA1 already matches), and `download_batch` (concurrent via `futures::stream`). All installers route through this.
- **`install`** — one module per loader (`vanilla`, `fabric`, `forge`, `neoforge`) plus `loader.rs`, the DI layer.
- **`launch`** — assembles classpath, extracts natives, substitutes `${placeholder}` args, writes a Java `@argfile`, and spawns the process.
- **`java`**, **`auth`**, **`mods`**, **`assets`** — supporting subsystems (Java detect/install, Microsoft Device Code Flow, mod/CurseForge/updater, resource-pack/shader/map detection).

### The install pipeline (most important to understand)

Installation is built around the `LoaderInstaller` trait in [src/install/loader.rs](src/install/loader.rs) — a dependency-injection abstraction so callers only touch `&dyn LoaderInstaller`. Key rules baked into `install_with_loader`:

- **Vanilla's `install` is the full install.** Fabric/Forge/NeoForge installers only *overlay* on top of an already-installed vanilla instance; `install_with_loader` runs `VanillaInstaller` first for any non-vanilla loader.
- **`instance_name` is chosen by the caller, not derived from the version.** It names the `instance/{name}/` subdirectory and exists specifically so the same MC version with different loaders (e.g. `1.21.1-vanilla` vs `1.21.1-fabric`) don't collide. Every install/launch function threads this through.
- Progress is reported via `ProgressFn = Arc<dyn Fn(usize, usize, &str) + Send + Sync>` — cloneable across async boundaries. Use `no_progress()` when unneeded.

### On-disk layout

A launcher root (`base_dir`, e.g. `./mc_data`) contains shared `assets/` and `libraries/`, plus per-instance `instance/{instance_name}/`. Each instance dir holds `instance_config.json` (the serialized `InstanceConfig`: main class, `start_args` with placeholders, library/native lists, `assets_id`, `java_version`) and a `natives/` dir. `InstanceConfig::save`/`load` is the contract between the install and launch phases — launch reads this file, never the loader APIs.

### Launch specifics

[src/launch/launcher.rs](src/launch/launcher.rs) canonicalizes `base_dir` and strips the Windows `\\?\` extended-path prefix (Java can't parse it) and normalizes to forward slashes. Args are passed via a written `@argfile` to avoid OS command-length limits. Auth token/UUID/XUID on `LaunchOptions` are optional — absent means offline mode (`LaunchOptions::offline(...)`).

## Error handling

All fallible functions return `Result<T> = std::result::Result<T, HexoError>`. `HexoError` ([src/error.rs](src/error.rs)) has `#[from]` conversions for reqwest/io/serde_json/zip, so use `?` freely. Prefer the specific variants (`ChecksumMismatch`, `VersionNotFound`, `JavaNotFound`, `ProcessorFailed`, etc.) over `HexoError::Other`.
