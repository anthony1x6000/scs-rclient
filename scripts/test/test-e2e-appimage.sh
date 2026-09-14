#!/usr/bin/env bash
# E2E test for the .AppImage release binary:
# extract -> sidecar WebDAV test -> launch smoke (extract-and-run under xvfb)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPIMAGE_FILE="${APPIMAGE_FILE:-dist-linux/scs-rclient-linux.AppImage}"

if [[ ! -f "$APPIMAGE_FILE" ]]; then
  echo "::error::AppImage file not found: $APPIMAGE_FILE" >&2
  exit 1
fi

chmod +x "$APPIMAGE_FILE"

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR" "squashfs-root"' EXIT
cp "$APPIMAGE_FILE" "$WORKDIR/scs-rclient.AppImage"
pushd "$WORKDIR" >/dev/null

echo "=== Extracting AppImage ==="
./scs-rclient.AppImage --appimage-extract >/dev/null
if [[ ! -d squashfs-root ]]; then
  echo "::error::AppImage extraction failed (no squashfs-root)" >&2
  exit 1
fi

echo "=== Locating sidecar in squashfs-root ==="
SIDECAR="$(find squashfs-root -name 'rclone-sidecar*' -type f 2>/dev/null | head -n 1 || true)"
if [[ -z "$SIDECAR" ]]; then
  echo "::error::Sidecar not found inside AppImage!" >&2
  exit 1
fi
chmod +x "$SIDECAR"
echo "Found sidecar: $SIDECAR"
ls -l "$SIDECAR"

echo "=== Running sidecar WebDAV verification ==="
bash "$SCRIPT_DIR/verify-webdav-endpoint.sh" "$PWD/$SIDECAR"
popd >/dev/null

echo "=== GUI launch smoke test (xvfb-run + extract-and-run) ==="
if timeout 15 xvfb-run -a "$APPIMAGE_FILE" --appimage-extract-and-run >/tmp/scs-appimage-launch.log 2>&1; then
  echo "App exited on its own within the timeout window."
else
  code=$?
  if [[ "$code" -eq 124 ]]; then
    echo "App stayed running for 15s (healthy start, killed by timeout)."
  else
    echo "::error::App launch failed with code $code; tail:" >&2
    tail -n 50 /tmp/scs-appimage-launch.log || true
    exit 1
  fi
fi

echo "✓ AppImage E2E passed"
