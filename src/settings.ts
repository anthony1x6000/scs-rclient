import { load, type Store } from "@tauri-apps/plugin-store";
import {
  normalizeExtensionList,
  sanitizeExtensionMap,
  subdirKey,
  type ExtensionFilterMode,
  type ExtensionFilterState,
} from "./extensionFilter";

export const SETTINGS_FILE = "settings.json";

export const STORE_KEYS = {
  webdavUrl: "webdav_url",
  savedUsername: "saved_username",
  selectedSubdir: "selected_subdirectory",
  targetSubdir: "target_subdirectory",
  legacyTestSubdir: "test_subdirectory",
  subdirectories: "subdirectories",
  bulkSelectedSubdirs: "bulk_selected_subdirectories",
  scanConcurrency: "scan_concurrency",
  excludedFolders: "excluded_folders",
  extensionWhitelist: "extension_whitelist",
  extensionBlacklist: "extension_blacklist",
  extensionWhitelistBySubdir: "extension_whitelist_by_subdir",
  extensionBlacklistBySubdir: "extension_blacklist_by_subdir",
} as const;

export const DEFAULT_SCAN_CONCURRENCY = 6;
export const MIN_SCAN_CONCURRENCY = 1;
export const MAX_SCAN_CONCURRENCY = 64;

/** Upper bounds shared with the Rust-side normalization so the UI and backend agree. */
export const MAX_EXCLUDED_FOLDERS = 64;
export const MAX_EXCLUDED_FOLDER_LENGTH = 256;

interface WrappedValue<T> {
  value: T;
}

let storePromise: Promise<Store> | null = null;

/** Load the settings store exactly once and reuse it (avoids N× IPC on startup). */
export function getStore(): Promise<Store> {
  if (!storePromise) {
    storePromise = load(SETTINGS_FILE, { autoSave: true, defaults: {} });
  }
  return storePromise;
}

async function getWrapped<T>(key: string): Promise<T | undefined> {
  const store = await getStore();
  const raw = await store.get<WrappedValue<T>>(key);
  return raw?.value;
}

async function setWrapped<T>(key: string, value: T): Promise<void> {
  const store = await getStore();
  await store.set(key, { value });
  // autoSave:true flushes lazily; explicitly save so a quick exit can't lose the write.
  try {
    await store.save();
  } catch (e) {
    console.error(`Failed to persist setting "${key}":`, e);
    throw e;
  }
}

function envBaseUrl(): string {
  return (import.meta.env["VITE_WEBDAV_BASE_URL"] as string | undefined) ?? "";
}

/**
 * Single source of truth for the WebDAV base URL.
 * Precedence: stored `webdav_url` → `VITE_WEBDAV_BASE_URL` env → "".
 * The literal placeholder string is never treated as a configured URL.
 */
export async function getWebDAVBase(): Promise<string> {
  const stored = await getWrapped<string>(STORE_KEYS.webdavUrl);
  if (stored && stored !== "VITE_WEBDAV_BASE_URL_PLACEHOLDER") return stored;
  return envBaseUrl();
}

export async function setWebDAVBase(url: string): Promise<void> {
  if (url === "VITE_WEBDAV_BASE_URL_PLACEHOLDER") return;
  await setWrapped(STORE_KEYS.webdavUrl, url);
}

/** Per-run remote subdirectory chosen in the Dropdown. No legacy fallback. */
export async function getSelectedSubdir(): Promise<string> {
  return (await getWrapped<string>(STORE_KEYS.selectedSubdir)) ?? "";
}

export async function setSelectedSubdir(subdir: string): Promise<void> {
  await setWrapped(STORE_KEYS.selectedSubdir, subdir);
}

/**
 * Mount-root subdirectory from Settings. Legacy `test_subdirectory` fallback
 * lives here — and only here — so removal touches one place.
 */
export async function getTargetSubdir(): Promise<string> {
  const current = await getWrapped<string>(STORE_KEYS.targetSubdir);
  if (current !== undefined) return current;
  return (await getWrapped<string>(STORE_KEYS.legacyTestSubdir)) ?? "";
}

export async function setTargetSubdir(subdir: string): Promise<void> {
  await setWrapped(STORE_KEYS.targetSubdir, subdir);
}

export async function getSavedUsername(): Promise<string> {
  return (await getWrapped<string>(STORE_KEYS.savedUsername)) ?? "";
}

export async function setSavedUsername(username: string): Promise<void> {
  await setWrapped(STORE_KEYS.savedUsername, username);
}

export async function getSubdirectories(): Promise<string[]> {
  const store = await getStore();
  const raw = await store.get<WrappedValue<unknown>>(STORE_KEYS.subdirectories);
  return Array.isArray(raw?.value) ? (raw.value as string[]) : [];
}

export async function setSubdirectories(items: string[]): Promise<void> {
  await setWrapped(STORE_KEYS.subdirectories, items);
}

/** Folders ticked for bulk actions; normalized against the list by the caller. */
export async function getBulkSelection(): Promise<string[]> {
  const stored = await getWrapped<unknown>(STORE_KEYS.bulkSelectedSubdirs);
  if (!Array.isArray(stored)) return [];
  return stored.filter((entry): entry is string => typeof entry === "string");
}

export async function setBulkSelection(folders: readonly string[]): Promise<void> {
  await setWrapped(STORE_KEYS.bulkSelectedSubdirs, folders);
}

/**
 * Clamps the scan concurrency value to the supported range [MIN_SCAN_CONCURRENCY, MAX_SCAN_CONCURRENCY].
 * Uses truncation toward zero matching Rust's integer clamping behavior.
 */
export function clampScanConcurrency(threads: number): number {
  return Math.min(Math.max(Math.trunc(threads), MIN_SCAN_CONCURRENCY), MAX_SCAN_CONCURRENCY);
}

/**
 * Parses and clamps a string input into a valid scan concurrency integer.
 * Returns null if the input is non-numeric, contains non-digit characters, or is empty.
 */
export function parseAndClampScanConcurrency(raw: string): number | null {
  const trimmed = raw.trim();
  if (trimmed === "" || !/^\d+$/.test(trimmed)) return null;
  const parsed = parseInt(trimmed, 10);
  if (isNaN(parsed)) return null;
  return clampScanConcurrency(parsed);
}

export async function getScanConcurrency(): Promise<number> {
  const val = await getWrapped<number>(STORE_KEYS.scanConcurrency);
  if (typeof val === "number" && !isNaN(val)) {
    return clampScanConcurrency(val);
  }
  return DEFAULT_SCAN_CONCURRENCY;
}

export async function setScanConcurrency(threads: number): Promise<void> {
  const clamped = clampScanConcurrency(threads);
  // Push the new limit to the backend so an operation already in flight resizes
  // its worker pool instead of waiting for the next run.
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("set_scan_concurrency", { threads: clamped });
  } catch (e) {
    console.error("Failed to apply scan concurrency to the running operation:", e);
  }
  await setWrapped(STORE_KEYS.scanConcurrency, clamped);
}

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

/** Folders skipped by local and remote scanning for uploads and downloads. */
export async function getExcludedFolders(): Promise<string[]> {
  const stored = await getWrapped<unknown>(STORE_KEYS.excludedFolders);
  if (!Array.isArray(stored)) return [];
  return normalizeExcludedFolders(stored.filter((entry): entry is string => typeof entry === "string"));
}

export async function setExcludedFolders(folders: readonly string[]): Promise<void> {
  await setWrapped(STORE_KEYS.excludedFolders, normalizeExcludedFolders(folders));
}


/** Loads every persisted extension filter; each read is sanitized independently. */
export async function getExtensionFilterState(): Promise<ExtensionFilterState> {
  const [globalWhitelist, globalBlacklist, perDirWhitelist, perDirBlacklist] = await Promise.all([
    getWrapped<unknown>(STORE_KEYS.extensionWhitelist),
    getWrapped<unknown>(STORE_KEYS.extensionBlacklist),
    getWrapped<unknown>(STORE_KEYS.extensionWhitelistBySubdir),
    getWrapped<unknown>(STORE_KEYS.extensionBlacklistBySubdir),
  ]);
  return {
    globalWhitelist: normalizeExtensionList(Array.isArray(globalWhitelist) ? globalWhitelist : []),
    globalBlacklist: normalizeExtensionList(Array.isArray(globalBlacklist) ? globalBlacklist : []),
    perDirWhitelist: sanitizeExtensionMap(perDirWhitelist),
    perDirBlacklist: sanitizeExtensionMap(perDirBlacklist),
  };
}

/** Saves (or clears, with an empty list) the global filter for one mode. */
export async function setGlobalExtensionFilter(
  mode: ExtensionFilterMode,
  extensions: readonly string[],
): Promise<void> {
  const key =
    mode === "whitelist" ? STORE_KEYS.extensionWhitelist : STORE_KEYS.extensionBlacklist;
  await setWrapped(key, normalizeExtensionList(extensions));
}

/** Saves (or clears, with an empty list) the per-dir filter for one mode. */
export async function setSubdirExtensionFilter(
  mode: ExtensionFilterMode,
  subdir: string,
  extensions: readonly string[],
): Promise<void> {
  const storeKey =
    mode === "whitelist"
      ? STORE_KEYS.extensionWhitelistBySubdir
      : STORE_KEYS.extensionBlacklistBySubdir;
  const dirKey = subdirKey(subdir);
  if (!dirKey) throw new Error("Enter a subdirectory first.");
  const state = await getExtensionFilterState();
  const map = mode === "whitelist" ? state.perDirWhitelist : state.perDirBlacklist;
  const list = normalizeExtensionList(extensions);
  if (list.length > 0) map[dirKey] = list;
  else delete map[dirKey];
  await setWrapped(storeKey, map);
}

/** Load the independent settings reads in parallel (no sequential waterfall). */
export async function loadAppSettings(): Promise<{
  baseUrl: string;
  selectedSubdir: string;
  targetSubdir: string;
  username: string;
  scanConcurrency: number;
  excludedFolders: string[];
  extensionFilterState: ExtensionFilterState;
}> {
  const [
    baseUrl,
    selectedSubdir,
    targetSubdir,
    username,
    scanConcurrency,
    excludedFolders,
    extensionFilterState,
  ] = await Promise.all([
    getWebDAVBase(),
    getSelectedSubdir(),
    getTargetSubdir(),
    getSavedUsername(),
    getScanConcurrency(),
    getExcludedFolders(),
    getExtensionFilterState(),
  ]);
  return {
    baseUrl,
    selectedSubdir,
    targetSubdir,
    username,
    scanConcurrency,
    excludedFolders,
    extensionFilterState,
  };
}

/**
 * Clears the persistent WebDAV remote directory listing cache.
 * Note: Clears all cached remote endpoint listings across the application.
 */
export async function clearWebDAVCache(): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("clear_webdav_cache");
}
