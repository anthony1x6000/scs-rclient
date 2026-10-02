#!/usr/bin/env bash
# E2E test for the standalone Linux release binary.
set -euo pipefail

# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
unset RCLONE_VERSION

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_BIN="${APP_BIN:-src-tauri/target/release/scs-rclient}"

if [[ ! -f "$APP_BIN" ]]; then
  echo "::error::App binary not found: $APP_BIN" >&2
  exit 1
fi

chmod +x "$APP_BIN"

echo "=== GUI launch smoke test (xvfb-run) ==="
if timeout 15 xvfb-run -a "$APP_BIN" >/tmp/scs-binary-launch.log 2>&1; then
  echo "App exited on its own within the timeout window."
else
  code=$?
  if [[ "$code" -eq 124 ]]; then
    echo "App stayed running for 15s (healthy start, killed by timeout)."
  else
    echo "::error::App launch failed with code $code; tail:" >&2
    tail -n 50 /tmp/scs-binary-launch.log || true
    exit 1
  fi
fi

echo "✓ Standalone binary E2E passed"
