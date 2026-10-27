import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  addExcludedFolder,
  ensureWebDAVBasePersisted,
  getExcludedFolders,
  removeExcludedFolder,
} from "../settings";
import { MAX_EXCLUDED_FOLDERS, normalizeExcludedFolder } from "../exclusions";
import {
  folderDisplayName,
  formatSize,
  groupFoldersByParent,
  rootFolder,
  type RemoteFolder,
} from "../folderTree";

interface ExcludeFolderSelectProps {
  /** Remote directory the tree is rooted at; the parent remounts on change. */
  subdir: string;
}

/**
 * In-flight folder listings keyed by remote path, shared across panel mounts:
 * closing and reopening the picker reuses the running request instead of
 * spawning a second recursive scan against the server.
 */
const inflightListings = new Map<string, Promise<RemoteFolder[]>>();

/** A listing slower than this releases its shared in-flight entry for retry. */
const LISTING_TIMEOUT_MS = 300_000;

/**
 * Starts a listing and registers it in the shared in-flight map. On settle it
 * evicts only its own entry, so a stale request's cleanup can never evict a
 * newer listing that a timeout-retry already registered.
 */
function startTrackedListing(subdir: string): Promise<RemoteFolder[]> {
  const tracked = invoke<RemoteFolder[]>("list_remote_folders", { subdir }).finally(() => {
    if (inflightListings.get(subdir) === tracked) inflightListings.delete(subdir);
  });
  inflightListings.set(subdir, tracked);
  // Keep the shared promise's rejection observed when a caller abandons it on
  // timeout; awaiters still see the rejection.
  tracked.catch(() => {});
  return tracked;
}

/**
 * Remote folder picker for exclusions: expands the selected remote directory as
 * a lazy tree (the first expansion is the only WebDAV request — deeper levels
 * derive from that listing) with each folder's total size. Ticking a checkbox
 * persists that folder on the selected directory's exclusion list immediately.
 */
export function ExcludeFolderSelect({ subdir }: ExcludeFolderSelectProps) {
  const [ready, setReady] = useState(false);
  const [excluded, setExcluded] = useState<string[]>([]);
  const [folders, setFolders] = useState<RemoteFolder[] | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const activeRef = useRef(true);
  const loadingRef = useRef(false);

  useEffect(() => {
    activeRef.current = true;
    // Persist the env-derived base (dev builds) so the backend can treat the
    // settings store as the sole source of truth for the remote origin.
    Promise.all([ensureWebDAVBasePersisted(), getExcludedFolders(subdir)])
      .then(([, stored]) => {
        if (!activeRef.current) return;
        setExcluded(stored);
        setFolders(null);
        setExpanded(new Set());
        setReady(true);
      })
      .catch((e) => {
        console.error("Failed to load folder picker settings:", e);
        if (activeRef.current) setSaveError("Could not load excluded folders.");
      });
    return () => {
      activeRef.current = false;
    };
  }, [subdir]);

  const childrenByParent = useMemo(() => groupFoldersByParent(folders ?? []), [folders]);

  // One recursive listing covers the whole tree; every deeper expansion is
  // derived from it, so this is the only WebDAV request the picker ever makes.
  // The shared in-flight map dedupes concurrent listings across remounts, and
  // a timeout releases a stalled entry so "toggle the root to retry" works.
  const fetchTree = async () => {
    if (!ready || loadingRef.current) return;
    loadingRef.current = true;
    setLoading(true);
    setListError(null);
    const request = inflightListings.get(subdir) ?? startTrackedListing(subdir);
    const timeout = new Promise<never>((_, reject) => {
      setTimeout(() => reject(new Error("Timed out.")), LISTING_TIMEOUT_MS);
    });
    try {
      const result = await Promise.race([request, timeout]);
      if (activeRef.current) setFolders(result);
    } catch (e: any) {
      // A timed-out caller releases the shared entry so a retry starts fresh.
      if (inflightListings.get(subdir) === request) inflightListings.delete(subdir);
      console.error("Failed to list remote folders:", e);
      if (activeRef.current) setListError(`Could not list remote folders: ${e?.message || e}`);
    } finally {
      loadingRef.current = false;
      if (activeRef.current) setLoading(false);
    }
  };

  const toggleExpand = (path: string) => {
    if (folders === null && !expanded.has(path) && !loadingRef.current) void fetchTree();
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  // Toggles persist as deltas against the freshest stored list, and the UI
  // reconciles with the store afterwards — silent no-ops (cap, duplicates)
  // and other writers' deltas are reflected instead of stale optimistic state.
  const apply = async (
    folder: string,
    op: (dir: string, entry: string) => Promise<void>,
    optimistic: string[],
  ) => {
    const previous = excluded;
    setExcluded(optimistic);
    try {
      await op(subdir, folder);
      setSaveError(null);
      setExcluded(await getExcludedFolders(subdir));
    } catch (e) {
      console.error("Failed to save excluded folders:", e);
      setSaveError("Could not save excluded folders.");
      try {
        setExcluded(await getExcludedFolders(subdir));
      } catch {
        setExcluded(previous);
      }
    }
  };

  const toggleExclude = (folder: string) => {
    if (excluded.includes(folder)) {
      void apply(folder, removeExcludedFolder, excluded.filter((item) => item !== folder));
      return;
    }
    if (!normalizeExcludedFolder(folder)) return;
    if (excluded.length >= MAX_EXCLUDED_FOLDERS) {
      setSaveError(`At most ${MAX_EXCLUDED_FOLDERS} folders can be excluded.`);
      return;
    }
    void apply(folder, addExcludedFolder, [...excluded, folder]);
  };

  const renderRow = (folder: RemoteFolder, depth: number) => {
    const name = folderDisplayName(folder.path);
    const isOpen = expanded.has(folder.path);
    const children = childrenByParent.get(folder.path) ?? [];
    return (
      <div key={folder.path} role="treeitem" aria-expanded={isOpen}>
        <div
          className="flex items-center gap-2 py-0.5"
          style={{ paddingLeft: depth * 16 }}
        >
          <button
            type="button"
            onClick={() => toggleExpand(folder.path)}
            aria-label={isOpen ? `Collapse ${folder.path}/` : `Expand ${folder.path}/`}
            className="w-4 shrink-0 text-gray-400 hover:text-white cursor-pointer transition-colors duration-150"
          >
            {isOpen ? "▾" : "▸"}
          </button>
          <input
            type="checkbox"
            checked={excluded.includes(folder.path)}
            onChange={() => toggleExclude(folder.path)}
            aria-label={`Exclude ${folder.path}/`}
            className="shrink-0 cursor-pointer"
          />
          <span className="font-mono truncate" title={`${folder.path}/`}>
            {name}/
          </span>
          <span className="ml-auto pl-2 text-gray-400 font-light whitespace-nowrap">
            {formatSize(folder.size)}
          </span>
        </div>
        {isOpen && (
          <div role="group">
            {children.length > 0 ? (
              children.map((child) => renderRow(child, depth + 1))
            ) : (
              <p
                className="text-gray-400 font-light py-0.5"
                style={{ paddingLeft: depth * 16 + 24 }}
              >
                No subfolders.
              </p>
            )}
          </div>
        )}
      </div>
    );
  };

  const root = folders !== null ? rootFolder(folders) : null;
  const rootChildren = root !== null ? childrenByParent.get("") ?? [] : [];
  const rootOpen = expanded.has("");

  return (
    <div id="scs-exclude-folder-select" className="px-2 pb-1 text-xs text-white">
      <p className="mt-1 text-[10px] text-gray-400 font-light">
        Tick remote folders to exclude for{" "}
        <span className="font-mono">{ready ? subdir || "…" : "…"}</span>. Sizes
        total every file beneath a folder.
      </p>
      {ready && !subdir && (
        <p className="mt-1 text-gray-400 font-light">Select a subdirectory first.</p>
      )}
      {ready && subdir && (
        <div
          role="tree"
          aria-label={`Remote folders under ${subdir}`}
          className="mt-1 max-h-64 overflow-y-auto"
        >
          <div role="treeitem" aria-expanded={rootOpen}>
            <div className="flex items-center gap-2 py-0.5">
              <button
                type="button"
                onClick={() => toggleExpand("")}
                aria-label={rootOpen ? "Collapse remote root" : "Expand remote root"}
                className="w-4 shrink-0 text-gray-400 hover:text-white cursor-pointer transition-colors duration-150"
              >
                {rootOpen ? "▾" : "▸"}
              </button>
              <span className="font-mono truncate" title={subdir}>
                {subdir}
              </span>
              <span className="ml-auto pl-2 text-gray-400 font-light whitespace-nowrap">
                {loading && folders === null
                  ? "Loading…"
                  : root !== null
                    ? formatSize(root.size)
                    : ""}
              </span>
            </div>
            {rootOpen && folders !== null && (
              <div role="group">
                {rootChildren.length > 0 ? (
                  rootChildren.map((child) => renderRow(child, 1))
                ) : (
                  <p className="text-gray-400 font-light py-0.5 pl-6">No subfolders.</p>
                )}
              </div>
            )}
          </div>
        </div>
      )}
      {listError && (
        <p role="alert" className="mt-1 text-red-300">
          {listError} Toggle the root folder to retry.
        </p>
      )}
      {saveError && (
        <p role="alert" className="mt-1 text-red-300">
          {saveError}
        </p>
      )}
    </div>
  );
}

export default ExcludeFolderSelect;
