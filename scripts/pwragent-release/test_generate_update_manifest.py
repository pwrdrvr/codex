#!/usr/bin/env python3

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("generate-update-manifest.py")
SPEC = importlib.util.spec_from_file_location("generate_update_manifest", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MANIFEST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MANIFEST)


class GenerateUpdateManifestTest(unittest.TestCase):
    def test_manifest_and_last_marker_have_exact_selection_contract(self) -> None:
        version = "0.149.0-pwragent.1"
        release_tag = f"pwragent-v{version}"
        commit = "a" * 40
        with tempfile.TemporaryDirectory() as temporary_directory:
            dist = Path(temporary_directory)
            for platform, _os, _arch, _target, archive_type in MANIFEST.PLATFORMS:
                (
                    dist / f"pwragent-codex-{version}-{platform}.{archive_type}"
                ).write_bytes(platform.encode("utf-8"))
            manifest_path = MANIFEST.generate_manifest(
                dist, version, release_tag, "pwrdrvr/codex", commit
            )
            (dist / MANIFEST.BUNDLE_NAME).write_text("{}\n", encoding="utf-8")
            marker_path = MANIFEST.generate_marker(dist, version, release_tag, commit)

            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            marker = json.loads(marker_path.read_text(encoding="utf-8"))
            self.assertEqual(
                [
                    (item["os"], item["arch"], item["platform"])
                    for item in manifest["artifacts"]
                ],
                [
                    ("darwin", "arm64", "macos-aarch64"),
                    ("darwin", "x64", "macos-x86_64"),
                    ("linux", "arm64", "linux-aarch64"),
                    ("linux", "x64", "linux-x86_64"),
                    ("win32", "x64", "windows-x86_64"),
                ],
            )
            self.assertEqual(
                manifest["capabilities"]["pwrdrvrTokenMiser"],
                {"identity": "pwrdrvr.pwragent.token-miser", "version": 1},
            )
            self.assertEqual(marker["complete"], True)
            self.assertEqual(marker["manifest"]["file"], MANIFEST.MANIFEST_NAME)
            self.assertEqual(
                marker["manifest"]["signatureBundle"], MANIFEST.BUNDLE_NAME
            )
            self.assertEqual(
                marker["manifest"]["sha256"], MANIFEST.sha256(manifest_path)
            )

    def test_missing_platform_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            with self.assertRaisesRegex(ValueError, "release artifact is missing"):
                MANIFEST.generate_manifest(
                    Path(temporary_directory),
                    "0.149.0-pwragent.1",
                    "pwragent-v0.149.0-pwragent.1",
                    "pwrdrvr/codex",
                    "b" * 40,
                )


if __name__ == "__main__":
    unittest.main()
