# Setup Build Environment Action

A composite GitHub Action that provisions and caches the full development toolchain required to build Tauri applications across Linux, macOS, and Windows runners.

Includes automated setup and multi-layer caching for:
- **pnpm** (via [`pnpm/action-setup`](https://github.com/pnpm/action-setup))
- **Node.js** with pnpm store caching (via [`actions/setup-node`](https://github.com/actions/setup-node))
- **Rust toolchain** (via [`dtolnay/rust-toolchain`](https://github.com/dtolnay/rust-toolchain))
- **Cargo & Rust target cache** (via [`swatinem/rust-cache`](https://github.com/swatinem/rust-cache))

---

## Security & Supply Chain Integrity (2026 Standards)

- **Immutable Commit SHA Pinning**: All external actions are pinned to full 40-character commit hashes to guard against upstream tag mutation or account compromise (SLSA Level 3 / OpenSSF Scorecard compliant).
- **Automated Dependabot Tracking**: Monitored weekly via GitHub Dependabot under `package-ecosystem: "github-actions"`.
- **Zero Injections**: All composite steps run with explicit `shell: bash` to ensure consistent execution and prevent shell-parsing disparities across Linux and Windows runners.

---

## Inputs

| Name | Type | Default | Required | Description |
| :--- | :--- | :--- | :--- | :--- |
| `node-version` | String | `'lts/*'` | No | Version spec of Node.js to install (e.g. `'22'`, `'lts/*'`, or a path to `.nvmrc`). |
| `node-cache` | String | `'pnpm'` | No | Package manager cache for Node.js (`'pnpm'`, `'npm'`, `'yarn'`, or empty string `''` to disable). |
| `pnpm-version` | String | `''` | No | Explicit pnpm version to install. If omitted, automatically resolves from `packageManager` in `package.json`. |
| `install-rust` | Boolean | `'true'` | No | Set to `'false'` for frontend-only or audit CI jobs to skip downloading the Rust toolchain and cache. |
| `rust-toolchain` | String | `'stable'` | No | Rust channel or version to install via `dtolnay/rust-toolchain`. |
| `rust-targets` | String | `''` | No | Comma-separated list of target triples to install (e.g. `x86_64-unknown-linux-gnu`). |
| `rust-components`| String | `''` | No | Comma-separated list of additional Rust components (e.g. `clippy, rustfmt`). |
| `rust-cache` | Boolean | `'true'` | No | Set to `'false'` to disable cargo build cache restoration and saving. |
| `rust-cache-workspaces` | String | `'./src-tauri -> target'` | No | Workspace paths for `swatinem/rust-cache`. |
| `rust-cache-key` | String | `''` | No | Additional cache key for Rust cache differentiation or manual cache busting. |
| `rust-cache-save`| Boolean | `'true'` | No | Whether to save the Rust cache on workflow completion. |
| `install-dependencies` | Boolean | `'false'` | No | Whether to execute `pnpm install --frozen-lockfile` immediately after toolchain setup. |

---

## Outputs

| Name | Description |
| :--- | :--- |
| `node-version` | Resolved Node.js version installed on the runner. |
| `pnpm-version` | Resolved pnpm version installed on the runner. |
| `rustc-version` | Installed `rustc` compiler version (empty if `install-rust: false`). |
| `cargo-version` | Installed `cargo` version (empty if `install-rust: false`). |
| `rust-cache-hit` | Boolean indicating whether an exact match was found in the Rust cache. |

---

## Usage Examples

### 1. Standard Tauri Build (Default)
Sets up Node.js, pnpm, stable Rust, and warms up the Rust cache:

```yaml
- name: Setup Build Environment
  uses: ./.github/actions/setup-build-env
```

### 2. Frontend-Only / Audit CI Job
Skips Rust toolchain and Rust cache to minimize runner minutes:

```yaml
- name: Setup Frontend Environment
  uses: ./.github/actions/setup-build-env
  with:
    install-rust: 'false'
    node-version: 22

- name: Install dependencies
  run: pnpm install --frozen-lockfile

- name: Audit NPM Dependencies
  run: pnpm audit
```

### 3. Automatic Dependency Installation
Provisions all toolchains and automatically runs `pnpm install --frozen-lockfile`:

```yaml
- name: Setup Environment and Install Dependencies
  uses: ./.github/actions/setup-build-env
  with:
    install-dependencies: 'true'
```

### 4. Custom Rust Toolchain with Components & Targets
Installs nightly Rust with `clippy` and a specific target triple:

```yaml
- name: Setup Build Environment
  uses: ./.github/actions/setup-build-env
  with:
    rust-toolchain: 'nightly'
    rust-components: 'clippy, rustfmt'
    rust-targets: 'x86_64-pc-windows-msvc'
```
