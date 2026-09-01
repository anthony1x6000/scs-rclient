#!/usr/bin/env bash
set -euo pipefail

echo "=== Setting up Flathub runtime remote ==="
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo

if [[ -f repo.tar.gz && ! -d repo ]]; then
  echo "=== Extracting OSTree Repository Archive ==="
  tar -xzf repo.tar.gz
fi

if [[ ! -d repo ]]; then
  echo "::error::OSTree repository directory 'repo' not found!"
  exit 1
fi

REMOTE_NAME="scs-test"

echo "=== Adding OSTree Repository Remote ($REMOTE_NAME) ==="
flatpak remote-add --if-not-exists --user --no-gpg-verify "$REMOTE_NAME" "file://$(pwd)/repo"

echo "=== Querying Remote Repository for Available Applications ==="
flatpak remote-ls --user --show-details "$REMOTE_NAME"

echo "=== Inspecting Remote Application Metadata ==="
flatpak remote-info --user "$REMOTE_NAME" online.anthonyis.scs-rclient

echo "=== Pulling and Installing Application from Repository ==="
flatpak install -y --user --noninteractive "$REMOTE_NAME" online.anthonyis.scs-rclient

echo "=== Verifying Installation Details ==="
flatpak info online.anthonyis.scs-rclient

echo "=== Verifying Application Permissions ==="
flatpak info --show-permissions online.anthonyis.scs-rclient

echo "=== Testing Incremental Pull & Update from Repository ==="
flatpak update -y --user --noninteractive online.anthonyis.scs-rclient

echo "=== Verifying Clean Uninstallation & Remote Cleanup ==="
flatpak uninstall -y --user --noninteractive online.anthonyis.scs-rclient
flatpak remote-delete --user "$REMOTE_NAME"

echo "✓ OSTree repository pull, installation, permissions check, and update flow verified successfully!"
