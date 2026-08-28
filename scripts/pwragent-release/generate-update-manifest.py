#!/usr/bin/env python3
"""Generate the deterministic PwrAgent Codex update manifest and completion marker."""

import argparse
import hashlib
import json
from pathlib import Path
import re


PRODUCT = "pwragent-codex"
MANIFEST_NAME = "pwragent-codex-update-v1.json"
BUNDLE_NAME = "pwragent-codex-update-v1.json.sigstore.json"
MARKER_NAME = "pwragent-codex-publication-complete-v1.json"
PLATFORMS = (
    ("macos-aarch64", "darwin", "arm64", "aarch64-apple-darwin", "tar.gz"),
    ("macos-x86_64", "darwin", "x64", "x86_64-apple-darwin", "tar.gz"),
    ("linux-aarch64", "linux", "arm64", "aarch64-unknown-linux-gnu", "tar.gz"),
    ("linux-x86_64", "linux", "x64", "x86_64-unknown-linux-gnu", "tar.gz"),
    ("windows-x86_64", "win32", "x64", "x86_64-pc-windows-msvc", "zip"),
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: Path, value: object) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )


def generate_manifest(
    dist: Path,
    version: str,
    release_tag: str,
    repository: str,
    source_commit: str,
) -> Path:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise ValueError(f"version is not SemVer: {version!r}")
    if release_tag != f"pwragent-v{version}":
        raise ValueError("release tag must be pwragent-v followed by the exact version")
    if not re.fullmatch(r"[0-9a-f]{40}", source_commit):
        raise ValueError("source commit must be a full lowercase SHA-1")

    artifacts = []
    for platform, operating_system, architecture, target, archive_type in PLATFORMS:
        filename = f"{PRODUCT}-{version}-{platform}.{archive_type}"
        artifact_path = dist / filename
        if not artifact_path.is_file():
            raise ValueError(f"release artifact is missing: {filename}")
        artifacts.append(
            {
                "arch": architecture,
                "archiveType": archive_type,
                "file": filename,
                "os": operating_system,
                "platform": platform,
                "sha256": sha256(artifact_path),
                "size": artifact_path.stat().st_size,
                "target": target,
            }
        )

    manifest_path = dist / MANIFEST_NAME
    write_json(
        manifest_path,
        {
            "artifacts": artifacts,
            "capabilities": {
                "codeModeOutputReducer": {
                    "intentContextVersion": 1,
                    "protocolVersion": 1,
                },
                "pwrdrvrTokenMiser": {
                    "identity": "pwrdrvr.pwragent.token-miser",
                    "version": 1,
                },
            },
            "product": PRODUCT,
            "releaseTag": release_tag,
            "schemaVersion": 1,
            "source": {
                "commit": source_commit,
                "repository": repository,
            },
            "version": version,
        },
    )
    return manifest_path


def generate_marker(
    dist: Path, version: str, release_tag: str, source_commit: str
) -> Path:
    manifest_path = dist / MANIFEST_NAME
    bundle_path = dist / BUNDLE_NAME
    if not manifest_path.is_file() or not bundle_path.is_file():
        raise ValueError(
            "the signed manifest and Sigstore bundle must exist before the marker"
        )
    marker_path = dist / MARKER_NAME
    write_json(
        marker_path,
        {
            "complete": True,
            "manifest": {
                "file": MANIFEST_NAME,
                "sha256": sha256(manifest_path),
                "signatureBundle": BUNDLE_NAME,
                "signatureFormat": "sigstore-bundle-v0.3",
            },
            "product": PRODUCT,
            "releaseTag": release_tag,
            "schemaVersion": 1,
            "sourceCommit": source_commit,
            "version": version,
        },
    )
    return marker_path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("manifest", "marker"))
    parser.add_argument("--dist", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--release-tag", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--repository", default="pwrdrvr/codex")
    args = parser.parse_args()
    if args.mode == "manifest":
        generate_manifest(
            args.dist,
            args.version,
            args.release_tag,
            args.repository,
            args.source_commit,
        )
    else:
        generate_marker(args.dist, args.version, args.release_tag, args.source_commit)


if __name__ == "__main__":
    main()
