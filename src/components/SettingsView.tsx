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
import type { UpdateState } from "../updater";

interface SettingsViewProps {
  onClose: () => void;
  targetSubdir: string;
  onTargetSubdirChange: (subdir: string) => void;
  updater: {
    state: UpdateState;
    currentVersion: string;
    checkUpdates: (interactive?: boolean) => Promise<void>;
    install: () => Promise<void>;
  };
}

function getCacheButtonClass(status: 'idle' | 'clearing' | 'cleared' | 'error'): string {
  const base = "shrink-0 px-2.5 py-1 text-xs border bg-transparent outline-none cursor-pointer active:scale-95 transition-all duration-150 text-nowrap";
  switch (status) {
    case 'cleared':
      return `${base} border-emerald-500 text-emerald-200`;
    case 'error':
      return `${base} border-red-500 text-red-300`;
    case 'clearing':
      return `${base} border-amber-400/60 text-amber-200`;
    default:
      return `${base} border-white/20 hover:border-amber-400/60 hover:text-amber-200 focus:border-amber-400/60 text-gray-300`;
  }
}

function getCacheButtonLabel(status: 'idle' | 'clearing' | 'cleared' | 'error'): string {
  switch (status) {
    case 'clearing':
      return 'Clearing…';
    case 'cleared':
      return 'Cache cleared';
    case 'error':
      return 'Clear failed';
    default:
      return 'Clear Cache';
  }
}

function getUpdateButtonClass(state: UpdateState): string {
  const base = "shrink-0 px-2.5 py-1 text-xs border bg-transparent outline-none cursor-pointer active:scale-95 transition-all duration-150 text-nowrap";
  switch (state.status) {
    case 'uptodate':
      return `${base} border-emerald-500 text-emerald-200`;
    case 'available':
      return `${base} border-emerald-500 text-emerald-200 bg-emerald-950/40 hover:border-emerald-400`;
    case 'error':
      return `${base} border-red-500 text-red-300`;
    case 'checking':
    case 'downloading':
    case 'installing':
    case 'restarting':
      return `${base} border-amber-400/60 text-amber-200`;
    default:
      return `${base} border-white/20 hover:border-white/50 focus:border-white/60 text-gray-300`;
  }
}

function getUpdateButtonLabel(state: UpdateState, currentVersion: string): string {
  switch (state.status) {
    case 'checking':
      return 'Checking…';
    case 'uptodate':
      return `Up to date (v${state.version || currentVersion})`;
    case 'available':
      return `Install v${state.info.version}`;
    case 'downloading':
      return state.progress.percentage != null
        ? `Downloading ${state.progress.percentage}%…`
        : 'Downloading…';
    case 'installing':
      return 'Installing…';
    case 'restarting':
      return 'Restarting…';
    case 'error':
      return 'Check failed';
    default:
      return 'Check for Updates';
  }
}

function SettingsView({ onClose, targetSubdir, onTargetSubdirChange, updater }: SettingsViewProps) {
  const [draft, setDraft] = useState<string>(targetSubdir);
  const [concurrencyDraft, setConcurrencyDraft] = useState<string>(String(DEFAULT_SCAN_CONCURRENCY));
  const [subdirStatus, setSubdirStatus] = useState<'idle' | 'success' | 'error'>('idle');
  const [concurrencyStatus, setConcurrencyStatus] = useState<'idle' | 'success' | 'error'>('idle');
  const [cacheStatus, setCacheStatus] = useState<'idle' | 'clearing' | 'cleared' | 'error'>('idle');
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
    setSubdirStatus('idle');
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
      setSubdirStatus('success');
    } catch (e) {
      console.error("Failed to save target subdirectory setting:", e);
      setSubdirStatus('error');
    }
  };

  const persistConcurrency = async (threads: number) => {
    try {
      await setScanConcurrency(threads);
      setConcurrencyStatus('success');
    } catch (e) {
      console.error("Failed to save scan concurrency setting:", e);
      setConcurrencyStatus('error');
    }
  };

  const handleClearCache = async () => {
    setCacheStatus('clearing');
    try {
      await clearWebDAVCache();
      setCacheStatus('cleared');
      setTimeout(() => setCacheStatus('idle'), 2000);
    } catch (e) {
      console.error("Failed to clear WebDAV cache:", e);
      setCacheStatus('error');
      setTimeout(() => setCacheStatus('idle'), 3000);
    }
  };

  const handleChange = (newVal: string) => {
    setDraft(newVal);
    setSubdirStatus('idle');
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => void persist(newVal.trim()), 400);
  };

  const handleConcurrencyChange = (raw: string) => {
    setConcurrencyDraft(raw);
    setConcurrencyStatus('idle');
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

  const isUpdateBusy =
    updater.state.status === 'checking' ||
    updater.state.status === 'downloading' ||
    updater.state.status === 'installing' ||
    updater.state.status === 'restarting';

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
          status={subdirStatus}
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
          status={concurrencyStatus}
          className="w-14 text-center [appearance:textfield] [&::-webkit-outer-spin-button]:appearance-none [&::-webkit-inner-spin-button]:appearance-none"
        />
      </div>
      <button
        type="button"
        disabled={cacheStatus === 'clearing'}
        onClick={handleClearCache}
        className={getCacheButtonClass(cacheStatus)}
        title="Clear cached remote WebDAV file listings"
      >
        {getCacheButtonLabel(cacheStatus)}
      </button>
      <button
        type="button"
        disabled={isUpdateBusy}
        onClick={() => {
          if (updater.state.status === 'available') {
            void updater.install();
          } else {
            void updater.checkUpdates(true);
          }
        }}
        className={getUpdateButtonClass(updater.state)}
        title={
          updater.state.status === 'available'
            ? `Release notes for v${updater.state.info.version}:\n${updater.state.info.body || 'No release notes.'}`
            : updater.state.status === 'error'
            ? `Error: ${updater.state.message}`
            : 'Check for updates from GitHub releases'
        }
      >
        {getUpdateButtonLabel(updater.state, updater.currentVersion)}
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


