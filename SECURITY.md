# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability within SCS RClient, please do not open a public GitHub issue. Instead, submit a security advisory report through GitHub's Security Advisories tab or email the maintainers directly.

---

## Auto-Updater Security Architecture

SCS RClient integrates the Tauri v2 auto-updater with GitHub Releases as its distribution backend. Updates follow a secure design:

### 1. Cryptographic Signature Verification
- All update artifacts (`.AppImage`, `.exe`) must be cryptographically signed using Minisign (Ed25519) before being distributed.
- The Minisign public key is hardcoded into `src-tauri/tauri.conf.json` under `plugins.updater.pubkey`.
- During update checks and downloads, the Tauri updater plugin automatically verifies that:
  1. The signature matches the public key embedded in the binary.
  2. The update payload has not been tampered with or corrupted in transit.
- In CI, the release pipeline (`scripts/actions/generate-updater-manifest.py`) validates signatures cryptographically using Ed25519 and BLAKE2b before publishing `latest.json`.

### 2. Secret Isolation
- `TAURI_SIGNING_PRIVATE_KEY` is maintained strictly as an encrypted GitHub Actions secret.
- It is never committed to source control and is never accessible in pull request builds.
- Base configurations (`tauri.conf.json`) omit build-time signing requirements so local development and unprivileged CI test builds run securely without access to signing keys.

### 3. Key Rotation & Compromise Response Plan

#### Key Rotation Strategy
1. Generate a new keypair using `pnpm tauri signer generate`.
2. Update the public key `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`.
3. Update the repository secret `TAURI_SIGNING_PRIVATE_KEY` with the new private key in GitHub repository settings.
4. Publish a release signed with both or the new key to transition clients to the new trust anchor.

#### Incident Response (Key Compromise)
In the event that the signing private key or CI signing pipeline is suspected of compromise:
1. **Immediate Revocation**: Immediately delete or rotate the `TAURI_SIGNING_PRIVATE_KEY` repository secret in GitHub Settings to prevent further unauthorized signing.
2. **Halt Update Distribution**: Remove or overwrite `latest.json` on the GitHub Release page with an empty or non-updating manifest so existing client installations will not fetch the compromised release.
3. **Generate Fresh Trust Anchor**: Generate a new Minisign keypair. Update `tauri.conf.json` with the new public key.
4. **Issue Out-of-Band Advisory**: Publish a security notice and provide verified manual installation packages with checksums on the release page.
5. **Client Recovery**: Release a verified emergency release. Users on existing versions will need to manually reinstall the application once if client-side verification blocks automatic upgrades due to the key change.

### 4. Process Capabilities & Least Privilege
- The `process:allow-restart` permission in `src-tauri/capabilities/default.json` is scoped strictly to allow the updater to restart the application after installing updates on platforms requiring manual relaunch.
