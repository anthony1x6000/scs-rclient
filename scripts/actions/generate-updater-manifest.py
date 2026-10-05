#!/usr/bin/env python3
"""
Generates the Tauri v2 `latest.json` updater manifest for GitHub Releases.

Tauri's updater plugin expects `latest.json` with the following structure:
{
  "version": "0.1.0",
  "notes": "Release notes...",
  "pub_date": "2026-10-05T12:00:00Z",
  "platforms": {
    "linux-x86_64": {
      "signature": "<minisign-signature>",
      "url": "https://github.com/<owner>/<repo>/releases/download/<tag>/scs-rclient-linux.AppImage"
    },
    "windows-x86_64": {
      "signature": "<minisign-signature>",
      "url": "https://github.com/<owner>/<repo>/releases/download/<tag>/scs-rclient-win-installer.exe"
    }
  }
}
"""

import json
import os
import subprocess
import sys
from datetime import datetime, timezone


def sign_file_if_needed(file_path: str, private_key: str, password: str = "") -> str | None:
    sig_path = f"{file_path}.sig"
    if os.path.exists(sig_path):
        with open(sig_path, "r", encoding="utf-8") as f:
            return f.read().strip()

    if not private_key or not os.path.exists(file_path):
        return None

    try:
        cmd = [
            "pnpm", "tauri", "signer", "sign",
            "-p", password,
            "-k", private_key,
            file_path
        ]
        res = subprocess.run(cmd, capture_output=True, text=True, check=True)
        if os.path.exists(sig_path):
            with open(sig_path, "r", encoding="utf-8") as f:
                return f.read().strip()
    except Exception as e:
        print(f"Warning: Failed to sign {file_path}: {e}", file=sys.stderr)

    return None


def main():
    if len(sys.argv) < 3:
        print("Usage: generate-updater-manifest.py <assets_dir> <tag_name> [repo]", file=sys.stderr)
        sys.exit(1)

    assets_dir = sys.argv[1]
    tag_name = sys.argv[2]
    repo = sys.argv[3] if len(sys.argv) > 3 else os.environ.get("GITHUB_REPOSITORY", "anthony1x6000/scs-rclient")

    version = tag_name.lstrip("v")
    private_key = os.environ.get("TAURI_SIGNING_PRIVATE_KEY", "").strip()
    key_password = os.environ.get("TAURI_SIGNING_PRIVATE_KEY_PASSWORD", "").strip()

    # Supported updater artifact mapping: platform_key -> asset_filename
    target_artifacts = {
        "linux-x86_64": "scs-rclient-linux.AppImage",
        "windows-x86_64": "scs-rclient-win-installer.exe"
    }

    platforms = {}

    for platform_key, asset_name in target_artifacts.items():
        asset_path = os.path.join(assets_dir, asset_name)
        sig = sign_file_if_needed(asset_path, private_key, key_password)

        if not sig:
            # Also check if a standalone .sig exists under artifacts or assets_dir
            candidate_sig = os.path.join(assets_dir, f"{asset_name}.sig")
            if os.path.exists(candidate_sig):
                with open(candidate_sig, "r", encoding="utf-8") as f:
                    sig = f.read().strip()

        if sig:
            download_url = f"https://github.com/{repo}/releases/download/{tag_name}/{asset_name}"
            platforms[platform_key] = {
                "signature": sig,
                "url": download_url
            }
            print(f"✓ Registered updater target {platform_key} for {asset_name}")
        else:
            print(f"Notice: No signature found or generated for {asset_name} ({platform_key})", file=sys.stderr)

    if not platforms:
        print("Notice: No signed platform artifacts available. Skipping latest.json generation.", file=sys.stderr)
        return

    notes = os.environ.get("RELEASE_NOTES", f"Release {tag_name}")
    pub_date = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    manifest = {
        "version": version,
        "notes": notes,
        "pub_date": pub_date,
        "platforms": platforms
    }

    out_file = os.path.join(assets_dir, "latest.json")
    with open(out_file, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)

    print(f"✓ Successfully wrote {out_file} with targets: {list(platforms.keys())}")


if __name__ == "__main__":
    main()
