import { Command } from "@tauri-apps/plugin-shell";

/**
 * Creates a Tauri Command for running rclone.
 * In development (pnpm tauri dev), it executes the system-installed 'rclone' executable (which must be in the developer's PATH).
 * In production (pnpm tauri build), it executes the bundled sidecar 'binaries/rclone'.
 */
export function createRcloneCommand(args: string[]): Command<string> {
  if (import.meta.env.DEV) {
    // Uses the system-installed rclone executable from the system's PATH
    return Command.create("rclone", args);
  } else {
    // Uses the packaged sidecar binary (rclone-x86_64-pc-windows-msvc.exe or rclone-x86_64-unknown-linux-gnu)
    return Command.sidecar("binaries/rclone", args);
  }
}

/**
 * Normalizes a URL by trimming whitespace and trailing slashes.
 */
export function normalizeUrl(url: string): string {
  let clean = url.trim();
  while (clean.endsWith("/")) {
    clean = clean.slice(0, -1);
  }
  return clean;
}

/**
 * Normalizes a subdirectory path by trimming whitespace, leading slashes, and trailing slashes.
 */
export function normalizeSubdir(subdir: string): string {
  let clean = subdir.trim();
  while (clean.startsWith("/")) {
    clean = clean.slice(1);
  }
  while (clean.endsWith("/")) {
    clean = clean.slice(0, -1);
  }
  return clean;
}

/**
 * Resolves the remote WebDAV URL using normalized base and subdirectory parts.
 */
export function resolveRemoteUrl(baseUrl: string, subdir: string): string {
  const cleanBase = normalizeUrl(baseUrl);
  const cleanSub = normalizeSubdir(subdir);
  return cleanSub ? `${cleanBase}/${cleanSub}` : cleanBase;
}

/**
 * Resolves the local path by joining the mount directory and subdirectory.
 */
export function resolveLocalPath(mountDir: string, subdir: string): string {
  const cleanMount = normalizeUrl(mountDir);
  const cleanSub = normalizeSubdir(subdir);
  return cleanSub ? `${cleanMount}/${cleanSub}` : cleanMount;
}

/**
 * Obscures the password using rclone's built-in obscure command.
 */
export async function obscurePassword(password: string): Promise<string> {
  const obscureCommand = createRcloneCommand(["obscure", password]);
  const result = await obscureCommand.execute();
  if (result.code !== 0) {
    throw new Error(result.stderr || "Failed to obscure password");
  }
  return result.stdout.trim();
}
