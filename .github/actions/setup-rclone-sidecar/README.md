# Setup Rclone Sidecar Action

A composite GitHub Action that unzips verified rclone binary releases and stages them with the target platform's triple name as a Tauri sidecar executable.

---

## Inputs

| Name | Type | Default | Required | Description |
| :--- | :--- | :--- | :--- | :--- |
| `version` | String | _None_ | Yes | Rclone version to extract (e.g. `1.74.3`). |
| `archive-dir` | String | `'verified-binaries'` | No | Directory containing downloaded and verified `.zip` archives. |
| `target-dir` | String | `'src-tauri/binaries'` | No | Directory where the formatted sidecar binary will be placed. |
| `target-os` | String | `'auto'` | No | Target operating system: `'auto'` (detects runner), `'linux'`, or `'windows'`. |

---

## Outputs

| Name | Description |
| :--- | :--- |
| `sidecar-name` | File name of the staged sidecar executable (e.g. `rclone-sidecar-x86_64-unknown-linux-gnu`). |
| `sidecar-path` | Relative path to the staged sidecar binary. |

---

## Usage Example

```yaml
- name: Download verified rclone binaries
  uses: actions/download-artifact@v7
  with:
    name: verified-rclone
    path: verified-binaries

- name: Setup Rclone Sidecar
  uses: ./.github/actions/setup-rclone-sidecar
  with:
    version: ${{ env.TARGET_RCLONE_VERSION }}
```
