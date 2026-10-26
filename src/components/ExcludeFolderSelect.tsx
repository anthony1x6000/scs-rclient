import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  getExcludedFolders,
  setExcludedFolders,
  getSavedUsername,
  getSelectedSubdir,
  getWebDAVBase,
} from "../settings";
import { MAX_EXCLUDED_FOLDERS, normalizeExcludedFolder } from "../exclusions";
import {
  folderDisplayName,
  formatSize,
  groupFoldersByParent,
  rootFolder,
  type RemoteFolder,
} from "../folderTree";

interface PickerConfig {
  baseUrl: string;
  subdir: string;
  username: string;
}

/**
 * Remote folder picker for exclusions: expands the selected remote directory as
 * a lazy tree (the first expansion is the only WebDAV request — deeper levels
 * derive from that listing) with each folder's total size. Ticking a checkbox
 * persists that folder on the selected directory's exclusion list immediately.
 */
export function ExcludeFolderSelect() {
  const [config, setConfig] = useState<PickerConfig | null>(null);
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
    Promise.all([getWebDAVBase(), getSelectedSubdir(), getSavedUsername()])
      .then(async ([baseUrl, subdir, username]) => {
        const stored = await getExcludedFolders(subdir);
        if (!activeRef.current) return;
        setConfig({ baseUrl, subdir, username });
        setExcluded(stored);
      })
      .catch((e) => {
        console.error("Failed to load folder picker settings:", e);
        if (activeRef.current) setSaveError("Could not load excluded folders.");
      });
    return () => {
      activeRef.current = false;
    };
  }, []);

  const childrenByParent = useMemo(() => groupFoldersByParent(folders ?? []), [folders]);

  // One recursive listing covers the whole tree; every deeper expansion is
  // derived from it, so this is the only WebDAV request the picker ever makes.
  const fetchTree = async () => {
    if (!config || loadingRef.current) return;
    loadingRef.current = true;
    setLoading(true);
    setListError(null);
    try {
      const result = await invoke<RemoteFolder[]>("list_remote_folders", {
        baseUrl: config.baseUrl,
        subdir: config.subdir,
        username: config.username || undefined,
      });
      if (activeRef.current) setFolders(result);
    } catch (e: any) {
      console.error("Failed to list remote folders:", e);
      if (activeRef.current) setListError(`Could not list remote folders: ${e?.message || e}`);
    } finally {
      loadingRef.current = false;
      if (activeRef.current) setLoading(false);
    }
  };

  const toggleExpand = (path: string) => {
    if (folders === null) {
      if (!loadingRef.current) void fetchTree();
      setExpanded((prev) => new Set(prev).add(path));
      return;
    }
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const persist = async (next: string[]) => {
    if (!config) return;
    const previous = excluded;
    setExcluded(next);
    try {
      await setExcludedFolders(config.subdir, next);
      setSaveError(null);
    } catch (e) {
      console.error("Failed to save excluded folders:", e);
      setExcluded(previous);
      setSaveError("Could not save excluded folders.");
    }
  };

  const toggleExclude = (folder: string) => {
    if (excluded.includes(folder)) {
      void persist(excluded.filter((item) => item !== folder));
      return;
    }
    if (!normalizeExcludedFolder(folder)) return;
    if (excluded.length >= MAX_EXCLUDED_FOLDERS) {
      setSaveError(`At most ${MAX_EXCLUDED_FOLDERS} folders can be excluded.`);
      return;
    }
    void persist([...excluded, folder]);
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
        <span className="font-mono">{config?.subdir || "…"}</span>. Sizes total
        every file beneath a folder.
      </p>
      {config && !config.subdir && (
        <p className="mt-1 text-gray-400 font-light">Select a subdirectory first.</p>
      )}
      {config?.subdir && (
        <div
          role="tree"
          aria-label={`Remote folders under ${config.subdir}`}
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
              <span className="font-mono truncate" title={config.subdir}>
                {config.subdir}
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
          {listError} Click ▸ to retry.
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
