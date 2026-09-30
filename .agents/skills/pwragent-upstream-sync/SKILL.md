---
name: pwragent-upstream-sync
description: Replay this fork's downstream commits onto a requested upstream Codex tag, reconcile upstream API and behavior changes, validate the result, and update the pwragent branch with an explicit force-push lease. Use for PwrAgent upstream rebases, not release publication.
---

# PwrAgent upstream sync

Work from the requested upstream tag. Keep the downstream changes as replayed commits, with any additional compatibility fixes reviewable. Repository AGENTS.md governs formatting and tests. Syncing does not itself authorize a release; use the sibling [pwragent-release skill](../pwragent-release/SKILL.md) when publication is requested.

## Establish the replay range

- Inspect tracked/untracked changes, worktrees, remotes, and local/remote branch tips. This fork normally uses `origin` for `openai/codex` and `pwrdrvr` for `pwrdrvr/codex`; verify rather than assuming aliases.
- Fetch the requested upstream tag and downstream branch. Record the full remote `pwragent` SHA now as the eventual lease. Resolve the tag to a commit and preserve the old downstream tip in a uniquely named local backup ref.
- Determine the old upstream base from ancestry and the actual downstream history. `git describe` can report a much older reachable tag; do not use it alone to select the replay range. Review `git log <candidate-base>..<old-tip>` and the cumulative diff. A merge base with the new tag is a candidate, not proof of the intended range, especially if the target is older or divergent.
- Check merge commits for resolution-only changes (`git show --remerge-diff <merge>`). Flattening a merge can discard those changes. Preserve or explicitly port them when needed.
- Create a temporary branch and separate worktree from the old downstream tip. Replay with `git rebase --onto <new-tag> <old-base>` there. Preserve user files such as the untracked `.local/` directory in the original checkout.

## Reconcile behavior, not just conflict markers

Read both upstream changes and downstream intent. Do not resolve whole files with `--ours`/`--theirs` merely to finish the rebase. Regenerate derived schemas after merging their source types.

Inspect affected callers even when Git merges them cleanly. Previous syncs exposed these useful checks; apply those relevant to the current diff:

- **Dispatch timing:** a formerly async API may now synchronously capture metadata and spawn work before returning its future. Wrapping its invocation inside a new async block can defer dispatch and break concurrency, Guardian reviews, and recording lifetimes. Preserve eager invocation while wrapping the returned future for cleanup.
- **Step versus turn state:** preserve upstream step-specific model, permissions, image-detail, and truncation settings when integrating reducer hooks. Preserve host-reported Code Mode timing rather than replacing it with local elapsed time.
- **Thread resume:** reconcile daemon recovery and retry flows with Token Miser activation, reducer refresh, and dynamic-tool replacement. Keep ordinary upstream resume/error semantics; any pre-first-turn exception should be limited to the explicit downstream overrides it serves. Retain omission/null/empty-array distinctions.
- **Protocol evolution:** inspect constructor arguments, enum payloads (for example image references), IPC fields, and release smoke tests that compare exact responses. Update smoke expectations to validate new variable fields without weakening unrelated assertions.
- **Downstream polling:** if empty `write_stdin` polls retain a longer minimum than upstream, tests waiting for subsequent approval events need sufficient time. Preserve the actual approval assertions.
- **Docs and generated outputs:** upstream may have split or moved documentation. Port only the downstream additions into an appropriate location instead of restoring an obsolete monolithic file.

## Validate the assembled branch

- Review `git range-diff <old-base>..<old-tip> <new-tag>..<new-tip>` and the new-tag-to-tip diff for lost downstream work and accidental reversions. Verify the requested tag is an ancestor.
- Follow AGENTS.md for affected-crate tests, integration coverage, schema generation, scoped lint fixes, and final formatting. Obtain permission for the full workspace suite when required. Do not rerun tests after final `just fix`/`just fmt` solely to repeat verification.
- **Keep required lockfile updates.** Release tags can set workspace package versions while carrying a lockfile with `0.0.0` entries. Cargo's workspace-version updates are required for `--locked` CI, even when registry/git dependency pins are unchanged. Review and commit them; verify with `cargo metadata --locked --filter-platform <host-triple> --format-version 1`. Use `--offline` only if dependencies are already cached. Run `just bazel-lock-update` for Cargo manifest/lock changes.
- Regenerate stable/experimental app-server fixtures and config schema when affected. The schema command also regenerates Python SDK types; use a Python version supported by the checked-in generator dependencies. Do not change dependency pins just to accommodate a newer local interpreter.
- **Rebuild test helpers for this source revision.** A shared Cargo target directory may contain stale `codex-code-mode-host`, `codex-exec`, `exec-server`, `codex-execve-wrapper`, `test_stdio_server` (from `codex-rmcp-client`), or CLI binaries that scoped tests do not rebuild. Missing IPC fields can indicate an old helper, not a protocol regression.
- If V8's default archive URL fails, follow [the V8 artifact guidance](../../../third_party/v8/README.md) and [the release setup action](../../../.github/actions/setup-rusty-v8/action.yml). Use exact-version archive/binding pairs and verify manifests and checksums. Never mix versions or bypass verification.
- Run the relevant [release smoke scripts](../../../scripts/pwragent-release) against rebuilt binaries when Code Mode or IPC changes. Inspect failed tests individually. Accept intended snapshot changes only; exclude machine-local skill discovery and errors from stale helpers. Rerun failures when a concrete fix or environment correction warrants it. Disclose unresolved failures rather than calling validation green.
- For long builds, use a supported job monitor if available, with durable logs/status files and a clear completion handoff. A tool-session ID is not portable between threads.

## Update the branch

Commit compatibility fixes and generated artifacts before pushing. Review the final diff and clean up only artifacts created by this task. Existing authorization to update `pwragent` covers the requested push; a read-only investigation does not.

Use the full remote SHA captured before replay as an explicit lease:

```sh
git push --force-with-lease=refs/heads/pwragent:<captured-remote-sha> <fork-remote> HEAD:refs/heads/pwragent
```

If the lease rejects, inspect the new remote work. Do not refresh the lease and overwrite it blindly. Update the original local branch only after checking it has no intervening tracked edits (for example with `git reset --keep <new-tip>` while on `pwragent`). Verify the remote tip and tag ancestry. Keep the backup ref and report the new SHA, validation limits, and backup name. Do not tag or publish a release as part of sync unless requested.
