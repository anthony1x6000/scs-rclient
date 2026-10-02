#!/usr/bin/env bash
# E2E test runner: launches an ephemeral Copyparty WebDAV server and runs WebDAV integration tests.
set -euo pipefail

PORT="${WEBDAV_TEST_PORT:-3923}"
TMP_DIR="$(mktemp -d)"
MOCK_DIR="$TMP_DIR/storage"
CONF_FILE="$TMP_DIR/copyparty.conf"
SERVER_PID=""

mkdir -p "$MOCK_DIR/docs"

cat <<EOF > "$CONF_FILE"
[global]
p: $PORT
dav-rt
dav-auth
q

[/docs]
$MOCK_DIR/docs
accs:
  rwda: testuser
flags:
  rw,d,daw

[accounts]
testuser: testpass
EOF

cleanup() {
  echo "=== Cleaning up WebDAV test environment ==="
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

echo "=== Launching Copyparty WebDAV server on port $PORT ==="
python3 -m copyparty -c "$CONF_FILE" > /tmp/copyparty-test.log 2>&1 &
SERVER_PID=$!

READY=0
for i in {1..30}; do
  if curl -s -u testuser:testpass -X PROPFIND -H "Depth: 0" "http://127.0.0.1:$PORT/docs/" >/dev/null 2>&1; then
    READY=1
    break
  fi
  sleep 0.5
done

if [[ "$READY" -ne 1 ]]; then
  echo "::error::Copyparty failed to start on port $PORT within 15 seconds; server log tail:" >&2
  tail -n 50 /tmp/copyparty-test.log || true
  exit 1
fi

echo "✓ Copyparty WebDAV server is listening and authenticated on port $PORT"

export TEST_WEBDAV_URL="http://127.0.0.1:$PORT/docs/"
export TEST_WEBDAV_USER="testuser"
export TEST_WEBDAV_PASS="testpass"

echo "=== Running WebDAV lib unit tests ==="
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --nocapture

echo "=== Running WebDAV integration & roundtrip E2E tests ==="
cargo test --manifest-path src-tauri/Cargo.toml --test webdav_e2e -- --nocapture

echo "✓ All WebDAV tests passed successfully against Copyparty"
