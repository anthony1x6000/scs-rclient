import { Command } from "@tauri-apps/plugin-shell";

/**
 * Creates a Tauri Command for running rclone.
 * In development (pnpm tauri dev), it executes the system-installed 'rclone' executable (which must be in the developer's PATH).
 * In production (pnpm tauri build), it executes the bundled sidecar 'binaries/rclone'.
 */
export function createRcloneCommand(args: string[]): Command {
  if (import.meta.env.DEV) {
    // Uses the system-installed rclone executable from the system's PATH
    return Command.create("rclone", args);
  } else {
    // Uses the packaged sidecar binary (rclone-x86_64-pc-windows-msvc.exe or rclone-x86_64-unknown-linux-gnu)
    return Command.sidecar("binaries/rclone", args);
  }
}
