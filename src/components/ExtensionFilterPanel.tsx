import { useEffect, useRef, useState } from "react";
import {
  getExtensionFilterState,
  setGlobalExtensionFilter,
  setSubdirExtensionFilter,
} from "../settings";
import {
  formatExtensionCsv,
  parseExtensionCsv,
  resolveExtensionFilter,
  subdirKey,
  type ExtensionFilterMode,
  type ExtensionFilterState,
} from "../extensionFilter";
import TextInput from "./TextInput";

interface ExtensionFilterPanelProps {
  mode: ExtensionFilterMode;
  selectedSubdir: string;
}

const MODE_LABEL: Record<ExtensionFilterMode, string> = {
  whitelist: "Whitelist extension",
  blacklist: "Blacklist extension",
};

const MODE_HINT: Record<ExtensionFilterMode, string> = {
  whitelist: "Only the listed file types are scanned and transferred; everything else is skipped.",
  blacklist: "The listed file types are skipped; everything else is scanned and transferred.",
};

/**
 * Editor for one mode of the persisted extension filters. Confirming writes either a
 * per-dir list (this dir only) or the global list (all dirs); confirming an empty
 * CSV clears that scope. Resolution happens per folder at action time, so bulk runs
 * use each folder's own list.
 */
export function ExtensionFilterPanel({ mode, selectedSubdir }: ExtensionFilterPanelProps) {
  const [snapshot, setSnapshot] = useState<ExtensionFilterState | null>(null);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const activeRef = useRef(true);

  const other: ExtensionFilterMode = mode === "whitelist" ? "blacklist" : "whitelist";

  useEffect(() => {
    activeRef.current = true;
    getExtensionFilterState()
      .then((state) => {
        if (!activeRef.current) return;
        setSnapshot(state);
        // Prefill with the list currently in effect for this dir (per-dir, else global).
        const winner = resolveExtensionFilter(selectedSubdir, state);
        setDraft(winner && winner.mode === mode ? formatExtensionCsv(winner.extensions) : "");
      })
      .catch((e) => {
        console.error("Failed to load extension filters:", e);
        if (activeRef.current) setError("Could not load extension filters.");
      });
    return () => {
      activeRef.current = false;
    };
    // The panel remounts on every open and on every selected-dir change (keyed in
    // RcloneConsole), so loading once per mount is enough and the draft can never
    // belong to a different dir than the one "Confirm for this dir" will save.
  }, []);

  const dirKey = subdirKey(selectedSubdir);
  const perDirList = snapshot
    ? dirKey
      ? (mode === "whitelist"
          ? snapshot.perDirWhitelist[dirKey]
          : snapshot.perDirBlacklist[dirKey])
      : undefined
    : undefined;
  const globalList = snapshot
    ? mode === "whitelist"
      ? snapshot.globalWhitelist
      : snapshot.globalBlacklist
    : [];
  const otherWinner =
    snapshot && resolveExtensionFilter(selectedSubdir, snapshot)?.mode === other;
  const effectiveForDir = perDirList ?? globalList;

  const confirm = async (scope: "dir" | "all") => {
    const { extensions, invalid } = parseExtensionCsv(draft);
    if (invalid.length > 0) {
      setError(`Not a file extension: ${invalid.join(", ")}`);
      return;
    }
    setSaving(true);
    try {
      if (scope === "dir") {
        await setSubdirExtensionFilter(mode, selectedSubdir, extensions);
      } else {
        await setGlobalExtensionFilter(mode, extensions);
      }
      const next = await getExtensionFilterState();
      setSnapshot(next);
      // Show the list exactly as saved for this scope, even when the other mode
      // wins resolution, so a second Confirm cannot accidentally clear it.
      setDraft(formatExtensionCsv(extensions));
      setError(null);
    } catch (e) {
      console.error("Failed to save extension filter:", e);
      setError("Could not save the extension filter.");
    } finally {
      setSaving(false);
    }
  };

  const panelId = `scs-extension-${mode}`;
  const inputId = `scs-extension-${mode}-input`;
  const hintId = `scs-extension-${mode}-hint`;
  const dirDisabled = saving || !snapshot || !selectedSubdir.trim();

  return (
    <div id={panelId} className="px-2 pb-1 text-xs text-white">
      <div className="flex flex-wrap items-center gap-2">
        <label htmlFor={inputId} className="text-gray-300 font-light select-none">
          {MODE_LABEL[mode]}:
        </label>
        <TextInput
          id={inputId}
          type="text"
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !dirDisabled) {
              e.preventDefault();
              void confirm("dir");
            }
          }}
          placeholder=".html,.txt"
          status={error ? "error" : "idle"}
          className="flex-1 min-w-40"
          aria-describedby={hintId}
        />
        <button
          type="button"
          disabled={dirDisabled}
          onClick={() => void confirm("dir")}
          title={selectedSubdir.trim() ? `Save for ${selectedSubdir.trim()} only` : "Select a subdirectory first"}
          className="px-2 py-1 border border-white/20 hover:border-white/50 bg-transparent text-white cursor-pointer transition-all duration-150 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          Confirm for this dir
        </button>
        <button
          type="button"
          disabled={saving || !snapshot}
          onClick={() => void confirm("all")}
          title="Save as the default for every subdirectory"
          className="px-2 py-1 border border-white/20 hover:border-white/50 bg-transparent text-white cursor-pointer transition-all duration-150 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          Confirm for all dirs
        </button>
      </div>
      <p id={hintId} className="mt-1 text-[10px] text-gray-400 font-light">
        {MODE_HINT[mode]} Entries are comma-separated, case-insensitive, and work with or
        without the dot (<code>docx</code> and <code>.docx</code> are the same). A list saved
        for this dir replaces the all-dirs list for{" "}
        <code>{selectedSubdir.trim() || "the selected dir"}</code>; bulk runs use each
        folder's own list.
      </p>
      <div className="mt-1 text-[10px] font-light">
        {snapshot && (
          <p className="text-gray-400">
            This dir ({selectedSubdir.trim() || "none selected"}):{" "}
            {perDirList && perDirList.length > 0 ? (
              <span className="font-mono">{formatExtensionCsv(perDirList)}</span>
            ) : (
              "no per-dir list"
            )}
            {" · "}All dirs:{" "}
            {globalList.length > 0 ? (
              <span className="font-mono">{formatExtensionCsv(globalList)}</span>
            ) : (
              "none"
            )}
          </p>
        )}
        {snapshot && otherWinner && effectiveForDir && effectiveForDir.length > 0 && (
          <p className="text-amber-200/80">
            This {mode} is ignored while a {other} is active for this dir.
          </p>
        )}
        {!selectedSubdir.trim() && (
          <p className="text-amber-200/80">Select a subdirectory to save a per-dir list.</p>
        )}
      </div>
      {error && (
        <p role="alert" className="mt-1 text-red-300">
          {error}
        </p>
      )}
    </div>
  );
}

export default ExtensionFilterPanel;
