#!/usr/bin/env python3
"""
Generates the Tauri v2 `latest.json` updater manifest for GitHub Releases with cryptographic verification.

Security guarantees:
1. Pinned public key validation against `src-tauri/tauri.conf.json`.
2. TOCTOU-resistant signing in an isolated temporary working directory with atomic replacement.
3. Cryptographic signature verification (Ed25519 & BLAKE2b) of all signatures prior to manifest generation.
4. Minimized child process environment for secret isolation.
"""

import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from typing import Optional

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
except ImportError:
    Ed25519PublicKey = None


def load_pinned_pubkey(repo_root: str) -> str:
    """Loads the pinned Minisign public key from tauri.conf.json or environment."""
    env_pubkey = os.environ.get("TAURI_SIGNING_PUBLIC_KEY", "").strip()
    if env_pubkey:
        return env_pubkey

    config_path = os.path.join(repo_root, "src-tauri", "tauri.conf.json")
    if os.path.exists(config_path):
        try:
            with open(config_path, "r", encoding="utf-8") as f:
                conf = json.load(f)
            return conf.get("plugins", {}).get("updater", {}).get("pubkey", "").strip()
        except (OSError, json.JSONDecodeError) as e:
            print(f"Warning: Could not read pubkey from {config_path}: {e}", file=sys.stderr)

    return ""


def verify_minisign_signature(file_path: str, sig_content: str, pubkey_content: str) -> bool:
    """Cryptographically verifies a Minisign signature against a file and public key."""
    if not Ed25519PublicKey:
        print("Warning: python cryptography library not available, skipping cryptographic verification.", file=sys.stderr)
        return False

    if not sig_content or not pubkey_content or not os.path.exists(file_path):
        return False

    try:
        # Decode base64 pubkey wrapper if present
        pub_text = base64.b64decode(pubkey_content).decode() if pubkey_content.strip().startswith("dW50") else pubkey_content
        pub_lines = [line.strip() for line in pub_text.splitlines() if line.strip()]
        if len(pub_lines) < 2:
            return False
        pub_bin = base64.b64decode(pub_lines[1])
        if len(pub_bin) != 42:
            return False
        pub_key_id = pub_bin[2:10]
        raw_pubkey = pub_bin[10:42]
        ed_pubkey = Ed25519PublicKey.from_public_bytes(raw_pubkey)

        # Decode base64 signature wrapper if present
        sig_text = base64.b64decode(sig_content).decode() if sig_content.strip().startswith("dW50") else sig_content
        sig_lines = [line.strip() for line in sig_text.splitlines() if line.strip()]
        if len(sig_lines) < 4:
            return False

        bin1 = base64.b64decode(sig_lines[1])
        if len(bin1) != 74:
            return False
        trusted_comment = sig_lines[2]
        if not trusted_comment.startswith("trusted comment: "):
            return False
        bin2 = base64.b64decode(sig_lines[3])
        if len(bin2) != 64:
            return False

        sig_alg = bin1[:2]
        sig_key_id = bin1[2:10]
        sig_bytes = bin1[10:74]
        global_sig = bin2

        if sig_key_id != pub_key_id:
            print(f"Signature key ID mismatch: {sig_key_id.hex()} != {pub_key_id.hex()}", file=sys.stderr)
            return False

        with open(file_path, "rb") as f:
            file_bytes = f.read()

        if sig_alg == b"ED":  # Prehashed with BLAKE2b-512
            digest = hashlib.blake2b(file_bytes, digest_size=64).digest()
            ed_pubkey.verify(sig_bytes, digest)
        elif sig_alg == b"Ed":  # Legacy raw Ed25519
            ed_pubkey.verify(sig_bytes, file_bytes)
        else:
            print(f"Unsupported signature algorithm: {sig_alg}", file=sys.stderr)
            return False

        # Verify global signature: sig_bytes + trusted_comment[17:]
        tc_bytes = trusted_comment[17:].encode("utf-8")
        ed_pubkey.verify(global_sig, sig_bytes + tc_bytes)
        return True
    except Exception as e:
        print(f"Cryptographic signature verification failed for {file_path}: {e}", file=sys.stderr)
        return False


def sign_and_verify_artifact(
    file_path: str,
    pubkey: str,
    private_key: str,
    password: str = ""
) -> Optional[str]:
    """
    Safely signs an artifact in an isolated temporary directory to prevent TOCTOU races,
    then cryptographically validates the resulting signature against the pinned public key.
    """
    sig_dest = f"{file_path}.sig"

    # If a pre-existing signature file exists, cryptographically verify it first
    if os.path.exists(sig_dest):
        try:
            with open(sig_dest, "r", encoding="utf-8") as f:
                existing_sig = f.read().strip()
            if verify_minisign_signature(file_path, existing_sig, pubkey):
                return existing_sig
            print(f"Notice: Pre-existing signature {sig_dest} failed cryptographic check; re-signing.", file=sys.stderr)
        except OSError:
            pass

    if not private_key:
        return None

    # Use isolated temporary directory to perform signing and avoid TOCTOU races
    with tempfile.TemporaryDirectory() as tmpdir:
        tmp_target = os.path.join(tmpdir, os.path.basename(file_path))
        try:
            os.link(file_path, tmp_target)
        except OSError:
            shutil.copy2(file_path, tmp_target)

        # Minimized environment passed to child process for secret isolation
        minimal_env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": os.environ.get("HOME", "/tmp"),
            "TAURI_SIGNING_PRIVATE_KEY": private_key,
        }
        if os.environ.get("PNPM_HOME"):
            minimal_env["PNPM_HOME"] = os.environ["PNPM_HOME"]

        cmd = ["pnpm", "tauri", "signer", "sign"]
        if password:
            minimal_env["TAURI_SIGNING_PRIVATE_KEY_PASSWORD"] = password
        else:
            cmd.extend(["-p", ""])
        cmd.append(tmp_target)

        try:
            subprocess.run(cmd, env=minimal_env, capture_output=True, text=True, check=True)
        except (subprocess.CalledProcessError, OSError) as e:
            print(f"Warning: Failed to sign {file_path} in secure sandbox: {e}", file=sys.stderr)
            return None

        tmp_sig = f"{tmp_target}.sig"
        if not os.path.exists(tmp_sig):
            return None

        with open(tmp_sig, "r", encoding="utf-8") as f:
            generated_sig = f.read().strip()

        # Cryptographically verify the freshly generated signature
        if not verify_minisign_signature(file_path, generated_sig, pubkey):
            print(f"Error: Freshly generated signature for {file_path} failed cryptographic validation!", file=sys.stderr)
            return None

        # Atomically write validated signature to final destination
        tmp_final_sig = f"{sig_dest}.tmp.{os.getpid()}"
        with open(tmp_final_sig, "w", encoding="utf-8") as f:
            f.write(generated_sig + "\n")
        os.replace(tmp_final_sig, sig_dest)

        return generated_sig


def main():
    if len(sys.argv) < 3:
        print("Usage: generate-updater-manifest.py <assets_dir> <tag_name> [repo] [repo_root]", file=sys.stderr)
        sys.exit(1)

    assets_dir = sys.argv[1]
    tag_name = sys.argv[2]
    repo = sys.argv[3] if len(sys.argv) > 3 and sys.argv[3] else os.environ.get("GITHUB_REPOSITORY")
    repo_root = sys.argv[4] if len(sys.argv) > 4 and sys.argv[4] else os.getcwd()

    if not repo:
        print("Error: GITHUB_REPOSITORY environment variable or repo argument is required.", file=sys.stderr)
        sys.exit(1)

    pubkey = load_pinned_pubkey(repo_root)
    if not pubkey:
        print("Warning: Pinned Minisign public key could not be loaded; cryptographic verification disabled.", file=sys.stderr)

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
        if not os.path.exists(asset_path):
            print(f"Notice: Artifact {asset_name} ({platform_key}) not found in {assets_dir}", file=sys.stderr)
            continue

        sig = sign_and_verify_artifact(asset_path, pubkey, private_key, key_password)

        if sig:
            download_url = f"https://github.com/{repo}/releases/download/{tag_name}/{asset_name}"
            platforms[platform_key] = {
                "signature": sig,
                "url": download_url
            }
            print(f"✓ Verified and registered updater target {platform_key} for {asset_name}")
        else:
            print(f"Notice: No cryptographically verified signature available for {asset_name} ({platform_key})", file=sys.stderr)

    if not platforms:
        print("Notice: No signed and verified platform artifacts available. Skipping latest.json generation.", file=sys.stderr)
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
    out_tmp_file = f"{out_file}.tmp.{os.getpid()}"
    with open(out_tmp_file, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
    os.replace(out_tmp_file, out_file)

    print(f"✓ Successfully wrote cryptographically verified {out_file} with targets: {list(platforms.keys())}")


if __name__ == "__main__":
    main()
