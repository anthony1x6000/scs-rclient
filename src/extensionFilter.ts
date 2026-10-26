import { normalizeSubdir } from "./utils/rclone";

/** Which side of an extension filter a transfer sits on. */
export type ExtensionFilterMode = "whitelist" | "blacklist";

/** Effective filter handed to the backend for one folder's action. */
export interface ExtensionFilter {
  mode: ExtensionFilterMode;
  extensions: string[];
}

/** Everything persisted for extension filters, both scopes and both modes. */
export interface ExtensionFilterState {
  perDirWhitelist: Record<string, string[]>;
  perDirBlacklist: Record<string, string[]>;
  globalWhitelist: string[];
  globalBlacklist: string[];
}

/** Upper bounds mirroring the Rust-side normalization so UI and backend agree. */
export const MAX_EXTENSIONS = 64;
export const MAX_EXTENSION_LENGTH = 64;

/**
 * Normalizes one extension entry: trimmed, leading dots dropped (`.html` and `html`
 * are the same), lowercased. Only plain alphanumeric entries survive, so an entry
 * can never smuggle separators, whitespace, or glob syntax. Returns null otherwise.
 */
export function normalizeExtensionEntry(raw: string): string | null {
  const stripped = raw.trim().replace(/^\.+/, "");
  if (!stripped || stripped.length > MAX_EXTENSION_LENGTH) return null;
  if (!/^[a-z0-9]+$/i.test(stripped)) return null;
  return stripped.toLowerCase();
}

/**
 * Parses CSV input (`.html,.txt`) into normalized extensions plus the tokens that
 * were rejected, so the UI can name them instead of silently dropping them.
 * Duplicates collapse and the kept list stops at MAX_EXTENSIONS.
 */
export function parseExtensionCsv(raw: string): { extensions: string[]; invalid: string[] } {
  const seen = new Set<string>();
  const extensions: string[] = [];
  const invalid: string[] = [];
  for (const token of raw.split(",")) {
    const trimmed = token.trim();
    if (!trimmed) continue;
    const normalized = normalizeExtensionEntry(trimmed);
    if (!normalized) {
      invalid.push(trimmed);
      continue;
    }
    if (seen.has(normalized) || extensions.length >= MAX_EXTENSIONS) continue;
    seen.add(normalized);
    extensions.push(normalized);
  }
  return { extensions, invalid };
}

/** Joins a normalized list back into CSV form for display and prefill. */
export function formatExtensionCsv(extensions: readonly string[]): string {
  return extensions.join(",");
}

/** Sanitizes stored lists: strings only, invalid entries dropped, deduped, capped. */
export function normalizeExtensionList(raw: readonly unknown[]): string[] {
  const { extensions } = raw
    .filter((entry): entry is string => typeof entry === "string")
    .reduce(
      (acc: { extensions: string[]; seen: Set<string> }, entry) => {
        const normalized = normalizeExtensionEntry(entry);
        if (normalized && !acc.seen.has(normalized) && acc.extensions.length < MAX_EXTENSIONS) {
          acc.seen.add(normalized);
          acc.extensions.push(normalized);
        }
        return acc;
      },
      { extensions: [], seen: new Set<string>() },
    );
  return extensions;
}

/** Map key for per-dir filters: the same normalization the folder picker applies. */
export function subdirKey(subdir: string | null | undefined): string | null {
  if (!subdir) return null;
  try {
    const key = normalizeSubdir(subdir);
    return key || null;
  } catch {
    return null;
  }
}

/**
 * Resolves the filter that governs `subdir`: a per-dir filter (either kind)
 * replaces the global one entirely, and a whitelist beats a blacklist within
 * the same scope. Null when nothing is set.
 *
 * Per-dir lists match exactly one folder — a list saved for `course/a` does not
 * apply to `course/a/b`; the nested folder resolves its own list or the global
 * one. Bulk runs call this per folder, so each ticked folder gets its own answer.
 */
export function resolveExtensionFilter(
  subdir: string | null | undefined,
  state: ExtensionFilterState,
): ExtensionFilter | null {
  const key = subdirKey(subdir);
  const dirWhitelist = key ? state.perDirWhitelist[key] : undefined;
  const dirBlacklist = key ? state.perDirBlacklist[key] : undefined;
  if (dirWhitelist && dirWhitelist.length > 0) {
    return { mode: "whitelist", extensions: dirWhitelist };
  }
  if (dirBlacklist && dirBlacklist.length > 0) {
    return { mode: "blacklist", extensions: dirBlacklist };
  }
  if (state.globalWhitelist.length > 0) {
    return { mode: "whitelist", extensions: state.globalWhitelist };
  }
  if (state.globalBlacklist.length > 0) {
    return { mode: "blacklist", extensions: state.globalBlacklist };
  }
  return null;
}
