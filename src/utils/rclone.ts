import { Command } from "@tauri-apps/plugin-shell";

export type RcloneStatus = "pending" | "packaged" | "system" | "missing";

let status: RcloneStatus = "pending";
let useSystemRclone = false;

/**
 * Detects if the packaged sidecar binary is valid and executable.
 * Falls back to the system-installed 'rclone' binary (dev convenience only).
 */
export async function detectRclone(): Promise<RcloneStatus> {
  try {
    const testCmd = Command.sidecar("binaries/rclone-sidecar", ["--version"]);
    const res = await testCmd.execute();
    if (res.code === 0) {
      useSystemRclone = false;
      status = "packaged";
      console.log("Using packaged rclone sidecar.");
      (window as any).__TEST_SIDECAR_STATUS__ = "packaged";
      return status;
    }
  } catch (e: any) {
    console.warn("Packaged rclone sidecar is invalid or unexecutable. Checking system rclone fallback.", e?.message || e);
    (window as any).__TEST_SIDECAR_ERROR__ = e?.message || e;
  }

  try {
    const testSysCmd = Command.create("rclone", ["--version"]);
    const res = await testSysCmd.execute();
    if (res.code === 0) {
      useSystemRclone = true;
      status = "system";
      console.log("Using system-installed rclone (dev fallback).");
      (window as any).__TEST_SIDECAR_STATUS__ = "system";
      return status;
    } else {
      console.error("System-level rclone returned a non-zero exit code:", res.code);
    }
  } catch (e: any) {
    console.error("System-level rclone is not available or is invalid on this system:", e?.message || e);
  }

  useSystemRclone = false;
  status = "missing";
  (window as any).__TEST_SIDECAR_STATUS__ = "missing";
  return status;
}

let detectPromise: Promise<RcloneStatus> | null = null;

/**
 * Ensures that the rclone detection runs exactly once.
 * Rejects when neither binary validates so callers can block runs.
 */
export function ensureRcloneDetected(): Promise<RcloneStatus> {
  if (!detectPromise) {
    detectPromise = detectRclone().then((s) => {
      if (s === "missing") {
        throw new Error("No usable rclone binary found (sidecar and system both unavailable).");
      }
      return s;
    }).catch((e) => {
      // Reset so a later retry can re-attempt detection.
      detectPromise = null;
      status = "missing";
      throw e;
    });
  }
  return detectPromise;
}

export function getRcloneStatus(): RcloneStatus {
  return status;
}

/**
 * Creates a Tauri Command for running rclone.
 */
export function createRcloneCommand(args: string[], env?: Record<string, string>): Command<string> {
  const options = env ? { env } : undefined;
  if (useSystemRclone) {
    return Command.create("rclone", args, options);
  } else {
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
 * Rejects `.` / `..` segments and backslash separators to prevent scope escape.
 */
export function normalizeSubdir(subdir: string): string {
  const trimmed = subdir.trim().replace(/\\/g, "/");
  const parts = trimmed.split("/").filter((p) => p.length > 0);
  for (const part of parts) {
    if (part === "." || part === "..") {
      throw new Error(`Invalid subdirectory "${subdir}": "." and ".." segments are not allowed.`);
    }
  }
  return parts.join("/");
}

function validateBaseUrl(baseUrl: string): string {
  const clean = normalizeUrl(baseUrl);
  if (!/^https?:\/\//i.test(clean)) {
    throw new Error(`Invalid base URL "${baseUrl}": must start with http:// or https://.`);
  }
  return clean;
}

/**
 * Resolves the remote WebDAV URL using normalized base and subdirectory parts.
 * Path segments are percent-encoded; `..`/absolute inputs are rejected.
 */
export function resolveRemoteUrl(baseUrl: string, subdir: string): string {
  const cleanBase = validateBaseUrl(baseUrl);
  const cleanSub = subdir ? normalizeSubdir(subdir) : "";
  if (!cleanSub) return cleanBase;
  const encoded = cleanSub.split("/").map((s) => encodeURIComponent(s)).join("/");
  return `${cleanBase}/${encoded}`;
}

/**
 * Resolves the local path by joining the mount directory and subdirectory.
 * Rejects `..` escape; verifies the result stays under the mount dir.
 */
export function resolveLocalPath(mountDir: string, subdir: string): string {
  const cleanMount = mountDir.trim().replace(/\\/g, "/").replace(/\/+$/, "");
  const cleanSub = subdir ? normalizeSubdir(subdir) : "";
  if (!cleanSub) return mountDir.trim();
  const joined = `${cleanMount}/${cleanSub}`;
  const mountParts = cleanMount.split("/").filter(Boolean);
  const joinedParts: string[] = [];
  for (const part of joined.split("/").filter(Boolean)) {
    if (part === "..") {
      throw new Error(`Invalid subdirectory "${subdir}": escapes the mount directory.`);
    }
    if (part !== ".") joinedParts.push(part);
  }
  // Containment check: resolved parts must start with the mount prefix.
  // (Leading "/" tolerated for absolute mounts on POSIX.)
  const prefix = mountParts.join("/");
  if (prefix && !joinedParts.join("/").startsWith(prefix)) {
    throw new Error(`Invalid subdirectory "${subdir}": escapes the mount directory.`);
  }
  return joined;
}

/**
 * Obscures the password using rclone's built-in obscure command.
 */
export async function obscurePassword(password: string): Promise<string> {
  await ensureRcloneDetected();
  const obscureCommand = createRcloneCommand(["obscure", password]);
  const result = await obscureCommand.execute();
  if (result.code !== 0) {
    throw new Error(result.stderr || "Failed to obscure password");
  }
  return result.stdout.trim();
}
