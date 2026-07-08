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

## 6. Tauri API Mocking & E2E Testing Pipeline
* **Issue**: Testing sidecar detection logic in development is unreliable on Windows because `tauri dev` creates 0-byte dummy binaries which Windows refuses to execute, throwing `%1 is not a valid Win32 application. (os error 193)`. We needed robust automated testing to verify execution.
* **Fix**: 
  * Integrated **Vitest** for frontend mocking (`test/rclone.test.ts`), verifying the IPC API properly prioritizes the sidecar using `@tauri-apps/api/mocks`.
  * Configured **WebdriverIO** with `tauri-driver` natively (`wdio.conf.ts` and `test/specs/sidecar.e2e.ts`) to extract application logs from the compiled release binaries to ensure the sidecar works correctly outside of the development environment.
  * Added `test`, `test:watch`, and `test:e2e` scripts to `package.json` that inherently block local execution by forcing a `process.env.GITHUB_ACTIONS === 'true'` check.
  * Updated `.github/workflows/tauri-build.yml` to execute these tests directly during compilation across all matrix OS platforms (using `xvfb-run` on Linux and `msedgedriver` on Windows).

## 7. Isolated Testing Environment Scope Fix
* **Issue**: The `vitest` command running in GitHub Actions threw `ERR_MODULE_NOT_FOUND` because `vitest.config.ts` was located in the root. After migrating the config to `test/`, a secondary transpilation crash occurred: `TypeError: (0, import_vite2.default) is not a function`. This happened because Jiti (the transpiler used by Vitest) incorrectly compiled the parent `vite.config.ts`'s named imports as CJS default imports.
* **Fix**: Made `test/vitest.config.ts` entirely standalone. It no longer imports the parent `vite.config.ts`, which successfully bypasses the transpilation bug. Explicitly added `@vitejs/plugin-react` and `vite` as local devDependencies inside `test/package.json` to keep the testing environment sandbox fully self-contained.
* **E2E Compatibility Fix**: The GitHub Actions runner failed running E2E tests with `The requested module '@wdio/native-utils' does not provide an export named 'installMockSyncOverride'`. This was due to an upstream incompatibility between `@wdio/tauri-service@1.2.0` and the newly released WebdriverIO `v9`. WebdriverIO packages inside `test/package.json` were strictly downgraded to `^8.0.0` to restore compatibility.
* **E2E TypeScript Compile Fix**: After downgrading to WebdriverIO v8, compiling `wdio.conf.ts` failed due to missing Node and WebdriverIO type definitions (`Cannot find module '@wdio/types'`, `Cannot find name 'process'`). Explicitly added `@types/node` and `@wdio/types` as direct devDependencies inside `test/package.json` to ensure clean standalone compilation inside `pnpm`'s strict non-hoisted folder scope.

## 8. Devtools Retention
* **Rule**: Ensure the `"devtools"` feature is always retained in the `tauri` dependency within `src-tauri/Cargo.toml`. Although generally considered bloat for strict production releases, it is explicitly requested to be allowed for debugging purposes in this application.
