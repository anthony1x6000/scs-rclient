#!/usr/bin/env bash
# E2E test runner: launches an ephemeral Copyparty WebDAV server with a D2L/IIS simulation proxy
# and runs native WebDAV integration tests against the exact endpoint layout.
set -euo pipefail

PROXY_PORT="${WEBDAV_TEST_PORT:-3923}"
UPSTREAM_PORT=$((PROXY_PORT + 1))
TMP_DIR="$(mktemp -d)"
MOCK_DIR="$TMP_DIR/storage"
CONF_FILE="$TMP_DIR/copyparty.conf"
SERVER_PID=""
CONTAINER_NAME=""
PROXY_PID=""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COURSE_DIR="content/enforced/1052175-dev_asteve18"
mkdir -p "$MOCK_DIR/$COURSE_DIR"

cat <<EOF > "$CONF_FILE"
[global]
p: $UPSTREAM_PORT
dav-rt
dav-auth
ed
q

[/$COURSE_DIR]
$MOCK_DIR/$COURSE_DIR
accs:
  rwda.: testuser
flags:
  rw,d,daw,dots

[accounts]
testuser: testpass
EOF

cleanup() {
  echo "=== Cleaning up WebDAV test environment ==="
  if [[ -n "$PROXY_PID" ]]; then
    kill "$PROXY_PID" 2>/dev/null || true
    wait "$PROXY_PID" 2>/dev/null || true
  fi
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ -n "${CONTAINER_NAME:-}" ]]; then
    docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

echo "=== Launching Copyparty backend server on port $UPSTREAM_PORT ==="
if command -v docker >/dev/null 2>&1; then
  IMAGE="${WEBDAV_DOCKER_IMAGE:-copyparty/ac:latest}"
  echo "Starting Copyparty via Docker container ($IMAGE)..."
  CONTAINER_NAME="copyparty-e2e-$$-${RANDOM}"
  if docker run -d \
    --name "$CONTAINER_NAME" \
    -p "$UPSTREAM_PORT:$UPSTREAM_PORT" \
    --user "$(id -u):$(id -g)" \
    -v "$TMP_DIR:$TMP_DIR" \
    "$IMAGE" -c "$CONF_FILE" -ed > /tmp/copyparty-docker.log 2>&1; then
    echo "Started Docker container $CONTAINER_NAME"
  else
    echo "::warning::Docker run failed; falling back to python copyparty..."
    CONTAINER_NAME=""
    python3 -m copyparty -c "$CONF_FILE" -ed > /tmp/copyparty-test.log 2>&1 &
    SERVER_PID=$!
  fi
else
  echo "Docker not available; starting Copyparty via Python..."
  python3 -m copyparty -c "$CONF_FILE" -ed > /tmp/copyparty-test.log 2>&1 &
  SERVER_PID=$!
fi

READY=0
for i in {1..30}; do
  if curl -s -u testuser:testpass -X PROPFIND -H "Depth: 0" "http://127.0.0.1:$UPSTREAM_PORT/$COURSE_DIR/" >/dev/null 2>&1; then
    READY=1
    break
  fi
  sleep 0.5
done

if [[ "$READY" -ne 1 ]]; then
  echo "::error::Copyparty failed to start on port $UPSTREAM_PORT within 15 seconds; server logs:" >&2
  if [[ -n "${CONTAINER_NAME:-}" ]]; then
    docker logs "$CONTAINER_NAME" || true
  fi
  tail -n 50 /tmp/copyparty-docker.log 2>/dev/null || true
  tail -n 50 /tmp/copyparty-test.log 2>/dev/null || true
  exit 1
fi

echo "✓ Copyparty backend is ready on port $UPSTREAM_PORT"

echo "=== Launching D2L Brightspace / IIS simulation proxy on port $PROXY_PORT ==="
python3 "$SCRIPT_DIR/d2l_mock_proxy.py" "$UPSTREAM_PORT" "$PROXY_PORT" > /tmp/d2l-proxy.log 2>&1 &
PROXY_PID=$!

PROXY_READY=0
for i in {1..30}; do
  if curl -s -u testuser:testpass -X PROPFIND -H "Depth: 0" "http://127.0.0.1:$PROXY_PORT/$COURSE_DIR/" >/dev/null 2>&1; then
    PROXY_READY=1
    break
  fi
  sleep 0.5
done

if [[ "$PROXY_READY" -ne 1 ]]; then
  echo "::error::D2L simulation proxy failed to start on port $PROXY_PORT within 15 seconds; proxy log tail:" >&2
  tail -n 50 /tmp/d2l-proxy.log || true
  exit 1
fi

echo "✓ D2L simulation proxy is listening on port $PROXY_PORT"
echo "  - Rejects 'Depth: infinity' with HTTP 403 Forbidden"
echo "  - Wraps PROPFIND hrefs with <d:href><![CDATA[...]]></d:href>"

export TEST_WEBDAV_URL="http://127.0.0.1:$PROXY_PORT/$COURSE_DIR/"
export TEST_WEBDAV_USER="testuser"
export TEST_WEBDAV_PASS="testpass"

echo "=== Running WebDAV lib unit tests ==="
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --nocapture

echo "=== Running WebDAV integration & roundtrip E2E tests against simulated D2L endpoint ==="
cargo test --manifest-path src-tauri/Cargo.toml --test webdav_e2e -- --nocapture --test-threads=1

echo "✓ All WebDAV tests passed successfully against D2L simulation setup!"
