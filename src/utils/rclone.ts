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

/** WHATWG dot-segment spellings the URL parser collapses into parent pops. */
const DOT_SEGMENT = /^(\.|\.\.|%2e|\.%2e|%2e\.|%2e%2e)$/i;
/** Control characters never valid in a folder name handed to the backend. */
const CONTROL_CHARS = /[\u0000-\u001f\u007f]/;

/**
 * Normalizes a subdirectory path by trimming whitespace, leading slashes, and trailing slashes.
 * Rejects `.` / `..` segments, percent-encoded dot segments, control characters, and backslash
 * separators to prevent scope escape. Segments are also checked after stripping trailing ASCII
 * spaces, dots, and `%20` (the trim Windows/IIS-family servers apply per component), so
 * spellings like `.. `, `...`, or `..%20` that alias onto a dot segment at resolve time are
 * rejected. Encoded separators (`%2f`/`%5c`) cannot traverse here: `resolveRemoteUrl` passes
 * each segment through `encodeURIComponent`, which re-encodes the `%` (so `%2f` is sent as
 * `%252f`) and the backend decodes it back to a literal, separator-free name; the Rust-side
 * guard additionally rejects them in the configured URL.
 */
export function normalizeSubdir(subdir: string): string {
  const trimmed = subdir.trim().replace(/\\/g, "/");
  const parts = trimmed.split("/").filter((p) => p.length > 0);
  for (const part of parts) {
    // What remains after the Windows-style per-segment trim: a part made
    // entirely of dots/spaces collapses to "", and encoded-dot spellings
    // are exposed to the DOT_SEGMENT check.
    const core = part.replace(/(%20|[ .])+$/i, "");
    if (
      core === "" ||
      core === "." ||
      core === ".." ||
      DOT_SEGMENT.test(core) ||
      CONTROL_CHARS.test(part)
    ) {
      throw new Error(`Invalid subdirectory "${subdir}": dot segments, encoded dot segments, windows dot-alias spellings, and control characters are not allowed.`);
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
