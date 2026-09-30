#!/usr/bin/env bash
# Assert that a packaged build can resolve its rclone sidecar at runtime.
#
# Why this exact path? Tauri's shell plugin turns the frontend call
#     Command.sidecar("binaries/rclone-sidecar")
# into
#     <directory of the running app executable>/rclone-sidecar
#
#   1. `tauri-plugin-shell`'s `commands.rs` matches the JS-supplied `program`
#      verbatim against `bundle.externalBin`, so the configured entry must stay
#      "binaries/rclone-sidecar".
#   2. `scope.rs::prepare_sidecar` reduces that entry to its last path component
#      (`components().next_back()`), which drops the "binaries/" prefix but does
#      NOT touch a "-<target-triple>" suffix -> "rclone-sidecar".
#   3. `process/mod.rs::relative_command_path` joins `current_exe()`'s parent
#      with that name and appends ".exe" on Windows.
#
# Consequently the ONLY supported sidecar locations are:
#     <app exe dir>/rclone-sidecar        (Linux, macOS)
#     <app exe dir>/rclone-sidecar.exe    (Windows)
# A `binaries/` subdirectory or a "-<target-triple>" suffix is never consulted,
# so shipping those and nothing else makes the app report
# "No usable rclone binary found (sidecar and system both unavailable)".
#
# Usage:
#   verify-sidecar-layout.sh <app-executable | directory> [expected-version]
#
# The expected version defaults to $TARGET_RCLONE_VERSION when set, and the
# version assertion is skipped when neither is provided.
set -euo pipefail

TARGET="${1:-}"
EXPECTED_VERSION="${2:-${TARGET_RCLONE_VERSION:-}}"
MIN_SIDECAR_BYTES="${MIN_SIDECAR_BYTES:-1048576}"

if [[ -z "$TARGET" ]]; then
  echo "::error::usage: $0 <app-executable | directory> [expected-version]" >&2
  exit 1
fi

fail() { echo "::error::$*" >&2; exit 1; }

if [[ -d "$TARGET" ]]; then
  # Useful for build trees (e.g. a Flatpak build-dir) where the app binary
  # cannot be executed but its directory layout can still be asserted.
  EXE_DIR="$(cd "$TARGET" && pwd -P)"
  APP_LABEL="directory $TARGET"
else
  [[ -e "$TARGET" ]] || fail "app executable not found: $TARGET"
  # Tauri reads the fully-resolved executable path (/proc/self/exe on Linux),
  # so mirror that by resolving symlinks before taking the parent directory.
  APP_EXE="$(readlink -f "$TARGET")"
  EXE_DIR="$(dirname "$APP_EXE")"
  APP_LABEL="$APP_EXE"
fi

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) SIDECAR_NAME="rclone-sidecar.exe" ;;
  *) SIDECAR_NAME="rclone-sidecar" ;;
esac

SIDECAR="$EXE_DIR/$SIDECAR_NAME"

echo "=== Sidecar layout check ==="
echo "app:            $APP_LABEL"
echo "exe directory:  $EXE_DIR"
echo "expected name:  $SIDECAR_NAME (triple suffix stripped, no binaries/ prefix)"
echo "expected path:  $SIDECAR"

[[ -e "$SIDECAR" ]] || fail "rclone sidecar missing at $SIDECAR -- the app would report 'No usable rclone binary found (sidecar and system both unavailable)'"
[[ -f "$SIDECAR" ]] || fail "rclone sidecar at $SIDECAR is not a regular file"
[[ -x "$SIDECAR" ]] || fail "rclone sidecar at $SIDECAR is not executable"

SIZE="$(stat -c %s "$SIDECAR" 2>/dev/null || stat -f %z "$SIDECAR")"
SIZE="$(printf '%s' "$SIZE" | tr -dc '0-9')"
echo "sidecar size:   ${SIZE} bytes"
((SIZE >= MIN_SIDECAR_BYTES)) || fail "sidecar at $SIDECAR is only ${SIZE} bytes (< ${MIN_SIDECAR_BYTES}): looks like a build.rs placeholder or a truncated download"

REPORTED="$("$SIDECAR" version 2>/dev/null | head -n 1 || true)"
echo "sidecar reports: ${REPORTED:-<no output>}"
if [[ -n "$EXPECTED_VERSION" ]]; then
  EXPECTED_VERSION="${EXPECTED_VERSION#v}"
  [[ "$REPORTED" == *"rclone v${EXPECTED_VERSION}"* ]] || fail "sidecar at $SIDECAR does not report the expected version v${EXPECTED_VERSION}"
else
  [[ -n "$REPORTED" ]] || fail "sidecar at $SIDECAR produced no version output -- the binary is not runnable"
fi

echo "✓ sidecar resolvable at the path the Tauri shell plugin actually uses"
