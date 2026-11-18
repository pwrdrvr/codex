#!/usr/bin/env python3
"""Stamp and verify the Rust workspace version for PwrAgent Codex builds.

Upstream keeps `version = "0.0.0"` in `codex-rs/Cargo.toml` on main and only
writes the real version on its release tags. This fork builds from
non-release upstream commits, so without stamping every
`env!("CARGO_PKG_VERSION")` in the binaries reports 0.0.0. The most visible
consequence is the models manager sending `client_version=0.0.0` to the
backend `/models` endpoint, which then hides every model gated on a minimum
client version.

`CODEX_BUILD_VERSION` (the downstream `X.Y.Z-pwragent.N` string) only feeds the
`--version` output and a few user-agent sites. It is not a substitute for the
Cargo package version, and the `--version` checks cannot detect this failure.

Usage, from the repository root:

    stamp-workspace-version.py stamp [--expect-release-version VERSION]
    stamp-workspace-version.py verify

`stamp` writes the plain upstream SemVer from
`scripts/pwragent-release/upstream-version.txt` into `[workspace.package]` and
into the `Cargo.lock` entries of the workspace members that inherit it, so
`--locked` builds keep working. It is idempotent.

`verify` fails unless the workspace members that report the client version to
the backend have exactly that version in both their resolved manifests and
`Cargo.lock`.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Iterable


ROOT = Path(__file__).resolve().parents[2]
UPSTREAM_VERSION_PATH = ROOT / "scripts/pwragent-release/upstream-version.txt"
WORKSPACE_DIR = ROOT / "codex-rs"

# Crates whose CARGO_PKG_VERSION reaches the backend or the shipped binaries.
# `codex-models-manager` builds the `/models` `client_version` query.
GUARDED_PACKAGES = (
    "codex-models-manager",
    "codex-cli",
    "codex-app-server",
    "codex-code-mode-host",
)

SEMVER_CORE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
WORKSPACE_PACKAGE_VERSION = re.compile(
    r'(?ms)(^\[workspace\.package\][^\[]*?^version\s*=\s*")([^"]*)(")'
)
INHERITS_WORKSPACE_VERSION = re.compile(
    r"(?m)^version(\.workspace\s*=\s*true|\s*=\s*\{\s*workspace\s*=\s*true\s*\})"
)


class StampError(Exception):
    pass


def read_upstream_version(path: Path = UPSTREAM_VERSION_PATH) -> str:
    version = path.read_text(encoding="utf-8").strip()
    if not SEMVER_CORE.match(version):
        raise StampError(
            f"{path.name} must hold a plain MAJOR.MINOR.PATCH version, got {version!r}"
        )
    if version == "0.0.0":
        raise StampError(f"{path.name} must not be the 0.0.0 placeholder")
    return version


def check_release_version(release_version: str, upstream_version: str) -> None:
    """Require the downstream version to be built on the stamped upstream one."""
    if release_version == upstream_version:
        return
    if release_version.startswith(f"{upstream_version}-") or release_version.startswith(
        f"{upstream_version}+"
    ):
        return
    raise StampError(
        f"release version {release_version!r} is not based on upstream version "
        f"{upstream_version!r}; update scripts/pwragent-release/upstream-version.txt"
    )


def read_text(path: Path) -> str:
    # newline="" keeps CRLF checkouts on Windows byte-for-byte intact.
    with path.open(encoding="utf-8", newline="") as handle:
        return handle.read()


def write_text(path: Path, text: str) -> None:
    with path.open("w", encoding="utf-8", newline="") as handle:
        handle.write(text)


def workspace_package_version(manifest: str) -> str:
    match = WORKSPACE_PACKAGE_VERSION.search(manifest)
    if match is None:
        raise StampError("codex-rs/Cargo.toml has no [workspace.package] version")
    return match.group(2)


def stamp_manifest(manifest: str, version: str) -> str:
    workspace_package_version(manifest)
    return WORKSPACE_PACKAGE_VERSION.sub(
        lambda match: f"{match.group(1)}{version}{match.group(3)}", manifest, count=1
    )


def stamp_lockfile(lockfile: str, members: Iterable[str], version: str) -> str:
    """Rewrite the version of each named local (sourceless) lockfile package."""
    pending = set(members)
    newline = "\r\n" if "\r\n" in lockfile else "\n"
    blocks = lockfile.split(f"{newline}{newline}")
    for index, block in enumerate(blocks):
        if not block.startswith("[[package]]"):
            continue
        name = re.search(r'(?m)^name = "([^"]+)"', block)
        if name is None or name.group(1) not in pending:
            continue
        if re.search(r"(?m)^source = ", block):
            continue
        blocks[index] = re.sub(
            r'(?m)^version = "[^"]*"', f'version = "{version}"', block, count=1
        )
        pending.discard(name.group(1))
    if pending:
        raise StampError(
            "Cargo.lock has no local entry for workspace members: "
            + ", ".join(sorted(pending))
        )
    return f"{newline}{newline}".join(blocks)


def local_lockfile_versions(lockfile: str) -> dict[str, str]:
    """Map each local (sourceless) lockfile package name to its version."""
    versions = {}
    for block in re.split(r"(?:\r?\n){2}", lockfile):
        if not block.startswith("[[package]]") or re.search(r"(?m)^source = ", block):
            continue
        name = re.search(r'(?m)^name = "([^"]+)"', block)
        version = re.search(r'(?m)^version = "([^"]+)"', block)
        if name and version:
            versions[name.group(1)] = version.group(1)
    return versions


def cargo_metadata(workspace_dir: Path) -> dict:
    # --no-deps needs neither the network nor the registry; it does not check
    # Cargo.lock either, so verify() reads the lockfile itself.
    command = ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"]
    result = subprocess.run(
        command, cwd=workspace_dir, check=False, capture_output=True, text=True
    )
    if result.returncode != 0:
        raise StampError(f"{' '.join(command)} failed:\n{result.stderr.strip()}")
    return json.loads(result.stdout)


def version_inheriting_members(metadata: dict) -> list[str]:
    members = set(metadata["workspace_members"])
    names = []
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        manifest = read_text(Path(package["manifest_path"]))
        if INHERITS_WORKSPACE_VERSION.search(manifest):
            names.append(package["name"])
    return sorted(names)


def stamp(workspace_dir: Path, version: str) -> None:
    manifest_path = workspace_dir / "Cargo.toml"
    lockfile_path = workspace_dir / "Cargo.lock"
    # Read membership before rewriting so Cargo sees a consistent workspace.
    members = version_inheriting_members(cargo_metadata(workspace_dir))
    if not members:
        raise StampError("no workspace member inherits [workspace.package] version")
    manifest = read_text(manifest_path)
    previous = workspace_package_version(manifest)
    write_text(lockfile_path, stamp_lockfile(read_text(lockfile_path), members, version))
    write_text(manifest_path, stamp_manifest(manifest, version))
    print(
        f"stamped codex-rs workspace version {previous} -> {version} "
        f"({len(members)} members)"
    )


def verify(workspace_dir: Path, version: str) -> None:
    manifest_version = workspace_package_version(read_text(workspace_dir / "Cargo.toml"))
    if manifest_version != version:
        raise StampError(
            f"codex-rs/Cargo.toml workspace version is {manifest_version!r}, "
            f"expected {version!r} from upstream-version.txt"
        )
    metadata = cargo_metadata(workspace_dir)
    members = set(metadata["workspace_members"])
    resolved = {
        package["name"]: package["version"]
        for package in metadata["packages"]
        if package["id"] in members
    }
    locked = local_lockfile_versions(read_text(workspace_dir / "Cargo.lock"))
    for name in GUARDED_PACKAGES:
        for source, actual in (("Cargo.toml", resolved.get(name)), ("Cargo.lock", locked.get(name))):
            if actual != version:
                raise StampError(
                    f"{name} has version {actual!r} in {source}, expected {version!r}; "
                    "the backend would receive the wrong client_version"
                )
    print(f"verified codex-rs workspace version {version}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    subcommands = parser.add_subparsers(dest="command", required=True)
    stamp_parser = subcommands.add_parser("stamp")
    stamp_parser.add_argument(
        "--expect-release-version",
        help="downstream version that must be based on upstream-version.txt",
    )
    subcommands.add_parser("verify")
    args = parser.parse_args(argv)

    try:
        version = read_upstream_version()
        if args.command == "stamp":
            if args.expect_release_version:
                check_release_version(args.expect_release_version, version)
            stamp(WORKSPACE_DIR, version)
        else:
            verify(WORKSPACE_DIR, version)
    except StampError as error:
        print(f"::error::workspace version: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
