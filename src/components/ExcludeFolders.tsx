import { useEffect, useRef, useState } from "react";
import {
  getExcludedFolders,
  setExcludedFolders,
  normalizeExcludedFolder,
  MAX_EXCLUDED_FOLDERS,
} from "../settings";
import TextInput from "./TextInput";

/**
 * Editor for the persisted list of folders that scan/transfer actions must skip.
 * Patterns are stored relative to the selected remote subdirectory and applied by
 * the Rust backend to both local walks and remote listings.
 */
export function ExcludeFolders() {
  const [folders, setFolders] = useState<string[]>([]);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const activeRef = useRef(true);

  useEffect(() => {
    activeRef.current = true;
    getExcludedFolders()
      .then((stored) => {
        if (activeRef.current) setFolders(stored);
      })
      .catch((e) => {
        console.error("Failed to load excluded folders:", e);
        if (activeRef.current) setError("Could not load excluded folders.");
      })
      .finally(() => {
        if (activeRef.current) setLoaded(true);
      });
    return () => {
      activeRef.current = false;
    };
  }, []);

  const persist = async (next: string[]) => {
    const previous = folders;
    setFolders(next);
    setSaving(true);
    try {
      await setExcludedFolders(next);
      setError(null);
    } catch (e) {
      console.error("Failed to save excluded folders:", e);
      setFolders(previous);
      setError("Could not save excluded folders.");
    } finally {
      setSaving(false);
    }
  };

  const add = () => {
    const folder = normalizeExcludedFolder(draft);
    if (!folder) {
      setError("Enter a folder like .git/ (no . or .. segments).");
      return;
    }
    if (folders.includes(folder)) {
      setError(`${folder}/ is already excluded.`);
      return;
    }
    if (folders.length >= MAX_EXCLUDED_FOLDERS) {
      setError(`At most ${MAX_EXCLUDED_FOLDERS} folders can be excluded.`);
      return;
    }
    setDraft("");
    void persist([...folders, folder]);
  };

  const remove = (folder: string) => {
    void persist(folders.filter((item) => item !== folder));
  };

  return (
    <div id="scs-exclude-folders" className="px-2 pb-1 text-xs text-white">
      <div className="flex flex-wrap items-center gap-2">
        <label
          htmlFor="scs-exclude-folder-input"
          className="text-gray-300 font-light select-none"
        >
          Exclude folder:
        </label>
        <TextInput
          id="scs-exclude-folder-input"
          type="text"
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
          placeholder=".git/"
          status={error ? "error" : "idle"}
          className="flex-1 min-w-40"
          aria-describedby="scs-exclude-folder-hint"
        />
        <button
          type="button"
          disabled={saving || !loaded}
          onClick={add}
          className="px-2 py-1 border border-white/20 hover:border-white/50 bg-transparent text-white cursor-pointer transition-all duration-150 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          Add
        </button>
      </div>
      <p
        id="scs-exclude-folder-hint"
        className="mt-1 text-[10px] text-gray-400 font-light"
      >
        Excluded folders are not scanned, uploaded, downloaded, or deleted by sync.
        A name like <code>.git/</code> matches at any depth; a path like{" "}
        <code>assets/cache</code> matches that relative folder.
      </p>
      {error && (
        <p role="alert" className="mt-1 text-red-300">
          {error}
        </p>
      )}
      {loaded && folders.length > 0 && (
        <ul
          className="flex flex-wrap gap-1 mt-1 list-none m-0 p-0"
          aria-label="Excluded folders"
        >
          {folders.map((folder) => (
            <li
              key={folder}
              className="flex items-center gap-1 border border-white/20 bg-black/25 px-2 py-0.5"
            >
              <span className="font-mono">{folder}/</span>
              <button
                type="button"
                onClick={() => remove(folder)}
                aria-label={`Remove excluded folder ${folder}`}
                title="Remove"
                className="text-gray-400 hover:text-red-300 cursor-pointer transition-colors duration-150"
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}
      {loaded && folders.length === 0 && (
        <p className="mt-1 text-gray-400 font-light">No folders excluded.</p>
      )}
    </div>
  );
}

export default ExcludeFolders;
