/**
 * Pure exclusion-pattern logic shared by the settings store, the manual editor,
 * and the remote folder picker. No Tauri imports so it stays unit-testable.
 */

/** Upper bounds shared with the Rust-side normalization so the UI and backend agree. */
export const MAX_EXCLUDED_FOLDERS = 64;
export const MAX_EXCLUDED_FOLDER_LENGTH = 256;

/** Exclusion lists keyed by the normalized remote subdirectory they apply to. */
export type ExcludedFoldersByDir = Record<string, string[]>;

/**
 * Normalizes a user-supplied excluded folder into a slash-separated relative path.
 * Leading/trailing slashes, `.` segments, backslashes, and repeated separators are
 * dropped, so `.git/`, `/.git`, and `.git` all become `.git`. Returns null for empty
 * input, `..` escapes, control characters, or over-long patterns. Matching itself happens
 * in Rust.
 */
export function normalizeExcludedFolder(raw: string): string | null {
  const parts = raw
    .trim()
    .replace(/\\/g, "/")
    .split("/")
    .filter((part) => part.length > 0 && part !== ".");
  if (parts.some((part) => part === ".." || /[\u0000-\u001f\u007f]/.test(part))) return null;
  const normalized = parts.join("/");
  if (!normalized || normalized.length > MAX_EXCLUDED_FOLDER_LENGTH) return null;
  return normalized;
}

/** Normalizes, drops invalid entries, and deduplicates exclusion patterns in order. */
export function normalizeExcludedFolders(raw: readonly string[]): string[] {
  const seen = new Set<string>();
  const normalized: string[] = [];
  for (const entry of raw) {
    if (normalized.length >= MAX_EXCLUDED_FOLDERS) break;
    if (typeof entry !== "string") continue;
    const folder = normalizeExcludedFolder(entry);
    if (!folder || seen.has(folder)) continue;
    seen.add(folder);
    normalized.push(folder);
  }
  return normalized;
}

/**
 * Reads a raw stored value as a per-directory map. The result is
 * prototype-less so a hand-edited `__proto__` scope key stays plain data.
 * Legacy flat arrays are not handled here — see [`migrateLegacyList`].
 */
export function parseExcludedFoldersMap(value: unknown): ExcludedFoldersByDir {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return {};
  const map = Object.create(null) as ExcludedFoldersByDir;
  for (const [key, list] of Object.entries(value)) {
    if (!Array.isArray(list)) return {};
    map[key] = list;
  }
  return map;
}

/** True for the pre-scope storage shape: one flat pattern list. */
export function isLegacyExcludedFoldersList(value: unknown): value is unknown[] {
  return Array.isArray(value);
}

/**
 * One-time import of the legacy flat exclusion list into `subdir`'s scope, so
 * upgrading never silently strips protections the user had configured.
 */
export function migrateLegacyList(
  raw: readonly unknown[],
  subdir: string,
): ExcludedFoldersByDir {
  const map = Object.create(null) as ExcludedFoldersByDir;
  map[exclusionScopeKey(subdir)] = normalizeExcludedFolders(
    raw.filter((entry): entry is string => typeof entry === "string"),
  );
  return map;
}

/** Storage key for a directory's exclusions: a normalized slash-separated path. */
export function exclusionScopeKey(subdir: string): string {
  return subdir
    .trim()
    .replace(/\\/g, "/")
    .split("/")
    .filter((part) => part.length > 0 && part !== ".")
    .join("/");
}

/** The exclusion list stored for `subdir`, normalized; unknown dirs have none. */
export function excludedFoldersFor(byDir: ExcludedFoldersByDir, subdir: string): string[] {
  const stored = byDir[exclusionScopeKey(subdir)];
  if (!Array.isArray(stored)) return [];
  return normalizeExcludedFolders(stored.filter((entry): entry is string => typeof entry === "string"));
}
