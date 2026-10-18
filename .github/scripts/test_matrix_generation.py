import unittest
from urllib.error import HTTPError
from unittest.mock import patch

import test_matrix


MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
FORGE_URL = (
    "https://files.minecraftforge.net/net/minecraftforge/forge/maven-metadata.json"
)
FABRIC_URL = "https://meta.fabricmc.net/v2/versions"


class MatrixTests(unittest.TestCase):
    def setUp(self):
        self.responses = {
            MANIFEST_URL: {
                "versions": [{"id": version} for version in test_matrix.VERSIONS]
            },
            FORGE_URL: {
                version: [f"{version}-9.9.0", f"{version}-10.0.0"]
                for version in ("1.6.4", "1.7.10", "1.12.2")
            },
            f"{FABRIC_URL}/game": [
                {"version": "26.3", "stable": True},
                {"version": "26w14a", "stable": False},
            ],
        }
        for version in ("26.3", "26w14a"):
            self.responses[f"{FABRIC_URL}/loader/{version}"] = [
                {"loader": {"version": "0.20.0-beta", "stable": False}},
                {"loader": {"version": "0.19.5", "stable": True}},
            ]
        fetch_patch = patch.object(test_matrix, "fetch", side_effect=self.fetch)
        self.fetcher = fetch_patch.start()
        self.addCleanup(fetch_patch.stop)

    def fetch(self, url):
        if url not in self.responses:
            raise HTTPError(url, 400, "Bad Request", None, None)
        response = self.responses[url]
        if isinstance(response, Exception):
            raise response
        return response

    def test_unsupported_fabric_versions_are_not_queried(self):
        matrix, summary = test_matrix.build_matrix()
        self.assertEqual(
            {
                row["minecraft"]
                for row in matrix["include"]
                if row["loader"] == "vanilla"
            },
            set(test_matrix.VERSIONS),
        )
        self.assertEqual(len(matrix["include"]), 12)
        for version in ("1.0", "1.6.4", "1.7.10", "1.12.2", "15w14a"):
            self.assertNotIn(
                unittest.mock.call(f"{FABRIC_URL}/loader/{version}"),
                self.fetcher.call_args_list,
            )
        self.assertIn("| 1.0 | Test | Unsupported by upstream |", summary)

    def test_supported_snapshots_use_stable_loader(self):
        matrix, _ = test_matrix.build_matrix()
        self.assertIn(
            {"minecraft": "26w14a", "loader": "fabric", "loader_version": "0.19.5"},
            matrix["include"],
        )

    def test_forge_versions_are_sorted_numerically(self):
        matrix, _ = test_matrix.build_matrix()
        forge = [row for row in matrix["include"] if row["loader"] == "forge"]
        self.assertEqual(len(forge), 3)
        self.assertTrue(all(row["loader_version"].endswith("-10.0.0") for row in forge))

    def test_supported_fabric_http_errors_are_not_hidden(self):
        url = f"{FABRIC_URL}/loader/26.3"
        for status in (400, 429, 500):
            with self.subTest(status=status):
                self.responses[url] = HTTPError(url, status, "API error", None, None)
                with self.assertRaises(HTTPError) as raised:
                    test_matrix.build_matrix()
                self.assertEqual(raised.exception.code, status)

    def test_missing_required_fabric_fails(self):
        self.responses[f"{FABRIC_URL}/game"] = []
        with self.assertRaisesRegex(
            ValueError, "Fabric must be available for Minecraft 26.3"
        ):
            test_matrix.build_matrix()

    def test_missing_required_forge_fails(self):
        del self.responses[FORGE_URL]["1.6.4"]
        with self.assertRaisesRegex(
            ValueError, "Forge must be available for Minecraft 1.6.4"
        ):
            test_matrix.build_matrix()

    def test_missing_minecraft_version_fails(self):
        self.responses[MANIFEST_URL]["versions"] = [{"id": "1.0"}]
        with self.assertRaisesRegex(
            ValueError, "Minecraft versions missing from Mojang"
        ):
            test_matrix.build_matrix()


if __name__ == "__main__":
    unittest.main()
