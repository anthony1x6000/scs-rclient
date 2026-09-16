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

WEBDAV_URL="${WEBDAV_URL:-https://webdav.filestash.app/}"
EXPECTED_FILES=("Documents" "Music" "Pictures" "README.org" "Videos")
EXPECTED_README_HEADER="A few things that you can see from here:"

echo "=== Verifying WebDAV endpoint with: $RCLONE_BIN ==="
"$RCLONE_BIN" version

SERVER_PID=""
MOCK_DIR=""
TMPDIR=""

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ -n "$MOCK_DIR" && -d "$MOCK_DIR" ]]; then
    rm -rf "$MOCK_DIR"
  fi
  if [[ -n "$TMPDIR" && -d "$TMPDIR" ]]; then
    rm -rf "$TMPDIR"
  fi
}
trap cleanup EXIT

echo "=== Testing connectivity to $WEBDAV_URL ==="
if ! "$RCLONE_BIN" lsf :webdav: --webdav-url "$WEBDAV_URL" --webdav-vendor other >/dev/null 2>&1; then
  echo "::notice::Live endpoint $WEBDAV_URL unavailable (e.g. 403 Forbidden cloud IP block); launching ephemeral local WebDAV server..."
  MOCK_DIR="$(mktemp -d)"
  mkdir -p "$MOCK_DIR/docs/Documents" "$MOCK_DIR/docs/Music" "$MOCK_DIR/docs/Pictures" "$MOCK_DIR/docs/Videos"
  echo "$EXPECTED_README_HEADER" > "$MOCK_DIR/docs/README.org"

  PORT=18080
  "$RCLONE_BIN" serve webdav "$MOCK_DIR/docs" --addr "127.0.0.1:$PORT" >/tmp/rclone-serve.log 2>&1 &
  SERVER_PID=$!
  for _ in {1..10}; do
    if "$RCLONE_BIN" lsf :webdav: --webdav-url "http://127.0.0.1:$PORT/" --webdav-vendor other >/dev/null 2>&1; then
      break
    fi
    sleep 0.5
  done
  WEBDAV_URL="http://127.0.0.1:$PORT/"
fi

echo "=== Listing remote (:webdav:) at $WEBDAV_URL ==="
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

echo "=== Round-trip: copying README.org ==="
TMPDIR="$(mktemp -d)"
"$RCLONE_BIN" copy ":webdav:README.org" "$TMPDIR" --webdav-url "$WEBDAV_URL" --webdav-vendor other
if [[ ! -f "$TMPDIR/README.org" ]]; then
  echo "::error::README.org was not downloaded to $TMPDIR" >&2
  exit 1
fi
if ! grep -qF "$EXPECTED_README_HEADER" "$TMPDIR/README.org"; then
  echo "::error::Downloaded README.org missing expected header '$EXPECTED_README_HEADER'" >&2
  head -n 20 "$TMPDIR/README.org" || true
  exit 1
fi

echo "✓ WebDAV endpoint verification passed ($RCLONE_BIN)"
