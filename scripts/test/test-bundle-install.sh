#!/usr/bin/env bash
set -euo pipefail

# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
unset RCLONE_VERSION

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUNDLE_FILE="${1:-scs-rclient-linux.flatpak}"

echo "=== Setting up Flathub remote ==="
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo

echo "=== Installing Flatpak Bundle ($BUNDLE_FILE) ==="
flatpak install -y --user --noninteractive "$BUNDLE_FILE"

echo "=== Verifying Installation Metadata ==="
flatpak info online.anthonyis.scs-rclient

echo "=== Verifying Application Permissions ==="
flatpak info --show-permissions online.anthonyis.scs-rclient

echo "=== Verifying the installed payload ships a resolvable sidecar ==="
# The app resolves <dir of its own exe>/rclone-sidecar. Inside the sandbox that is
# /app/bin/rclone-sidecar, which is <deployment>/files/bin/rclone-sidecar on the
# host. The manifest must install the sidecar under that exact name; a
# target-triple-suffixed install makes the packaged app report
# "No usable rclone binary found".
FLATPAK_LOCATION="$(flatpak info --user --show-location online.anthonyis.scs-rclient)"
echo "Flatpak deployment: $FLATPAK_LOCATION"
bash "$SCRIPT_DIR/verify-sidecar-layout.sh" "$FLATPAK_LOCATION/files/bin" "${TARGET_RCLONE_VERSION:-}"

echo "=== Verifying sandboxed rclone sidecar inside Flatpak ==="
WEBDAV_URL="https://webdav.filestash.app/"
if ! flatpak run --command=rclone online.anthonyis.scs-rclient lsf :webdav: --webdav-url "$WEBDAV_URL" --webdav-vendor other 2>/dev/null; then
  echo "::notice::Live endpoint $WEBDAV_URL unavailable (e.g. 403 Forbidden cloud IP block); starting sandboxed WebDAV server..."
  MOCK_DIR="$HOME/Documents/scs-rclient/mock-docs"
  mkdir -p "$MOCK_DIR/Documents" "$MOCK_DIR/Music" "$MOCK_DIR/Pictures" "$MOCK_DIR/Videos"
  echo "A few things that you can see from here:" > "$MOCK_DIR/README.org"
  PORT=18080
  flatpak run --command=rclone online.anthonyis.scs-rclient serve webdav "$MOCK_DIR" --addr "127.0.0.1:$PORT" >/tmp/flatpak-serve.log 2>&1 &
  SERVER_PID=$!
  for _ in {1..10}; do
    if flatpak run --command=rclone online.anthonyis.scs-rclient lsf :webdav: --webdav-url "http://127.0.0.1:$PORT/" --webdav-vendor other >/dev/null 2>&1; then
      break
    fi
    sleep 0.5
  done
  WEBDAV_URL="http://127.0.0.1:$PORT/"
  flatpak run --command=rclone online.anthonyis.scs-rclient lsf :webdav: --webdav-url "$WEBDAV_URL" --webdav-vendor other
  kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$MOCK_DIR"
fi

echo "=== Verifying Clean Uninstallation ==="
flatpak uninstall -y --user --noninteractive online.anthonyis.scs-rclient

echo "✓ Standalone bundle installation and removal verified!"
