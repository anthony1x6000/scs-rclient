# Setup Rclone Sidecar Action

A composite GitHub Action that unzips verified rclone binary releases and stages them with the target platform's triple name as a Tauri sidecar executable. It fails closed if the extracted binary is suspiciously small or does not report the pinned rclone version.

## Which name does the app look for?

`bundle.externalBin` declares `binaries/rclone-sidecar`. At runtime `tauri-plugin-shell`
matches that entry against the requested program, reduces it to its **last path
component** (dropping the `binaries/` prefix and *not* touching the target-triple
suffix) and joins it with the directory of the running app executable. The app
therefore only ever looks for:

```
<app exe dir>/rclone-sidecar       # Linux, macOS
<app exe dir>/rclone-sidecar.exe   # Windows
```

The triple-suffixed name this action produces (`sidecar-name`) is the name the
**bundler** consumes from `src-tauri/binaries/`. When you copy the sidecar next to
a distributable app binary yourself, use the `runtime-name` output instead.

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
| `sidecar-name` | File name of the staged sidecar executable (e.g. `rclone-sidecar-x86_64-unknown-linux-gnu`). This is what the bundler reads. |
| `sidecar-path` | Relative path to the staged sidecar binary. |
| `runtime-name` | File name the running app resolves (e.g. `rclone-sidecar` / `rclone-sidecar.exe`). Use this when placing a sidecar next to a distributable app binary. |

---

## Usage Example

```yaml
- name: Download verified rclone binaries
  uses: actions/download-artifact@v7
  with:
    name: verified-rclone
    path: verified-binaries

- name: Setup Rclone Sidecar
  id: sidecar
  uses: ./.github/actions/setup-rclone-sidecar
  with:
    version: ${{ env.TARGET_RCLONE_VERSION }}

- name: Stage sidecar next to the portable binary
  shell: pwsh
  run: Copy-Item -Path 'src-tauri/binaries/${{ steps.sidecar.outputs.sidecar-name }}' -Destination 'dist-win/${{ steps.sidecar.outputs.runtime-name }}'
```
