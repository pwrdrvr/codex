# PwrAgent Codex distribution

Signed downstream Codex binaries for PwrDrvr products. One build, one signing
pass, consumed by PwrAgent, PwrSnap, and PwrGit — so none of them has to sign
Codex itself.

Built by [`.github/workflows/pwragent-release.yml`](../.github/workflows/pwragent-release.yml).

## What ships

| Platform | Runner | Target | Asset |
| --- | --- | --- | --- |
| macOS arm64 | `macos-15` | `aarch64-apple-darwin` | `pwragent-codex-<version>-macos-aarch64.tar.gz` |
| macOS x64 | `macos-15-intel` | `x86_64-apple-darwin` | `pwragent-codex-<version>-macos-x86_64.tar.gz` |
| Linux arm64 | `ubuntu-24.04-arm` | `aarch64-unknown-linux-gnu` | `pwragent-codex-<version>-linux-aarch64.tar.gz` |
| Linux x64 | `ubuntu-24.04` | `x86_64-unknown-linux-gnu` | `pwragent-codex-<version>-linux-x86_64.tar.gz` |
| Windows x64 | `windows-2022` | `x86_64-pc-windows-msvc` | `pwragent-codex-<version>-windows-x86_64.zip` |

Each archive contains `LICENSE`, `NOTICE`, a `PWRAGENT-BUILD.txt` provenance
stamp (version, source repository, source commit, target), and the binaries:

- `codex` — the CLI.
- `codex-app-server` — the JSON-RPC surface PwrDrvr products drive.
- `codex-code-mode-host` — required for code mode; without it code mode fails
  closed.
- `codex-windows-sandbox-setup`, `codex-command-runner` — Windows sandbox
helpers, Windows only.

## Managed update contract

The first release carrying the managed Token Miser activation contract is
`pwragent-v0.149.0-pwragent.1` (`0.149.0-pwragent.1`). PwrAgent selects exactly
one of the five archive names in the table above using these stable pairs:

| `os` | `arch` | `platform` |
| --- | --- | --- |
| `darwin` | `arm64` | `macos-aarch64` |
| `darwin` | `x64` | `macos-x86_64` |
| `linux` | `arm64` | `linux-aarch64` |
| `linux` | `x64` | `linux-x86_64` |
| `win32` | `x64` | `windows-x86_64` |

Every published release has these metadata assets in addition to the five
archives:

- `SHA256SUMS` covers the five archives in GNU sha256sum format.
- `pwragent-codex-update-v1.json` is the deterministic selection manifest.
- `pwragent-codex-update-v1.json.sigstore.json` is the JSON-serialized Sigstore
  bundle produced by `actions/attest` for the five archives, `SHA256SUMS`, and
  the selection manifest.
- `pwragent-codex-publication-complete-v1.json` is uploaded last. A release is
  selectable only after this marker exists and its `complete` field is `true`.

The manifest has this exact top-level contract:

```json
{
  "schemaVersion": 1,
  "product": "pwragent-codex",
  "version": "<SemVer>",
  "releaseTag": "pwragent-v<same SemVer>",
  "source": {"repository": "pwrdrvr/codex", "commit": "<40 hex SHA>"},
  "capabilities": {
    "codeModeOutputReducer": {"protocolVersion": 1, "intentContextVersion": 1},
    "pwrdrvrTokenMiser": {"version": 1, "identity": "pwrdrvr.pwragent.token-miser"}
  },
  "artifacts": [{
    "file": "<exact archive name>",
    "platform": "<stable platform>",
    "os": "<stable os>",
    "arch": "<stable arch>",
    "target": "<Rust target>",
    "archiveType": "tar.gz|zip",
    "sha256": "<64 lowercase hex>",
    "size": 123
  }]
}
```

The completion marker binds `version`, `releaseTag`, and `sourceCommit` to a
`manifest` object containing the literal manifest filename, its SHA-256, the
literal Sigstore bundle filename, and `signatureFormat:
"sigstore-bundle-v0.3"`. PwrAgent must verify both the manifest and selected
archive against that bundle, require repository `pwrdrvr/codex`, and require
signer workflow `pwrdrvr/codex/.github/workflows/pwragent-release.yml` before
installing. The bundle uses GitHub Actions OIDC and the public Sigstore service;
it is independently verifiable with `gh attestation verify --bundle` and does
not add another long-lived release secret.

The certificate OIDC issuer must be exactly
`https://token.actions.githubusercontent.com`, and its certificate identity must
be exactly
`https://github.com/pwrdrvr/codex/.github/workflows/pwragent-release.yml@refs/tags/<releaseTag>`.
Verification must require the public Rekor transparency-log entry, GitHub event
`push`, and ref `refs/tags/<releaseTag>`; the attested source commit must equal
both `source.commit` in the manifest and `sourceCommit` in the completion
marker.

## Deliberate differences from upstream `rust-release.yml`

Upstream's release pipeline cannot run on this fork as-is, so this is a separate
workflow rather than a reused one:

| | Upstream | Downstream |
| --- | --- | --- |
| Runners | self-hosted groups (`codex-runners`, `macos-15-xlarge`) | GitHub-hosted only |
| Signing | OpenAI, `codesigning` environment, Azure Key Vault | PwrDrvr, `apple-signing` / `windows-signing` |
| Linux libc | MUSL, plus a bundled `bwrap` | glibc, no bundled `bwrap` |
| macOS layout | per-arch, DMG, dSYM symbol archives | per-arch tarballs, no DMG, no symbols |
| Windows arches | x64 and arm64 | x64 only |

The MUSL and `bwrap` omissions are the ones most likely to matter later.
Upstream builds `bwrap` first and embeds its digest into `codex` so the bundled
sandbox helper can be verified at runtime; this pipeline does not, so Linux
sandboxing falls back to whatever `codex` does without a bundled `bwrap`.
Revisit if a PwrDrvr product ships Codex on Linux to end users.

## Signing

### macOS — Developer ID, `apple-signing` environment

Identity: `Developer ID Application: PwrDrvr LLC (T44CNHC4UH)`.

| Secret | Contents |
| --- | --- |
| `CSC_LINK` | Base64-encoded Developer ID Application `.p12`, optionally with the `data:application/x-pkcs12;base64,` prefix |
| `CSC_KEY_PASSWORD` | Export password for that `.p12` |
| `APPLE_NOTARY_KEY` | Base64-encoded App Store Connect Team API private key (`.p8`) |
| `APPLE_NOTARY_KEY_ID` | App Store Connect Team API Key ID |
| `APPLE_NOTARY_ISSUER_ID` | App Store Connect Team API Issuer ID |

Every Mach-O in the archive is signed with `--options runtime --timestamp`, then
verified with `codesign --verify --all-architectures --strict`, plus an
exact-match check on both `Authority=` and `TeamIdentifier=`. The protected job
then puts those exact signed bytes in a temporary ZIP, submits it with
`xcrun notarytool submit --wait`, and requires Apple's final status to be
`Accepted`. The ZIP exists only because Apple accepts ZIP, DMG, and signed flat
PKG submission containers; the published files remain the existing per-arch
`.tar.gz` assets.

The App Store Connect Team API key must belong to team `T44CNHC4UH`, be enabled
for notarization, and be stored with its Key ID and Issuer ID as secrets on the
protected `apple-signing` GitHub Environment. The private key is decoded into an
ephemeral mode-0600 file and deleted before the job exits. GitHub Environment
deployment protection rules should restrict access to trusted release refs and
reviewers.

Notarization does not modify a standalone Mach-O. Before submission the workflow
records each binary's SHA-256, and it checks those digests both after Apple
accepts the submission and after extracting the release tarball. It separately
checks four properties:

- `codesign` verifies Developer ID signature validity and hardened-runtime
  signing.
- `notarytool` verifies that Apple accepted the submitted bytes; any other
  status fails closed before packaging.
- `spctl` verifies Gatekeeper assesses each packaged binary as
  `Notarized Developer ID`, with bounded retries for ticket propagation.
- Stapling is unavailable for these artifacts: Apple's `stapler` supports UDIF
  disk images, code-signed executable bundles, and signed flat installer
  packages. A raw standalone Mach-O and ZIP/tar archives cannot be stapled, so
  Gatekeeper retrieves the notarization ticket online.

The release-policy check is offline and only enforces this workflow contract;
ordinary CI never submits to Apple's live notarization service. A labeled
`ci:release-signing` PR or `pwragent-v*` tag exercises the protected live path.

### Windows — Azure Trusted Signing, `windows-signing` environment

| Variable | Value |
| --- | --- |
| `WIN_AZURE_SIGN_ACCOUNT` | `pwrdrvrsigning` |
| `WIN_AZURE_SIGN_ENDPOINT` | `https://eus.codesigning.azure.net/` |
| `WIN_AZURE_SIGN_PUBLISHER_NAME` | `PwrDrvr LLC` |
| `WIN_AZURE_SIGN_PROFILE` | `pwrdrvr-public-trust` |

| Secret | Contents |
| --- | --- |
| `AZURE_TENANT_ID` | Entra tenant for the signing service principal |
| `AZURE_CLIENT_ID` | Service principal application ID |
| `AZURE_CLIENT_SECRET` | Service principal secret |

The service principal needs the **Trusted Signing Certificate Profile Signer**
role on the signing account.

All five `.exe` files are signed in one `Invoke-TrustedSigning` call, then each
is verified individually for a `Valid` signature, a `CN=PwrDrvr LLC` signer, and
the presence of an RFC 3161 timestamp.

### Loading the Apple secrets

`pwrdrvr/grok-build` carries `scripts/release/upload-csc-link-from-1password.sh`,
which reads the Developer ID `.p12` out of 1Password and pushes it to a repo's
`apple-signing` environment. It takes the repository from `GITHUB_REPOSITORY`,
so it can populate this repo without being copied here. That helper only loads
`CSC_LINK` and `CSC_KEY_PASSWORD`; it does not configure notarization.

For notarization, create an App Store Connect Team API key for PwrDrvr's
provider, retain the downloaded `.p8`, and configure the three
`APPLE_NOTARY_*` secrets listed above in the repository's `apple-signing`
environment. Protect that environment with trusted-branch/tag and required-
reviewer rules. A release-signing run deliberately fails before packaging if
any credential is absent, invalid, or unable to produce an `Accepted` result.

## Why the pipeline is split into prepare and sign jobs

The jobs that build and stage (`build`, `windows-prepare`) enter no environment
and read no secrets. They hand off a tarball plus its SHA-256, and the signing
jobs verify that digest before touching a credential. The
`check-release-signing.py` contract enforces this separation, so a future edit
that starts reading a secret from a build job fails CI rather than quietly
widening the blast radius of a compromised build step.

The Windows TrustedSigning client is downloaded, catalog-verified, and
checksummed in the unprivileged `windows-prepare` job and shipped to the signing
job as pinned bytes. The signing job is forbidden from calling `Save-Module` or
`Install-Module`, so it cannot pull new code while holding credentials.

## Running it

### On a pull request

The signed path is gated behind the **`ci:release-signing`** label. Add the
label to run the whole pipeline including both signing jobs; the run produces a
`signed-release-candidate` artifact but publishes nothing.

Without the label, `pwragent-release.yml` does not build at all — only
`pwragent-release-check.yml` runs, which validates the contract and the pinned
TrustedSigning client without secrets.

Release builds inject the resolved downstream version into all three staged
binaries. Development artifacts use the reviewed upstream compatibility baseline recorded in
`scripts/pwragent-release/upstream-version.txt` plus a run-scoped
`-pwragent.dev.N` suffix; tagged builds use the tag version. The workflow runs
each staged binary with `--version` before packaging, so `PWRAGENT-BUILD.txt`
and the executable identity cannot silently diverge.

### Manually (`workflow_dispatch`)

A manual run builds every platform but enters no signing environment. It emits
the two Linux tarballs plus `unsigned-macos-aarch64`, `unsigned-macos-x86_64`,
and `unsigned-windows-x86_64` artifacts. Those are for smoke-testing a build;
they carry no signature and must never be shipped.

Note that a labeled PR run enters the protected environments from
`refs/pull/<PR number>/merge`. If either environment gets a branch protection
rule, that ref has to be allowed or the signing jobs will hang waiting for a
reviewer.

### Publishing

Push a tag:

```bash
git tag pwragent-v0.0.0-pwragent.1
git push fork pwragent-v0.0.0-pwragent.1
```

The tag suffix after `pwragent-v` becomes the version verbatim and must be
SemVer. Releases are immutable: the publish step fails if the tag already has a
release rather than overwriting assets.

Untagged runs (`workflow_dispatch`, or a labeled PR) derive the reviewed
baseline in `scripts/pwragent-release/upstream-version.txt` plus
`-pwragent.dev.<run number>`.

## Branch layout

`pwragent` is this fork's default branch and the integration branch PwrDrvr
builds from. Feature work lands on `agent/*` branches and merges into
`pwragent`. Rebasing `pwragent` onto a newer upstream `main` is a manual
operation; nothing here does it automatically.
