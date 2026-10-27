import { load, type Store } from "@tauri-apps/plugin-store";
import type { BulkPreset } from "./bulkPresets";
import {
  excludedFoldersFor,
  exclusionScopeKey,
  isLegacyExcludedFoldersList,
  legacyFallbackList,
  migrateLegacyList,
  parseExcludedFoldersMap,
  MAX_EXCLUDED_FOLDERS,
  type ExcludedFoldersByDir,
} from "./exclusions";
import {
  normalizeExtensionList,
  sanitizeExtensionMap,
  subdirKey,
  type ExtensionFilterMode,
  type ExtensionFilterState,
} from "./extensionFilter";

// Re-exported for existing consumers; the logic lives in the pure `exclusions` module.
export {
  normalizeExcludedFolder,
  normalizeExcludedFolders,
  excludedFoldersFor,
  exclusionScopeKey,
  MAX_EXCLUDED_FOLDERS,
  MAX_EXCLUDED_FOLDER_LENGTH,
} from "./exclusions";
export type { ExcludedFoldersByDir } from "./exclusions";

export const SETTINGS_FILE = "settings.json";

export const STORE_KEYS = {
  webdavUrl: "webdav_url",
  savedUsername: "saved_username",
  selectedSubdir: "selected_subdirectory",
  targetSubdir: "target_subdirectory",
  legacyTestSubdir: "test_subdirectory",
  subdirectories: "subdirectories",
  bulkSelectedSubdirs: "bulk_selected_subdirectories",
  bulkPresets: "bulk_presets",
  scanConcurrency: "scan_concurrency",
  excludedFolders: "excluded_folders",
  extensionWhitelist: "extension_whitelist",
  extensionBlacklist: "extension_blacklist",
  extensionWhitelistBySubdir: "extension_whitelist_by_subdir",
  extensionBlacklistBySubdir: "extension_blacklist_by_subdir",
  excludedFoldersLegacy: "excluded_folders_legacy",
} as const;

export const DEFAULT_SCAN_CONCURRENCY = 6;
export const MIN_SCAN_CONCURRENCY = 1;
export const MAX_SCAN_CONCURRENCY = 64;

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

/**
 * Returns the effective WebDAV base, persisting the env-derived default the
 * first time it applies, so backend commands can treat the settings store as
 * the single source of truth for the configured origin.
 */
export async function ensureWebDAVBasePersisted(): Promise<string> {
  const stored = await getWrapped<string>(STORE_KEYS.webdavUrl);
  if (stored && stored !== "VITE_WEBDAV_BASE_URL_PLACEHOLDER") return stored;
  const effective = envBaseUrl();
  if (effective) await setWrapped(STORE_KEYS.webdavUrl, effective);
  return effective;
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
  if (!Array.isArray(raw?.value)) return [];
  // Stored entries are re-validated on load: legacy stores persisted raw text,
  // and a poisoned store file must not select a run scope the add-input guard
  // would have rejected (encoded dot segments, traversal, control characters).
  const { normalizeSubdir } = await import("./utils/rclone");
  const kept: string[] = [];
  for (const entry of raw.value as unknown[]) {
    if (typeof entry !== "string") continue;
    try {
      const normalized = normalizeSubdir(entry);
      if (normalized && !kept.includes(normalized)) kept.push(normalized);
    } catch {
      // Drop entries that fail the same validation applied at add time.
    }
  }
  return kept;
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

/** Recently run bulk folder sets, recency-ordered (index 0 = most recent). */
export async function getBulkPresets(): Promise<BulkPreset[]> {
  const stored = await getWrapped<unknown>(STORE_KEYS.bulkPresets);
  if (!Array.isArray(stored)) return [];
  return stored.filter(
    (entry): entry is BulkPreset =>
      typeof entry === "object" &&
      entry !== null &&
      Array.isArray((entry as BulkPreset).folders) &&
      (entry as BulkPreset).folders.every((folder) => typeof folder === "string"),
  );
}

export async function setBulkPresets(presets: BulkPreset[]): Promise<void> {
  await setWrapped(STORE_KEYS.bulkPresets, presets);
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
 * Serializes every read-modify-write cycle on the exclusion map: the manual
 * editor, the picker, and rapid checkbox toggles queue behind each other, so
 * no save can clobber another's entries.
 */
let excludedFoldersQueue: Promise<unknown> = Promise.resolve();

function enqueueExcludedFoldersWrite<T>(op: () => Promise<T>): Promise<T> {
  const run = excludedFoldersQueue.then(op, op);
  excludedFoldersQueue = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}

/**
 * Raw exclusion-state read (caller must already hold the write queue):
 * the per-directory map plus the legacy fallback list, migrating a legacy
 * flat list on first access after an upgrade — inside the same operation, so
 * the fallback can never be read before the migration that persists it.
 */
async function readExcludedFoldersState(
  subdir: string,
): Promise<{ byDir: ExcludedFoldersByDir; legacy: string[] }> {
  const stored = await getWrapped<unknown>(STORE_KEYS.excludedFolders);
  if (!isLegacyExcludedFoldersList(stored)) {
    const legacyStored = await getWrapped<unknown>(STORE_KEYS.excludedFoldersLegacy);
    return { byDir: parseExcludedFoldersMap(stored), legacy: legacyFallbackList(legacyStored) };
  }
  // First access after an upgrade: import the flat list under `subdir`'s
  // scope and keep the raw copy as every other scope's fallback, so no
  // directory silently loses its exclusions. Legacy copy first: if the map
  // write then fails, the still-present legacy array lets the next attempt
  // converge; the reverse order could lose the fallback forever.
  await setWrapped(STORE_KEYS.excludedFoldersLegacy, stored);
  const byDir = migrateLegacyList(stored, subdir);
  await setWrapped(STORE_KEYS.excludedFolders, byDir);
  return { byDir, legacy: legacyFallbackList(stored) };
}

/**
 * The exclusion state for `subdir` (see [`readExcludedFoldersState`]),
 * serialized behind every other exclusion access.
 */
async function getExcludedFoldersState(
  subdir: string,
): Promise<{ byDir: ExcludedFoldersByDir; legacy: string[] }> {
  return enqueueExcludedFoldersWrite(() => readExcludedFoldersState(subdir));
}

/**
 * Folders skipped by local and remote scanning of `subdir` (patterns relative
 * to it): the explicit scope entry, or the legacy list for scopes the user
 * has not configured yet.
 */
export async function getExcludedFolders(subdir: string): Promise<string[]> {
  const { byDir, legacy } = await getExcludedFoldersState(subdir);
  return excludedFoldersFor(byDir, subdir, legacy);
}

/**
 * Adds one exclusion pattern to `subdir`'s scope. Deltas are applied to the
 * freshest stored list inside the write queue, so panels holding stale state
 * can never overwrite another writer's patterns.
 */
export async function addExcludedFolder(subdir: string, folder: string): Promise<void> {
  await enqueueExcludedFoldersWrite(async () => {
    const { byDir, legacy } = await readExcludedFoldersState(subdir);
    const current = excludedFoldersFor(byDir, subdir, legacy);
    if (current.includes(folder) || current.length >= MAX_EXCLUDED_FOLDERS) return;
    byDir[exclusionScopeKey(subdir)] = [...current, folder];
    await setWrapped(STORE_KEYS.excludedFolders, byDir);
  });
}

/** Removes one exclusion pattern from `subdir`'s scope (see addExcludedFolder). */
export async function removeExcludedFolder(subdir: string, folder: string): Promise<void> {
  await enqueueExcludedFoldersWrite(async () => {
    const { byDir, legacy } = await readExcludedFoldersState(subdir);
    const current = excludedFoldersFor(byDir, subdir, legacy);
    byDir[exclusionScopeKey(subdir)] = current.filter((item) => item !== folder);
    await setWrapped(STORE_KEYS.excludedFolders, byDir);
  });
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
  excludedFoldersByDir: ExcludedFoldersByDir;
  excludedFoldersLegacy: string[];
  extensionFilterState: ExtensionFilterState;
}> {
  const [baseUrl, selectedSubdir, targetSubdir, username, scanConcurrency] = await Promise.all([
    getWebDAVBase(),
    getSelectedSubdir(),
    getTargetSubdir(),
    getSavedUsername(),
    getScanConcurrency(),
  ]);
  // The exclusion state read depends on the selected dir (legacy migration target).
  const { byDir: excludedFoldersByDir, legacy: excludedFoldersLegacy } =
    await getExcludedFoldersState(selectedSubdir);
  const extensionFilterState = await getExtensionFilterState();
  return {
    baseUrl,
    selectedSubdir,
    targetSubdir,
    username,
    scanConcurrency,
    excludedFoldersByDir,
    excludedFoldersLegacy,
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
