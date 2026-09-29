---
name: pwragent-release
description: Prepare, tag, and verify publication of this fork's signed PwrAgent Codex distribution through its GitHub Actions release workflow. Use when asked to release the custom build, not to rebase upstream commits or publish an upstream Codex release.
---

# PwrAgent release

Use the repository's downstream pipeline. Read [the distribution contract](../../../docs/pwragent-distribution.md) and [pwragent-release.yml](../../../.github/workflows/pwragent-release.yml) for current versioning, signing, assets, and publication rules. The upstream `rust-release.yml` is not this fork's release path.

## Prepare the release

- Verify the fork remote, `pwragent` source commit, working tree, recent CI results, existing remote tags, and existing GitHub releases. Preserve unrelated work. Investigate relevant CI failures before tagging, including failures that happen before compilation.
- The established naming is `pwragent-v<major.minor.patch>-pwragent.<N>`. For example, upstream `rust-v0.155.0-alpha.9` becomes downstream version `0.155.0-pwragent.1` and tag `pwragent-v0.155.0-pwragent.1`. Drop the upstream prerelease suffix; the downstream separator is `-pwragent.`, not `.pwragent.`. Choose an unused sequence number for the baseline and honor an explicitly requested version when the workflow supports it.
- The source can remain based on an upstream alpha tag. The tag-derived `CODEX_BUILD_VERSION` stamps the downstream version into packaged executables; do not change the Rust workspace version merely to duplicate that stamp.
- Keep `scripts/pwragent-release/upstream-version.txt` aligned with the intended numeric upstream baseline. It controls untagged development builds; tagged builds derive their version from the tag.
- Check that Cargo's lockfile matches the workspace, using `cargo metadata --locked --filter-platform <host-triple> --format-version 1`. Do not discard required workspace-version updates as cosmetic churn. Follow AGENTS.md for dependency/lock changes, including `just bazel-lock-update`.
- Run `python3 scripts/pwragent-release/check-release-signing.py` and the manifest tests (`python3 -m unittest discover -s scripts/pwragent-release -p 'test_*.py'`). Reuse still-applicable build/test evidence from the sync; additional tests should address actual changes or unresolved concerns.
- When checking local Code Mode binaries, rebuild helpers from the release source and run the host and deferred-reducer smoke scripts. Read each script's arguments. For V8 download failures, use the exact-version, checksum-verified assets configured by [setup-rusty-v8](../../../.github/actions/setup-rusty-v8/action.yml), following [the V8 README](../../../third_party/v8/README.md).

## Trigger publication

A request to make a release authorizes its tag push and the workflow's publication. Preparing or inspecting a release alone does not. Finish reviewable preparation before seeking any missing publication authorization; do not repeatedly ask when it is already provided.

Commit required preparation changes, then create the new tag at the verified commit. Push the branch and new tag atomically when both need updating; otherwise push only the new tag. Use the verified fork remote. Do not force-update an existing release tag.

The `pwragent-v*` tag push starts the signed publishing workflow. `workflow_dispatch` currently produces unsigned artifacts and is not a substitute for publication. The workflow publishes a GitHub **prerelease** under the established downstream contract; report that status accurately.

Capture the release workflow run ID and URL for this tag and SHA. Monitor its build, signing, attestation, and publication jobs. If a supported background monitor is available, give it the exact run ID, polling procedure, failure evidence to return, and publication checks below. Otherwise use the available workflow-status tooling. Do not announce success while the build is merely queued or running.

## Handle failures without mutating published releases

- Inspect the first actionable failed step and logs. Do not weaken signing, checksums, smoke tests, or attestations to get a green run.
- For a clearly transient failure, retry failed jobs on the same tag at most twice; stop if the same deterministic failure persists or credentials/environment approval require user action.
- A source fix belongs in a new commit and unused release tag. Do not move the old tag or overwrite published assets. Explain any version increment needed for the fix.
- Let the workflow handle its narrowly defined cleanup of matching incomplete draft releases. Do not delete a published release or unrelated draft as a retry shortcut.

## Verify publication

Require a successful workflow and a non-draft release for the expected tag. Check the current contract for exact asset names. It currently requires five platform archives (macOS arm64/x64, Linux arm64/x64, Windows x64), plus:

- `SHA256SUMS`
- `pwragent-codex-update-v1.json`
- `pwragent-codex-update-v1.json.sigstore.json`
- `pwragent-codex-publication-complete-v1.json`

Download the manifest and completion marker into a temporary directory. Check `complete: true`, the expected version/tag/source SHA, and the marker's manifest SHA-256 against the downloaded bytes. Cross-check manifest artifact names, sizes, and platforms against the release assets. The marker is uploaded last; a release without it is not ready for managed installation.

The workflow performs signing and attestation. Do not claim independent signature verification unless it was actually performed; use the distribution contract's exact repository/workflow/ref identity requirements if independently verifying the Sigstore bundle.

Finish with the release URL, version, prerelease status, and verified publication outcome. If blocked, give the failing job and concrete next step, without presenting incomplete artifacts as a release.
