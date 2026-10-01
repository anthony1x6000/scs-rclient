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
  const prefix = mountParts.join("/");
  if (prefix && !joinedParts.join("/").startsWith(prefix)) {
    throw new Error(`Invalid subdirectory "${subdir}": escapes the mount directory.`);
  }
  return joined;
}
