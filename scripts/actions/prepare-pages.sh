#!/usr/bin/env bash
set -euo pipefail

SITE_DIR="${1:-_site}"

mkdir -p "$SITE_DIR"
if [ -f repo.tar.gz ]; then
  echo "=== Extracting repo.tar.gz into $SITE_DIR/ ==="
  tar -xzf repo.tar.gz -C "$SITE_DIR/"
elif [ -d repo ] && [ "$SITE_DIR" != "." ]; then
  echo "=== Copying existing repo into $SITE_DIR/ ==="
  cp -a repo "$SITE_DIR/"
fi

mkdir -p "$SITE_DIR/scripts/actions" "$SITE_DIR/scripts/test" "$SITE_DIR/.github/flatpak"
if [ -d scripts ]; then
  cp -a scripts "$SITE_DIR/"
fi
if [ -f .github/flatpak/online.anthonyis.scs-rclient.yml ]; then
  cp .github/flatpak/online.anthonyis.scs-rclient.yml "$SITE_DIR/online.anthonyis.scs-rclient.yml"
  cp .github/flatpak/online.anthonyis.scs-rclient.yml "$SITE_DIR/.github/flatpak/online.anthonyis.scs-rclient.yml"
fi
if [ -f .github/flatpak/online.anthonyis.scs-rclient.desktop ]; then
  cp .github/flatpak/online.anthonyis.scs-rclient.desktop "$SITE_DIR/online.anthonyis.scs-rclient.desktop"
  cp .github/flatpak/online.anthonyis.scs-rclient.desktop "$SITE_DIR/.github/flatpak/online.anthonyis.scs-rclient.desktop"
fi
if [ -f .github/flatpak/online.anthonyis.scs-rclient.metainfo.xml ]; then
  cp .github/flatpak/online.anthonyis.scs-rclient.metainfo.xml "$SITE_DIR/online.anthonyis.scs-rclient.metainfo.xml"
  cp .github/flatpak/online.anthonyis.scs-rclient.metainfo.xml "$SITE_DIR/.github/flatpak/online.anthonyis.scs-rclient.metainfo.xml"
fi
if [ -f README.md ]; then
  cp README.md "$SITE_DIR/README.md"
fi

echo "=== Dynamically pulling raw README.md and rendering via GitHub Markdown REST API ==="
python3 - << 'PYEOF' "$SITE_DIR"
import base64
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.request

site_dir = sys.argv[1]
os.makedirs(site_dir, exist_ok=True)

# 1. Dynamically resolve GitHub repository (<owner>/<repo>)
repo = os.environ.get("GITHUB_REPOSITORY")
if not repo:
    try:
        remote_url = subprocess.check_output(
            ["git", "config", "--get", "remote.origin.url"], text=True
        ).strip()
        m = re.search(r"github\.com[:/]([^/]+)/(.+?)(?:\.git)?$", remote_url)
        if m:
            repo = f"{m.group(1)}/{m.group(2)}"
    except Exception:
        pass

if not repo:
    repo = "anthony1x6000/scs-rclient"

owner, repo_name = repo.split("/", 1)

# 2. Dynamically resolve branch or ref
ref = os.environ.get("GITHUB_REF_NAME") or os.environ.get("GITHUB_SHA")
if not ref:
    try:
        ref = subprocess.check_output(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"], text=True
        ).strip()
        if ref == "HEAD":
            ref = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    except Exception:
        ref = "main"

print(f"Target repository: {repo} (ref: {ref})")

# 3. Pull README dynamically as raw file from GitHub or fallback to local
token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
raw_url = f"https://raw.githubusercontent.com/{repo}/{ref}/README.md"
auth_headers = {"User-Agent": "SCS-RClient-Flatpak-Pages-Builder"}
if token:
    auth_headers["Authorization"] = f"Bearer {token}"

readme_text = None
try:
    print(f"Fetching raw README from: {raw_url}")
    req_raw = urllib.request.Request(raw_url, headers=auth_headers)
    with urllib.request.urlopen(req_raw, timeout=30) as resp:
        readme_text = resp.read().decode("utf-8")
    print(f"✓ Dynamically fetched {len(readme_text)} bytes from raw repository URL")
except Exception as e:
    print(f"Notice: Could not fetch from raw URL ({e}), falling back to local README.md", file=sys.stderr)
    if os.path.exists("README.md"):
        with open("README.md", "r", encoding="utf-8") as f:
            readme_text = f.read()

if not readme_text:
    print("::error::Could not retrieve README content from remote or local!", file=sys.stderr)
    sys.exit(1)

# 4. Render markdown using GitHub REST API endpoint
api_url = "https://api.github.com/markdown"
api_headers = {
    "Accept": "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
    "User-Agent": "SCS-RClient-Flatpak-Pages-Builder",
    "Content-Type": "application/json"
}
if token:
    api_headers["Authorization"] = f"Bearer {token}"

payload = json.dumps({
    "text": readme_text,
    "mode": "gfm",
    "context": repo
}).encode("utf-8")

req_api = urllib.request.Request(api_url, data=payload, headers=api_headers, method="POST")

try:
    with urllib.request.urlopen(req_api, timeout=30) as response:
        rendered_html = response.read().decode("utf-8")
    print(f"✓ Successfully rendered markdown via GitHub Markdown REST API ({len(rendered_html)} bytes HTML)")
except Exception as e:
    print(f"Warning: Failed to render via GitHub API ({e}), using raw fallback", file=sys.stderr)
    rendered_html = f"<pre>{readme_text}</pre>"

# 5. Output pure semantic HTML without CSS
index_html = f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>scs-rclient Flatpak Repository ({repo_name})</title>
</head>
<body>
  <main>
{rendered_html}
  </main>
</body>
</html>
"""

output_html_file = os.path.join(site_dir, "index.html")
with open(output_html_file, "w", encoding="utf-8") as f:
    f.write(index_html)

# 6. Dynamically write Flatpak repository & ref configuration files
flatpakrepo_content = f"""[Flatpak Repo]
Title=scs-rclient
Url=https://{owner}.github.io/{repo_name}/repo/
Homepage=https://github.com/{repo}
Comment=Flatpak repository for scs-rclient
Description=Automated Flatpak repository for scs-rclient hosted via GitHub Pages.
Icon=https://raw.githubusercontent.com/{repo}/main/src-tauri/icons/128x128.png
gpg-verify=false
"""

flatpakref_content = f"""[Flatpak Ref]
Name=online.anthonyis.scs-rclient
Branch=master
Title=scs-rclient
IsRuntime=false
Url=https://{owner}.github.io/{repo_name}/repo/
SuggestRemoteName=scs-rclient
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
Homepage=https://github.com/{repo}
Comment=GUI wrapper for rclone
Description=Flatpak packaging for scs-rclient desktop application
Icon=https://raw.githubusercontent.com/{repo}/main/src-tauri/icons/128x128.png
gpg-verify=false
"""

for name in ["online.anthonyis.scs-rclient", repo_name]:
    with open(os.path.join(site_dir, f"{name}.flatpakrepo"), "w", encoding="utf-8") as f:
        f.write(flatpakrepo_content)
    with open(os.path.join(site_dir, f"{name}.flatpakref"), "w", encoding="utf-8") as f:
        f.write(flatpakref_content)

# Copy icons if available
icons_dest = os.path.join(site_dir, "icons")
os.makedirs(icons_dest, exist_ok=True)
if os.path.exists("src-tauri/icons"):
    for icon_name in os.listdir("src-tauri/icons"):
        src_icon = os.path.join("src-tauri/icons", icon_name)
        if os.path.isfile(src_icon):
            shutil.copy2(src_icon, os.path.join(icons_dest, icon_name))
elif os.path.exists("icons"):
    for icon_name in os.listdir("icons"):
        src_icon = os.path.join("icons", icon_name)
        if os.path.isfile(src_icon):
            shutil.copy2(src_icon, os.path.join(icons_dest, icon_name))

print(f"✓ Generated {output_html_file}, repository, and ref files")
PYEOF

touch "$SITE_DIR/.nojekyll"

echo "✓ GitHub Pages static site successfully prepared in $SITE_DIR!"
