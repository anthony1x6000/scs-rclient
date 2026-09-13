import { load, type Store } from "@tauri-apps/plugin-store";

export const SETTINGS_FILE = "settings.json";

export const STORE_KEYS = {
  webdavUrl: "webdav_url",
  savedUsername: "saved_username",
  selectedSubdir: "selected_subdirectory",
  targetSubdir: "target_subdirectory",
  legacyTestSubdir: "test_subdirectory",
  subdirectories: "subdirectories",
} as const;

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

/** Load the independent settings reads in parallel (no sequential waterfall). */
export async function loadAppSettings(): Promise<{
  baseUrl: string;
  selectedSubdir: string;
  targetSubdir: string;
  username: string;
}> {
  const [baseUrl, selectedSubdir, targetSubdir, username] = await Promise.all([
    getWebDAVBase(),
    getSelectedSubdir(),
    getTargetSubdir(),
    getSavedUsername(),
  ]);
  return { baseUrl, selectedSubdir, targetSubdir, username };
}
