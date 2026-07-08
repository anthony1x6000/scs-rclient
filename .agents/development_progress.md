# Development Progress (scs-rclient)

This document tracks recent changes, bug fixes, and workflow improvements made to `scs-rclient`.

## 1. WebDAV Subdirectory Dropdown Fix
* **Issue**: When opening the subdirectory dropdown, only the currently selected item (or items matching it) would show up. Other WebDAV drives/subdirectories were filtered out.
* **Fix**: Modified [Dropdown.tsx](file:///mnt/c/Users/asteve18/OffDrive/GitHub/scs-rclient/src/components/Dropdown.tsx) to clear `searchText` (set to `""`) when opening the dropdown. The currently selected item remains displayed as a placeholder, while all available drives/subdirectories show up in the list.

## 2. Dev Build Credentials & Win32 Execution Fix
* **Issue**: Validating WebDAV credentials during `tauri dev` on Windows failed with `%1 is not a valid Win32 application. (os error 193)`. This happened because the application bypassed the sidecar in development and tried to run `rclone` from the system `PATH`, which pointed to a wrapper bash script that Windows could not execute directly.
* **Fix**: Modified [rclone.ts](file:///mnt/c/Users/asteve18/OffDrive/GitHub/scs-rclient/src/utils/rclone.ts) to verify if the sidecar binary works first (even in development mode) and only fall back to the system-installed `rclone` binary if the sidecar is empty, invalid, or fails. Added detailed logging for system-installed `rclone` failures.

## 3. Support for macOS ARM64 Release Build
* **Tauri Config**: Added `"dmg"` to targets in [tauri.conf.json](file:///mnt/c/Users/asteve18/OffDrive/GitHub/scs-rclient/src-tauri/tauri.conf.json) to bundle the application as a macOS `.dmg` installer.
* **Workflow Additions**: Updated [.github/workflows/tauri-build.yml](file:///mnt/c/Users/asteve18/OffDrive/GitHub/scs-rclient/.github/workflows/tauri-build.yml):
  * Updated the signature verification step to download and verify the `rclone-v1.74.3-osx-arm64.zip` sidecar binary.
  * Added the `build-macos` job running on the native Apple Silicon runner `macos-15`.
  * Configured the macOS job to extract and bundle the ARM64 rclone sidecar binary as `rclone-aarch64-apple-darwin`.
  * Updated the `publish-release` job to download the macOS built artifacts and publish `.dmg` and `.zip` files to the GitHub release, updating the checksum log accordingly.

## 4. GitHub Actions Node.js 20 Deprecation Fix
* **Issue**: Deprecation warnings were generated on GitHub Actions runners due to using an outdated version of the paths-filter action.
* **Fix**: Upgraded `dorny/paths-filter` from `v3` to `v4` in [tauri-build.yml](file:///mnt/c/Users/asteve18/OffDrive/GitHub/scs-rclient/.github/workflows/tauri-build.yml) to ensure compatibility with Node.js 24.

## 5. Linux Build dependency installation optimization
* **Issue**: The `Install Linux dependencies` step hung for ~40 minutes because the `needrestart` daemon on Ubuntu attempted to open interactive prompts for service restarts without a TTY.
* **Fix**: Configured `apt-get` commands in the `build-linux` job to run non-interactively using `NEEDRESTART_MODE=a` and `DEBIAN_FRONTEND=noninteractive`.
