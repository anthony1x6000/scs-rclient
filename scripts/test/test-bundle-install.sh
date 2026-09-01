#!/usr/bin/env bash
set -euo pipefail

BUNDLE_FILE="${1:-scs-rclient-linux.flatpak}"

echo "=== Setting up Flathub remote ==="
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo

echo "=== Installing Flatpak Bundle ($BUNDLE_FILE) ==="
flatpak install -y --user --noninteractive "$BUNDLE_FILE"

echo "=== Verifying Installation Metadata ==="
flatpak info online.anthonyis.scs-rclient

echo "=== Verifying Application Permissions ==="
flatpak info --show-permissions online.anthonyis.scs-rclient

echo "=== Verifying Clean Uninstallation ==="
flatpak uninstall -y --user --noninteractive online.anthonyis.scs-rclient

echo "✓ Standalone bundle installation and removal verified!"
