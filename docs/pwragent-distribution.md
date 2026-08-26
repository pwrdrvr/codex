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

Every mach-O in the archive is signed with `--options runtime --timestamp`, then
verified: `codesign --verify --all-architectures --strict`, plus an exact-match
check on both `Authority=` and `TeamIdentifier=`.

These binaries are **signed but not notarized**. They are intended to be nested
inside a PwrDrvr application bundle that is itself notarized, which works
because the nested code carries the same Team ID and hardened runtime. Shipping
one of these binaries standalone to end users would need a notarization step
that does not exist here yet.

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
so it can populate this repo without being copied here.

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

Untagged runs (`workflow_dispatch`, or a labeled PR) derive
`<workspace version>-pwragent.dev.<run number>`. Upstream leaves the workspace
version at `0.0.0` on `main` and only bumps it on release branches, so dev
builds usually read `0.0.0-pwragent.dev.N`.

## Branch layout

`pwragent` is this fork's default branch and the integration branch PwrDrvr
builds from. Feature work lands on `agent/*` branches and merges into
`pwragent`. Rebasing `pwragent` onto a newer upstream `main` is a manual
operation; nothing here does it automatically.
