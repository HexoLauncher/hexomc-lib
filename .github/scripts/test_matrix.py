import json
import os
import re
import urllib.request
from pathlib import Path


VERSIONS = ("1.0", "26.3", "1.12.2", "1.7.10", "1.6.4", "15w14a", "26w14a")


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "hexomc-lib-ci"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def build_matrix():
    manifest = fetch("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
    known_versions = {entry["id"] for entry in manifest["versions"]}
    missing = set(VERSIONS) - known_versions
    if missing:
        raise ValueError(f"Minecraft versions missing from Mojang: {sorted(missing)}")
    forge = fetch(
        "https://files.minecraftforge.net/net/minecraftforge/forge/maven-metadata.json"
    )
    fabric_games = {
        entry["version"]
        for entry in fetch("https://meta.fabricmc.net/v2/versions/game")
    }
    matrix = []
    summary = ["| Minecraft | Vanilla | Fabric | Forge |", "| --- | --- | --- | --- |"]
    for version in VERSIONS:
        fabric = (
            fetch(f"https://meta.fabricmc.net/v2/versions/loader/{version}")
            if version in fabric_games
            else []
        )
        fabric_versions = [
            entry["loader"]["version"] for entry in fabric if entry["loader"]["stable"]
        ]
        forge_versions = forge.get(version, [])
        if version == "26.3" and not fabric_versions:
            raise ValueError("Fabric must be available for Minecraft 26.3")
        if version in ("1.6.4", "1.7.10", "1.12.2") and not forge_versions:
            raise ValueError(f"Forge must be available for Minecraft {version}")
        loaders = {"vanilla": "vanilla"}
        if fabric_versions:
            loaders["fabric"] = fabric_versions[0]
        if forge_versions:
            loaders["forge"] = max(
                forge_versions,
                key=lambda value: tuple(
                    int(part) for part in re.findall(r"\d+", value)
                ),
            )
        for loader, loader_version in loaders.items():
            matrix.append(
                {
                    "minecraft": version,
                    "loader": loader,
                    "loader_version": loader_version,
                }
            )
        summary.append(
            f"| {version} | Test | {loaders.get('fabric', 'Unsupported by upstream')} | "
            f"{loaders.get('forge', 'Unsupported by upstream')} |"
        )
    return {"include": matrix}, "\n".join(summary) + "\n"


if __name__ == "__main__":
    matrix, summary = build_matrix()
    print(json.dumps(matrix))
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        output.write(f"matrix={json.dumps(matrix)}\n")
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as output:
        output.write(summary)
