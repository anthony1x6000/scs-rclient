# Agent Guidelines & Development Notes (scs-rclient)

This file contains crucial workspace rules, context, and development process details for future agents maintaining or extending `scs-rclient`.

## 1. Project Overview

`scs-rclient` is a cross-platform desktop application built with **Tauri v2**, **React 19**, and **Tailwind CSS v4**. It replaces traditional visual FTP interfaces (like Dreamweaver) with automated, high-performance command-line operations using **rclone** under the hood.

### Key Architecture Components (Preserving original UI design)
- **Frontend Core (`src/App.tsx`):** Coordinates layout structure. It keeps the original user visual layout completely unchanged (including bottom absolute-positioned configuration panels and size constraints).
- **RcloneActions (`src/components/RcloneActions.tsx`):** Handles all command line operations. It maps click events on standard grid list elements to spawn `rclone` execution tasks.
- **RcloneConsole (`src/components/RcloneConsole.tsx`):** Decouples output log streaming and coordinates automatic scroll viewport anchoring within the exact original styling constraints.
- **Backend Rust (`src-tauri/`):** Exposes keyring secure credentials access and dynamic home documents path mapping commands.

---

## 2. Single Responsibility Principle (SRP) Guidelines

When building or updating client features:
- Do not clutter `src/App.tsx` with business logic.
- Place all functional control operations inside dedicated components under `src/components/` (e.g. `RcloneActions` for executing system command tasks).
- Avoid modifying the layout of component containers inside `App.tsx` directly unless requested, keeping UI style definitions separate from logic execution.

---

## 3. Rclone Dynamic Configuration & Commands

To avoid managing global config files on the user's OS, we execute commands using on-the-fly dynamic WebDAV endpoints.
- Remote targeting uses `:webdav:` with dynamic inline parameters.
- Prior to launching connection tasks, passwords are obscured using `rclone obscure <password>`.
- Concatenations are resolved safely using normalizers to ensure robust slash routing on Linux/macOS/Windows paths.

### Commands reference:
- **Put (Push):** `rclone copy <local_path> :webdav: --webdav-url=... --webdav-user=... --webdav-pass=...`
- **Get (Pull):** `rclone copy :webdav: <local_path> --webdav-url=... --webdav-user=... --webdav-pass=...`
- **ls / lsd:** `rclone ls/lsd :webdav: --webdav-url=...`
- **check:** `rclone check <local_path> :webdav: --webdav-url=...`
- **Simulation:** Append `--dry-run` to copy commands.

---

## 4. Credentials Test Logging Requirement

When validating user connection credentials (inside `src/components/CredentialsForm.tsx`), ensure the complete WebDAV URL path is logged to the debug console:
```typescript
console.log("Testing credentials with rclone...", fullTestUrl);
```
Ensure `fullTestUrl` is fully resolved before executing this log statement.
