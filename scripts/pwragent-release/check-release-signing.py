#!/usr/bin/env python3
"""Pin fail-closed invariants in the downstream release signing workflow.

Adapted from the equivalent check in pwrdrvr/grok-build. The point is that the
signed release path cannot quietly degrade into an unsigned one: the assertions
below fail the build if a signing job loses its environment, if a preparation
job starts reading secrets, or if the release stops depending on both signers.
"""

from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = ROOT / ".github/workflows/pwragent-release.yml"
CHECK_WORKFLOW_PATH = ROOT / ".github/workflows/pwragent-release-check.yml"
DEFAULT_CLIENT_PATH = ROOT / "codex-rs/login/src/auth/default_client.rs"
ROLLOUT_RECORDER_PATH = ROOT / "codex-rs/rollout/src/recorder.rs"
UNSIGNED_WORKFLOW_PATH = ROOT / ".github/workflows/pwragent-macos-unsigned.yml"
WINDOWS_SIGNER_PATH = ROOT / "scripts/pwragent-release/sign-windows-binaries.ps1"
WINDOWS_SIGNING_PREPARER_PATH = (
    ROOT / "scripts/pwragent-release/prepare-trusted-signing.ps1"
)
UPSTREAM_VERSION_PATH = ROOT / "scripts/pwragent-release/upstream-version.txt"
WINDOWS_SIGNING_VERIFIER_PATH = (
    ROOT / "scripts/pwragent-release/verify-trusted-signing-tools.ps1"
)
RUNBOOK_PATH = ROOT / "docs/pwragent-distribution.md"

# Every executable the downstream distribution ships and therefore must sign.
UNIX_BINARIES = ("codex", "codex-app-server", "codex-code-mode-host")
WINDOWS_BINARIES = UNIX_BINARIES + (
    "codex-windows-sandbox-setup",
    "codex-command-runner",
)
# linux x2, macos x2, windows x1
EXPECTED_RELEASE_ASSETS = 5


def fail(message: str) -> None:
    print(f"release signing contract: {message}", file=sys.stderr)
    raise SystemExit(1)


def require(text: str, fragment: str, scope: str) -> None:
    if fragment not in text:
        fail(f"{scope} must contain {fragment!r}")


def job(workflow: str, name: str) -> str:
    match = re.search(
        rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  [a-z0-9][a-z0-9-]*:\n|\Z)",
        workflow,
    )
    if match is None:
        fail(f"workflow job {name!r} is missing")
    return match.group(0)


workflow = WORKFLOW_PATH.read_text(encoding="utf-8")
check_workflow = CHECK_WORKFLOW_PATH.read_text(encoding="utf-8")
default_client = DEFAULT_CLIENT_PATH.read_text(encoding="utf-8")
rollout_recorder = ROLLOUT_RECORDER_PATH.read_text(encoding="utf-8")
unsigned_workflow = UNSIGNED_WORKFLOW_PATH.read_text(encoding="utf-8")
windows_signer = WINDOWS_SIGNER_PATH.read_text(encoding="utf-8")
windows_signing_preparer = WINDOWS_SIGNING_PREPARER_PATH.read_text(encoding="utf-8")
windows_signing_verifier = WINDOWS_SIGNING_VERIFIER_PATH.read_text(encoding="utf-8")
upstream_version = UPSTREAM_VERSION_PATH.read_text(encoding="utf-8").strip()

try:
    upstream_version_parts = tuple(int(part) for part in upstream_version.split("."))
except ValueError:
    fail(f"invalid upstream version baseline: {upstream_version!r}")
if len(upstream_version_parts) != 3 or upstream_version_parts < (0, 125, 0):
    fail(f"upstream version baseline must be SemVer >= 0.125.0: {upstream_version!r}")
runbook = RUNBOOK_PATH.read_text(encoding="utf-8")

require(workflow, "id-token: none", "workflow")
require(
    workflow,
    "run: python3 scripts/pwragent-release/check-release-signing.py",
    "metadata job",
)
require(workflow, "pull_request:", "workflow")
require(workflow, "- labeled", "workflow")
require(workflow, "- synchronize", "workflow")
require(workflow, "'ci:release-signing'", "workflow")
for fragment in (
    "scripts/pwragent-release/upstream-version.txt",
    'export CODEX_BUILD_VERSION="$CODEX_VERSION"',
    "codex-cli ${CODEX_VERSION}",
    "codex-app-server ${CODEX_VERSION}",
    "codex-code-mode-host ${CODEX_VERSION}",
):
    require(workflow, fragment, "compiled version contract")
for fragment in (
    'option_env!("CODEX_BUILD_VERSION")',
    'env!("CARGO_PKG_VERSION")',
    "{}/{BUILD_VERSION}",
):
    require(default_client, fragment, "runtime version contract")
for fragment in (
    'option_env!("CODEX_BUILD_VERSION")',
    'env!("CARGO_PKG_VERSION")',
    "cli_version: BUILD_VERSION.to_string()",
):
    require(rollout_recorder, fragment, "rollout version contract")
for fragment in (
    "github.event.action == 'labeled' || github.event.action == 'unlabeled'",
    "github.event.label.name != 'ci:release-signing'",
    "github.run_id",
    "github.event.action == 'synchronize'",
    "github.event.action == 'reopened'",
    "github.event.label.name == 'ci:release-signing'",
):
    require(workflow, fragment, "PR signing trigger guard")

# Upstream's own release pipeline runs on self-hosted runner groups that do not
# exist on this fork. Keeping the downstream build on hosted runners is what
# makes it runnable here at all, so it is part of the contract. Comments are
# stripped first so the header can name the upstream runners it is avoiding.
workflow_code = "\n".join(
    line for line in workflow.splitlines() if not line.lstrip().startswith("#")
)
if "-runners" in workflow_code or "self-hosted" in workflow_code:
    fail("the downstream workflow must only use GitHub-hosted runners")

build = job(workflow, "build")
macos_sign = job(workflow, "macos-sign")
windows_prepare = job(workflow, "windows-prepare")
windows_sign = job(workflow, "windows-sign")
release_candidate = job(workflow, "release-candidate")
release = job(workflow, "release")

for name, section in (("build", build), ("windows-prepare", windows_prepare)):
    if "environment:" in section or "secrets." in section:
        fail(f"{name} must remain a no-secret preparation job")

# The macOS payload ships with its own checksum file. GitHub Actions cannot
# export per-entry outputs from a matrix job, so unlike windows-sign there is no
# out-of-band digest to compare against on this side; within-run artifacts are
# the trust boundary. Do not "strengthen" this with a second artifact holding
# the same digest -- the same job writes both, so it proves nothing.
require(build, "name: signing-input-${{ matrix.platform }}", "build")

for binary in UNIX_BINARIES:
    require(build, f"--bin {binary}", "build")

for fragment in (
    "scripts/pwragent-release/prepare-trusted-signing.ps1",
    "-OutputRoot signing-tools",
    "stage/windows-x86_64 signing-tools scripts/pwragent-release",
    "signing-input-sha256:",
):
    require(windows_prepare, fragment, "windows-prepare")

for binary in WINDOWS_BINARIES:
    require(windows_prepare, f"--bin {binary}", "windows-prepare")

for fragment in (
    "startsWith(github.ref, 'refs/tags/pwragent-v')",
    "contains(github.event.pull_request.labels.*.name, 'ci:release-signing')",
    "environment: apple-signing",
    "CSC_LINK: ${{ secrets.CSC_LINK }}",
    "CSC_KEY_PASSWORD: ${{ secrets.CSC_KEY_PASSWORD }}",
    "APPLE_TEAM_ID: T44CNHC4UH",
    "Developer ID Application: PwrDrvr LLC (${APPLE_TEAM_ID})",
    "--options runtime",
    "--timestamp",
    "codesign --verify --all-architectures --strict",
    "TeamIdentifier=${APPLE_TEAM_ID}",
):
    require(macos_sign, fragment, "macos-sign")

# Every shipped mach-O has to go through codesign, not just the CLI entrypoint.
require(
    macos_sign,
    "for binary in " + " ".join(UNIX_BINARIES) + "; do",
    "macos-sign",
)

for fragment in (
    "startsWith(github.ref, 'refs/tags/pwragent-v')",
    "contains(github.event.pull_request.labels.*.name, 'ci:release-signing')",
    "environment: windows-signing",
    "scripts/pwragent-release/sign-windows-binaries.ps1",
    "-SigningToolsRoot signing-tools",
    "WIN_AZURE_SIGN_PUBLISHER_NAME: ${{ vars.WIN_AZURE_SIGN_PUBLISHER_NAME }}",
    "WIN_AZURE_SIGN_ENDPOINT: ${{ vars.WIN_AZURE_SIGN_ENDPOINT }}",
    "WIN_AZURE_SIGN_ACCOUNT: ${{ vars.WIN_AZURE_SIGN_ACCOUNT }}",
    "WIN_AZURE_SIGN_PROFILE: ${{ vars.WIN_AZURE_SIGN_PROFILE }}",
    "AZURE_TENANT_ID: ${{ secrets.AZURE_TENANT_ID }}",
    "AZURE_CLIENT_ID: ${{ secrets.AZURE_CLIENT_ID }}",
    "AZURE_CLIENT_SECRET: ${{ secrets.AZURE_CLIENT_SECRET }}",
):
    require(windows_sign, fragment, "windows-sign")

for binary in WINDOWS_BINARIES:
    require(windows_sign, f"stage/windows-x86_64/{binary}.exe", "windows-sign")

if "Install-Module" in windows_sign or "Save-Module" in windows_sign:
    fail("windows-sign must not acquire PowerShell modules inside the protected job")

for dependency in ("macos-sign", "windows-sign"):
    require(release_candidate, f"- {dependency}", "release-candidate")
require(
    release_candidate,
    "contains(github.event.pull_request.labels.*.name, 'ci:release-signing')",
    "release-candidate",
)
require(
    release_candidate,
    f'test "${{#assets[@]}}" -eq {EXPECTED_RELEASE_ASSETS}',
    "release-candidate",
)
require(release_candidate, "name: signed-release-candidate", "release-candidate")
require(release_candidate, "contents: read", "release-candidate")
for fragment in (
    "attestations: write",
    "id-token: write",
    "actions/attest@59d89421af93a897026c735860bf21b6eb4f7b26 # v4.1.0",
    "create-storage-record: false",
    "scripts/pwragent-release/generate-update-manifest.py manifest",
    "pwragent-codex-update-v1.json",
    "pwragent-codex-update-v1.json.sigstore.json",
    "scripts/pwragent-release/generate-update-manifest.py marker",
    "pwragent-codex-publication-complete-v1.json",
):
    require(release_candidate, fragment, "signed update metadata")
require(release, "- release-candidate", "release")
require(release, "name: signed-release-candidate", "release")
require(release, "contents: write", "release")
for fragment in (
    'marker="dist/pwragent-codex-publication-complete-v1.json"',
    '! -name "$(basename "$marker")"',
    'gh release create "$RELEASE_TAG" "${assets[@]}"',
    "--draft",
    'gh release upload "$RELEASE_TAG" "$marker"',
    'gh release edit "$RELEASE_TAG" --repo "$GITHUB_REPOSITORY" --draft=false',
):
    require(release, fragment, "publication-complete marker ordering")

for fragment in (
    "WIN_AZURE_SIGN_PUBLISHER_NAME = $env:WIN_AZURE_SIGN_PUBLISHER_NAME",
    "AZURE_CLIENT_SECRET = $env:AZURE_CLIENT_SECRET",
    "Invoke-TrustedSigning @signingParameters",
    "Get-AuthenticodeSignature -LiteralPath $resolvedBinary",
    "SignatureStatus]::Valid",
    "TimeStamperCertificate",
    "CN=$expectedPublisher",
    "verify-trusted-signing-tools.ps1",
    "$verifiedSigningTools.ModuleManifest",
    "$verifiedSigningTools.LocalAppDataRoot",
):
    require(windows_signer, fragment, "Windows signing script")

# Each binary is verified after signing, not just the last one in the list.
require(
    windows_signer,
    "foreach ($resolvedBinary in $resolvedBinaries) {",
    "Windows signing script",
)

for fragment in (
    '"modules/TrustedSigning/$trustedSigningVersion/TrustedSigning.psd1"',
    "Get-FileHash -Algorithm SHA256",
    "Get-ChildItem -LiteralPath $resolvedSigningToolsRoot -File -Recurse -Force",
    "TrustedSigning input files are not covered by SHA256SUMS",
    "$uncoveredFiles -join",
    "Microsoft.Trusted.Signing.Client.1.0.95",
):
    require(windows_signing_verifier, fragment, "TrustedSigning verifier")

for fragment in (
    'trustedSigningVersion = "0.5.8"',
    "Save-Module",
    "-RequiredVersion $trustedSigningVersion",
    "Test-FileCatalog",
    "-Detailed",
    "$moduleFiles.FullName",
    'Name -ne "PSGetModuleInfo.xml"',
    "duplicate catalog leaf names",
    "SignatureStatus]::Valid",
    'catalogSigner -ne "Microsoft Corporation"',
    "Get-EveryDependency",
    "-File -Recurse -Force",
    "$filesToChecksum",
    'Join-Path $resolvedOutputRoot "SHA256SUMS"',
):
    require(windows_signing_preparer, fragment, "TrustedSigning preparer")

if "Install-PackageProvider" in windows_signing_preparer:
    fail("TrustedSigning preparer must not bootstrap the legacy NuGet provider")

for fragment in (
    "trusted-signing-preparation:",
    "runs-on: windows-2022",
    "timeout-minutes: 10",
    "scripts/pwragent-release/prepare-trusted-signing.ps1",
    "-OutputRoot $env:RUNNER_TEMP/signing-tools",
    "Verify signing client after archive round-trip",
    "tar.exe -czf",
    "tar.exe -xzf",
    "scripts/pwragent-release/verify-trusted-signing-tools.ps1",
    "id-token: none",
):
    require(check_workflow, fragment, "release signing check workflow")

if "environment:" in check_workflow or "secrets." in check_workflow:
    fail("release signing check workflow must not enter an environment or read secrets")

unsigned_workflow_code = "\n".join(
    line for line in unsigned_workflow.splitlines() if not line.lstrip().startswith("#")
)
if "environment:" in unsigned_workflow_code or "secrets." in unsigned_workflow_code:
    fail("the unsigned macOS workflow must not enter an environment or read secrets")
for fragment in ("softprops/action-gh-release", "gh release", "contents: write"):
    if fragment in unsigned_workflow_code:
        fail(f"the unsigned macOS workflow must not publish ({fragment!r})")
for fragment in (
    "'ci:macos-unsigned'",
    "name: unsigned-macos-aarch64",
    "signed=no",
    "id-token: none",
):
    require(unsigned_workflow, fragment, "unsigned macOS workflow")

for fragment in (
    "Developer ID Application: PwrDrvr LLC (T44CNHC4UH)",
    "`CSC_LINK`",
    "`CSC_KEY_PASSWORD`",
    "`WIN_AZURE_SIGN_ACCOUNT` | `pwrdrvrsigning`",
    "`WIN_AZURE_SIGN_ENDPOINT` | `https://eus.codesigning.azure.net/`",
    "`WIN_AZURE_SIGN_PUBLISHER_NAME` | `PwrDrvr LLC`",
    "`WIN_AZURE_SIGN_PROFILE` | `pwrdrvr-public-trust`",
    "`AZURE_TENANT_ID`",
    "`AZURE_CLIENT_ID`",
    "`AZURE_CLIENT_SECRET`",
    "`ci:release-signing`",
    "`signed-release-candidate`",
    "`pwragent-v`",
    "`pwragent-codex-update-v1.json`",
    "`pwragent-codex-update-v1.json.sigstore.json`",
    "`pwragent-codex-publication-complete-v1.json`",
):
    require(runbook, fragment, "release signing runbook")

print("release signing contract: ok")
