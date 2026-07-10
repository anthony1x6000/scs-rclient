import { Command } from "@tauri-apps/plugin-shell";

let useSystemRclone = true;

/**
 * Detects if the packaged sidecar binary is valid and executable.
 * If execution fails (e.g. due to dummy 0-byte file in local builds),
 * falls back to using the system-installed 'rclone' binary.
 */
export async function detectRclone(): Promise<void> {
  try {
    const testCmd = Command.sidecar("binaries/rclone-sidecar", ["--version"]);
    const res = await testCmd.execute();
    if (res.code === 0) {
      useSystemRclone = false;
      console.log("Using packaged rclone sidecar.");
      (window as any).__TEST_SIDECAR_STATUS__ = "packaged";
      return;
    }
  } catch (e: any) {
    console.warn("Packaged rclone sidecar is invalid or unexecutable. Falling back to system rclone.", e?.message || e);
    (window as any).__TEST_SIDECAR_ERROR__ = e?.message || e;
  }

  try {
    const testSysCmd = Command.create("rclone", ["--version"]);
    const res = await testSysCmd.execute();
    if (res.code === 0) {
      useSystemRclone = true;
      console.log("Using system-installed rclone.");
      (window as any).__TEST_SIDECAR_STATUS__ = "system";
      return;
    } else {
      console.error("System-level rclone returned a non-zero exit code:", res.code);
    }
  } catch (e: any) {
    console.error("System-level rclone is not available or is invalid on this system:", e?.message || e);
  }

  useSystemRclone = true;
}

let detectPromise: Promise<void> | null = null;

/**
 * Ensures that the rclone detection runs exactly once.
 */
export function ensureRcloneDetected(): Promise<void> {
  if (!detectPromise) {
    detectPromise = detectRclone().catch(console.error) as Promise<void>;
  }
  return detectPromise;
}
/**
 * Creates a Tauri Command for running rclone.
 * If the sidecar is invalid or we are in development, it executes the system-installed 'rclone'.
 * Otherwise, it executes the packaged sidecar 'binaries/rclone'.
 */
export function createRcloneCommand(args: string[], env?: Record<string, string>): Command<string> {
  const options = env ? { env } : undefined;
  if (useSystemRclone) {
    // Uses the system-installed rclone executable from the system's PATH
    return Command.create("rclone", args, options);
  } else {
    // Uses the packaged sidecar binary (rclone-sidecar-x86_64-pc-windows-msvc.exe or rclone-sidecar-x86_64-unknown-linux-gnu)
    return Command.sidecar("binaries/rclone-sidecar", args, options);
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
