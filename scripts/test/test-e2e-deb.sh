#!/usr/bin/env bash
# E2E test for the .deb release binary:
# install -> verify structure/permissions -> sidecar WebDAV test -> GUI smoke -> uninstall
set -euo pipefail

# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
unset RCLONE_VERSION

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEB_FILE="${DEB_FILE:-dist-linux/scs-rclient-linux.deb}"

if [[ ! -f "$DEB_FILE" ]]; then
  echo "::error::.deb file not found: $DEB_FILE" >&2
  exit 1
fi

echo "=== Installing .deb ($DEB_FILE) ==="
sudo dpkg -i "$DEB_FILE" || sudo apt-get install -f -y

echo "=== Verifying package installation ==="
dpkg -l scs-rclient
dpkg -L scs-rclient

echo "=== Locating installed app binary and rclone sidecar ==="
APP_BIN="/usr/bin/scs-rclient"
if [[ ! -x "$APP_BIN" ]]; then
  echo "::error::Expected app binary not found at $APP_BIN" >&2
  exit 1
fi
ls -l "$APP_BIN"

# The sidecar ships next to the app binary or under /usr/lib/scs-rclient.
SIDECAR="$(find /usr/bin /usr/lib /opt -name 'rclone-sidecar*' -type f 2>/dev/null | head -n 1 || true)"
if [[ -z "$SIDECAR" ]]; then
  echo "::error::Installed rclone sidecar not found!" >&2
  exit 1
fi
echo "Found sidecar: $SIDECAR"
ls -l "$SIDECAR"
test -x "$SIDECAR" || { echo "::error::Sidecar is not executable: $SIDECAR" >&2; exit 1; }

echo "=== Running sidecar WebDAV verification ==="
bash "$SCRIPT_DIR/verify-webdav-endpoint.sh" "$SIDECAR"

echo "=== GUI launch smoke test (xvfb-run) ==="
if timeout 15 xvfb-run -a "$APP_BIN" >/tmp/scs-deb-launch.log 2>&1; then
  echo "App exited on its own within the timeout window."
else
  code=$?
  if [[ "$code" -eq 124 ]]; then
    echo "App stayed running for 15s (healthy start, killed by timeout)."
  else
    echo "::error::App launch failed with code $code; tail:" >&2
    tail -n 50 /tmp/scs-deb-launch.log || true
    exit 1
  fi
fi

echo "=== Uninstalling .deb ==="
sudo dpkg -r scs-rclient
if [[ -e "$APP_BIN" ]]; then
  echo "::error::Binary still exists after uninstall: $APP_BIN" >&2
  exit 1
fi

echo "✓ .deb E2E passed"
