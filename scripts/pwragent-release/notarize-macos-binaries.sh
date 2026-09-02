#!/usr/bin/env bash

# Submit the exact signed standalone binaries that the PwrAgent tarball ships.
# The ZIP is only an Apple-supported notarization transport; it is not published.

set -euo pipefail

stage_dir="${1:?usage: notarize-macos-binaries.sh STAGE_DIR DIGEST_PATH}"
digest_path="${2:?usage: notarize-macos-binaries.sh STAGE_DIR DIGEST_PATH}"

: "${APPLE_NOTARY_KEY:?APPLE_NOTARY_KEY is required}"
: "${APPLE_NOTARY_KEY_ID:?APPLE_NOTARY_KEY_ID is required}"
: "${APPLE_NOTARY_ISSUER_ID:?APPLE_NOTARY_ISSUER_ID is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"

stage_dir="$(cd "$stage_dir" && pwd -P)"
digest_dir="$(cd "$(dirname "$digest_path")" && pwd -P)"
digest_path="$digest_dir/$(basename "$digest_path")"
work_dir="$(mktemp -d "$RUNNER_TEMP/pwragent-notary.XXXXXX")"
api_key="$work_dir/AuthKey_${APPLE_NOTARY_KEY_ID}.p8"
submission="$work_dir/pwragent-codex-notarization.zip"
result="$work_dir/notary-result.json"

cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

umask 077
notary_key_base64="${APPLE_NOTARY_KEY#data:application/octet-stream;base64,}"
printf '%s' "$notary_key_base64" | base64 -D > "$api_key"
test -s "$api_key"

binaries=(codex codex-app-server codex-code-mode-host)
: > "$digest_path"
for binary in "${binaries[@]}"; do
  test -f "$stage_dir/$binary"
  digest="$(shasum -a 256 "$stage_dir/$binary" | awk '{print $1}')"
  printf '%s  %s\n' "$digest" "$binary" >> "$digest_path"
done

# notarytool accepts ZIP, DMG, and signed flat PKG submissions. The signed
# bytes remain in stage_dir; the existing tar.gz distribution format is built
# from that directory only after Apple returns an Accepted result.
/usr/bin/ditto -c -k --keepParent "$stage_dir" "$submission"

if ! xcrun notarytool submit "$submission" \
  --key "$api_key" \
  --key-id "$APPLE_NOTARY_KEY_ID" \
  --issuer "$APPLE_NOTARY_ISSUER_ID" \
  --wait \
  --timeout 45m \
  --output-format json > "$result"
then
  echo "::error::Apple notarytool submission failed." >&2
  exit 1
fi

if ! jq -e \
  '.status == "Accepted" and (.id | type == "string" and length > 0)' \
  "$result" >/dev/null
then
  status="$(jq -r '.status // "missing"' "$result")"
  submission_id="$(jq -r '.id // "missing"' "$result")"
  echo "::error::Apple notarization did not reach Accepted (submission ${submission_id}, status ${status})." >&2
  exit 1
fi

submission_id="$(jq -r '.id' "$result")"
echo "Apple notarization accepted submission ${submission_id}."

# Prove the files Apple accepted are still byte-for-byte the files awaiting
# packaging. The packaged-archive verification checks these digests again.
(
  cd "$stage_dir"
  shasum -a 256 --check "$digest_path"
)
