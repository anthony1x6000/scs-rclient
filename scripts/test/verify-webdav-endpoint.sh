#!/usr/bin/env bash
# Shared helper: verify an rclone binary against the live demo WebDAV server.
# Usage: verify-webdav-endpoint.sh <path-to-rclone>
set -euo pipefail

# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
unset RCLONE_VERSION

RCLONE_BIN="${1:-}"
if [[ -z "$RCLONE_BIN" ]]; then
  echo "::error::Usage: $0 <path-to-rclone>" >&2
  exit 1
fi
if [[ ! -x "$RCLONE_BIN" ]]; then
  echo "::error::rclone binary not found or not executable: $RCLONE_BIN" >&2
  exit 1
fi

WEBDAV_URL="https://a.ocv.me/pub/demo/docs/"
EXPECTED_FILES=("5m-iceblaze.ans" "LDA-MIST.ANS" "README.md")
EXPECTED_README_HEADER="this folder contains stolen content;"

echo "=== Verifying WebDAV endpoint with: $RCLONE_BIN ==="
"$RCLONE_BIN" version

echo "=== Listing remote (:webdav:) ==="
LSF_OUTPUT="$("$RCLONE_BIN" lsf :webdav: --webdav-url "$WEBDAV_URL" --webdav-vendor other)"
echo "$LSF_OUTPUT"
echo "--- exit code: $? ---"

for expected in "${EXPECTED_FILES[@]}"; do
  if ! grep -qxF "$expected" <<<"$LSF_OUTPUT" && ! grep -qF "$expected" <<<"$LSF_OUTPUT"; then
    echo "::error::Expected file '$expected' not found in rclone lsf output!" >&2
    exit 1
  fi
  echo "Found expected file: $expected"
done

echo "=== Round-trip: copying README.md ==="
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
"$RCLONE_BIN" copy ":webdav:README.md" "$TMPDIR" --webdav-url "$WEBDAV_URL" --webdav-vendor other
if [[ ! -f "$TMPDIR/README.md" ]]; then
  echo "::error::README.md was not downloaded to $TMPDIR" >&2
  exit 1
fi
if ! grep -qF "$EXPECTED_README_HEADER" "$TMPDIR/README.md"; then
  echo "::error::Downloaded README.md missing expected header '$EXPECTED_README_HEADER'" >&2
  head -n 20 "$TMPDIR/README.md" || true
  exit 1
fi

echo "✓ WebDAV endpoint verification passed ($RCLONE_BIN)"
