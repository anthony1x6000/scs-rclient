import { useState, useEffect, useRef } from "react";
import { setTargetSubdir } from "../settings";
import TextInput from "./TextInput";

interface SettingsViewProps {
  onClose: () => void;
  targetSubdir: string;
  onTargetSubdirChange: (subdir: string) => void;
}

function SettingsView({ onClose, targetSubdir, onTargetSubdirChange }: SettingsViewProps) {
  const [draft, setDraft] = useState<string>(targetSubdir);
  const [savedIndicator, setSavedIndicator] = useState<string>("");
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const onChangeRef = useRef(onTargetSubdirChange);
  onChangeRef.current = onTargetSubdirChange;

  // Sync from parent only when the parent value changes externally.
  useEffect(() => {
    setDraft(targetSubdir);
  }, [targetSubdir]);

  useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
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

  const handleChange = (newVal: string) => {
    setDraft(newVal);
    setSavedIndicator("");
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => void persist(newVal.trim()), 400);
  };

  return (
    <div role="dialog" aria-label="Settings">
      <label className="sr-only" htmlFor="scs-target-subdir">Mount root subdirectory</label>
      <TextInput
        id="scs-target-subdir"
        type="text"
        value={draft}
        onChange={(e) => handleChange(e.target.value)}
        placeholder="Mount root subdirectory..."
        className="w-[80%]"
      />
      <button
        type="button"
        onClick={onClose}
        className="w-[20%] text-center ml-2 px-2 py-1 text-xs border border-white/20 hover:border-white/40 focus:border-white/60 bg-transparent text-white outline-none cursor-pointer hover:bg-white/5 active:scale-95 transition-all text-nowrap"
      >
        Close Settings
      </button>
      <span className="text-xs text-gray-400 ml-2" role="status" aria-live="polite">{savedIndicator}</span>
    </div>
  );
}

export default SettingsView;
