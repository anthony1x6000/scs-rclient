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
flatpak run --command=rclone online.anthonyis.scs-rclient lsf :webdav: --webdav-url "https://a.ocv.me/pub/demo/docs/" --webdav-vendor other

echo "=== Verifying Clean Uninstallation ==="
flatpak uninstall -y --user --noninteractive online.anthonyis.scs-rclient

echo "✓ Standalone bundle installation and removal verified!"
