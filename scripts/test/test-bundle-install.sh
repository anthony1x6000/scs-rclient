#!/usr/bin/env bash
set -euo pipefail

# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
unset RCLONE_VERSION

BUNDLE_FILE="${1:-scs-rclient-linux.flatpak}"

echo "=== Setting up Flathub remote ==="
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo

echo "=== Installing Flatpak Bundle ($BUNDLE_FILE) ==="
flatpak install -y --user --noninteractive "$BUNDLE_FILE"

echo "=== Verifying Installation Metadata ==="
flatpak info online.anthonyis.scs-rclient

echo "=== Verifying Application Permissions ==="
flatpak info --show-permissions online.anthonyis.scs-rclient

echo "=== Verifying sandboxed rclone sidecar inside Flatpak ==="
WEBDAV_URL="https://a.ocv.me/pub/demo/docs/"
if ! flatpak run --command=rclone online.anthonyis.scs-rclient lsf :webdav: --webdav-url "$WEBDAV_URL" --webdav-vendor other 2>/dev/null; then
  echo "::notice::Live endpoint $WEBDAV_URL unavailable (e.g. 403 Forbidden cloud IP block); starting sandboxed WebDAV server..."
  MOCK_DIR="$HOME/Documents/scs-rclient/mock-docs"
  mkdir -p "$MOCK_DIR"
  touch "$MOCK_DIR/5m-iceblaze.ans" "$MOCK_DIR/LDA-MIST.ANS"
  echo "this folder contains stolen content;" > "$MOCK_DIR/README.md"
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
