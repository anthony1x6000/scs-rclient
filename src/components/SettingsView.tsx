import { useState, useEffect, useRef } from "react";
import {
  setTargetSubdir,
  getScanConcurrency,
  setScanConcurrency,
  clearWebDAVCache,
  parseAndClampScanConcurrency,
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
  const [concurrencyDraft, setConcurrencyDraft] = useState<string>(String(DEFAULT_SCAN_CONCURRENCY));
  const [subdirSaved, setSubdirSaved] = useState<boolean>(false);
  const [concurrencySaved, setConcurrencySaved] = useState<boolean>(false);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const concurrencyDebounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const onChangeRef = useRef(onTargetSubdirChange);
  onChangeRef.current = onTargetSubdirChange;

  // Load stored scan concurrency on mount
  useEffect(() => {
    let active = true;
    getScanConcurrency().then((c) => {
      if (active) setConcurrencyDraft(String(c));
    });
    return () => {
      active = false;
    };
  }, []);

  // Sync from parent only when the parent value changes externally.
  useEffect(() => {
    setDraft(targetSubdir);
    setSubdirSaved(false);
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
    try {
      await setTargetSubdir(newVal);
      onChangeRef.current(newVal);
      setSubdirSaved(true);
    } catch (e) {
      console.error("Failed to save target subdirectory setting:", e);
      setSubdirSaved(false);
    }
  };

  const persistConcurrency = async (threads: number) => {
    try {
      await setScanConcurrency(threads);
      setConcurrencySaved(true);
    } catch (e) {
      console.error("Failed to save scan concurrency setting:", e);
      setConcurrencySaved(false);
    }
  };

  const handleClearCache = async () => {
    try {
      await clearWebDAVCache();
    } catch (e) {
      console.error("Failed to clear WebDAV cache:", e);
    }
  };

  const handleChange = (newVal: string) => {
    setDraft(newVal);
    setSubdirSaved(false);
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => void persist(newVal.trim()), 400);
  };

  const handleConcurrencyChange = (raw: string) => {
    setConcurrencyDraft(raw);
    setConcurrencySaved(false);
    const clamped = parseAndClampScanConcurrency(raw);
    if (clamped !== null) {
      if (concurrencyDebounceRef.current) clearTimeout(concurrencyDebounceRef.current);
      concurrencyDebounceRef.current = setTimeout(() => void persistConcurrency(clamped), 400);
    }
  };

  const handleConcurrencyBlur = () => {
    if (concurrencyDebounceRef.current) {
      clearTimeout(concurrencyDebounceRef.current);
      concurrencyDebounceRef.current = null;
    }
    const clamped = parseAndClampScanConcurrency(concurrencyDraft);
    const finalVal = clamped ?? DEFAULT_SCAN_CONCURRENCY;
    setConcurrencyDraft(String(finalVal));
    void persistConcurrency(finalVal);
  };

  return (
    <div role="dialog" aria-label="Settings" className="flex items-center gap-3 w-full">
      <div className="flex-1 min-w-0">
        <label className="sr-only" htmlFor="scs-target-subdir">Mount root subdirectory</label>
        <TextInput
          id="scs-target-subdir"
          type="text"
          value={draft}
          onChange={(e) => handleChange(e.target.value)}
          placeholder="Mount root subdirectory..."
          status={subdirSaved ? "success" : "idle"}
          className="w-full"
        />
      </div>
      <div className="flex items-center gap-1.5 shrink-0 ml-1">
        <label htmlFor="scs-scan-concurrency" className="text-xs text-gray-300 font-light select-none">
          Threads:
        </label>
        <TextInput
          id="scs-scan-concurrency"
          type="number"
          min={MIN_SCAN_CONCURRENCY}
          max={MAX_SCAN_CONCURRENCY}
          value={concurrencyDraft}
          onChange={(e) => handleConcurrencyChange(e.target.value)}
          onBlur={handleConcurrencyBlur}
          title="Number of concurrent scanning threads (1–64)"
          status={concurrencySaved ? "success" : "idle"}
          className="w-14 text-center [appearance:textfield] [&::-webkit-outer-spin-button]:appearance-none [&::-webkit-inner-spin-button]:appearance-none"
        />
      </div>
      <button
        type="button"
        onClick={handleClearCache}
        className="shrink-0 px-2.5 py-1 text-xs border border-white/20 hover:border-amber-400/60 hover:text-amber-200 focus:border-amber-400/60 bg-transparent text-gray-300 outline-none cursor-pointer active:scale-95 transition-all duration-150 text-nowrap"
        title="Clear cached remote WebDAV file listings"
      >
        Clear Cache
      </button>
      <button
        type="button"
        onClick={onClose}
        className="shrink-0 px-3 py-1 text-xs border border-white/20 hover:border-white/50 focus:border-white/60 bg-transparent text-white outline-none cursor-pointer active:scale-95 transition-all duration-150 text-nowrap"
      >
        Close Settings
      </button>
    </div>
  );
}

export default SettingsView;


