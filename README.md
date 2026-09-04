# scs-rclient 

GUI wrapper for rclone. Rclone is bundled. 

To replace dreamweaver webdav functionality.

![scs-rclient Icon](src-tauri/icons/128x128@2x.png)

## Installation

### Install from Flatpak repository

With the Flatpak repository, you get automatic updates whenever a new version is released.

Ensure Flathub and the scs-rclient repository are added:

```bash
# Add Flathub for runtime dependencies
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo

# Add scs-rclient repository
flatpak remote-add --if-not-exists --user scs-rclient https://anthony1x6000.github.io/scs-rclient/online.anthonyis.scs-rclient.flatpakrepo
```

Install:

```bash
flatpak install --user scs-rclient online.anthonyis.scs-rclient
```

With the repo added, you can check for updates with:

```bash
flatpak update
```

### Install the standalone .flatpak (no repository)

```bash
flatpak install --user https://github.com/anthony1x6000/scs-rclient/releases/latest/download/scs-rclient-linux.flatpak
```

## Run the app

```bash
flatpak run online.anthonyis.scs-rclient
```

## Default permissions

- `~/Documents/scs-rclient`
- `~/.config/rclone` (Read-only)
- Secret Service / Keyring (`org.freedesktop.secrets`, `org.kde.kwalletd5/6`)
- Network access (`--share=network`)
- Display server access (X11 & Wayland)

If you need access to other directories for file synchronization, use [Flatseal](https://flathub.org/en/apps/com.github.tchx84.Flatseal) or use the CLI:

```bash
# Grant access to Documents folder
flatpak override --user --filesystem=~/Documents online.anthonyis.scs-rclient

# Or grant access to entire home directory
flatpak override --user --filesystem=host online.anthonyis.scs-rclient
```

## Other release packages

Standalone release binaries are also available from [GitHub Releases](https://github.com/anthony1x6000/scs-rclient/releases/latest):
- **Linux**: AppImage, DEB, RPM, Flatpak
- **Windows**: Portable EXE, Installer EXE
