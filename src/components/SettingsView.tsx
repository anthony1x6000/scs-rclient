import { useState, useEffect, useRef } from "react";
import {
  setTargetSubdir,
  getScanConcurrency,
  setScanConcurrency,
  clearWebDAVCache,
  clampScanConcurrency,
  DEFAULT_SCAN_CONCURRENCY,
  MIN_SCAN_CONCURRENCY,
  MAX_SCAN_CONCURRENCY,
} from "../settings";
import TextInput from "./TextInput";

interface SettingsViewProps {
  onClose: () => void;
  targetSubdir: string;
  onTargetSubdirChange: (subdir: string) => void;
}

function SettingsView({ onClose, targetSubdir, onTargetSubdirChange }: SettingsViewProps) {
  const [draft, setDraft] = useState<string>(targetSubdir);
  const [concurrencyDraft, setConcurrencyDraft] = useState<number | string>(DEFAULT_SCAN_CONCURRENCY);
  const [savedIndicator, setSavedIndicator] = useState<string>("");
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const concurrencyDebounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const onChangeRef = useRef(onTargetSubdirChange);
  onChangeRef.current = onTargetSubdirChange;

  // Load stored scan concurrency on mount
  useEffect(() => {
    let active = true;
    getScanConcurrency().then((c) => {
      if (active) setConcurrencyDraft(c);
    });
    return () => {
      active = false;
    };
  }, []);

  // Sync from parent only when the parent value changes externally.
  useEffect(() => {
    setDraft(targetSubdir);
  }, [targetSubdir]);

  useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      if (concurrencyDebounceRef.current) clearTimeout(concurrencyDebounceRef.current);
    };
  }, []);

  useEffect(() => {
    const handleEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handleEsc);
    return () => window.removeEventListener("keydown", handleEsc);
  }, [onClose]);

  const persist = async (newVal: string) => {
    setSavedIndicator("Saving…");
    try {
      await setTargetSubdir(newVal);
      onChangeRef.current(newVal);
      setSavedIndicator("Saved.");
    } catch (e) {
      console.error("Failed to save target subdirectory setting:", e);
      setSavedIndicator("Save failed.");
    }
  };

  const persistConcurrency = async (threads: number) => {
    setSavedIndicator("Saving…");
    try {
      await setScanConcurrency(threads);
      setSavedIndicator("Saved.");
    } catch (e) {
      console.error("Failed to save scan concurrency setting:", e);
      setSavedIndicator("Save failed.");
    }
  };

  const handleClearCache = async () => {
    setSavedIndicator("Clearing…");
    try {
      await clearWebDAVCache();
      setSavedIndicator("Cache cleared.");
    } catch (e) {
      console.error("Failed to clear WebDAV cache:", e);
      setSavedIndicator("Clear failed.");
    }
  };

  const handleChange = (newVal: string) => {
    setDraft(newVal);
    setSavedIndicator("");
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => void persist(newVal.trim()), 400);
  };

  const handleConcurrencyChange = (raw: string) => {
    setConcurrencyDraft(raw);
    const parsed = parseInt(raw, 10);
    if (!isNaN(parsed)) {
      const clamped = clampScanConcurrency(parsed);
      setSavedIndicator("");
      if (concurrencyDebounceRef.current) clearTimeout(concurrencyDebounceRef.current);
      concurrencyDebounceRef.current = setTimeout(() => void persistConcurrency(clamped), 400);
    }
  };

  return (
    <div role="dialog" aria-label="Settings" className="flex items-center gap-2 w-full">
      <div className="flex-1 min-w-0">
        <label className="sr-only" htmlFor="scs-target-subdir">Mount root subdirectory</label>
        <TextInput
          id="scs-target-subdir"
          type="text"
          value={draft}
          onChange={(e) => handleChange(e.target.value)}
          placeholder="Mount root subdirectory..."
          className="w-full"
        />
      </div>
      <div className="flex items-center gap-1.5 shrink-0">
        <label htmlFor="scs-scan-concurrency" className="text-xs text-gray-300 font-light select-none">
          Threads:
        </label>
        <input
          id="scs-scan-concurrency"
          type="number"
          min={MIN_SCAN_CONCURRENCY}
          max={MAX_SCAN_CONCURRENCY}
          value={concurrencyDraft}
          onChange={(e) => handleConcurrencyChange(e.target.value)}
          title="Number of concurrent scanning threads (1–64)"
          className="w-14 px-1.5 py-1 text-xs text-center border border-white/20 bg-black/40 text-white rounded outline-none focus:border-white/60 [appearance:textfield] [&::-webkit-outer-spin-button]:appearance-none [&::-webkit-inner-spin-button]:appearance-none"
        />
      </div>
      <button
        type="button"
        onClick={handleClearCache}
        className="shrink-0 px-2.5 py-1 text-xs border border-white/20 hover:border-amber-400/40 hover:text-amber-200 focus:border-amber-400/60 bg-transparent text-gray-300 outline-none cursor-pointer hover:bg-amber-500/10 active:scale-95 transition-all text-nowrap"
        title="Clear cached remote WebDAV file listings"
      >
        Clear Cache
      </button>
      <button
        type="button"
        onClick={onClose}
        className="shrink-0 px-3 py-1 text-xs border border-white/20 hover:border-white/40 focus:border-white/60 bg-transparent text-white outline-none cursor-pointer hover:bg-white/5 active:scale-95 transition-all text-nowrap"
      >
        Close Settings
      </button>
      <span className="text-xs text-gray-400 shrink-0 min-w-[45px]" role="status" aria-live="polite">
        {savedIndicator}
      </span>
    </div>
  );
}

export default SettingsView;


