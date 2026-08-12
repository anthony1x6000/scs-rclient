import { useState, useEffect } from "react";
import { load } from "@tauri-apps/plugin-store";
import TextInput from "./TextInput";

interface SettingsViewProps {
  onClose: () => void;
}

function SettingsView({ onClose }: SettingsViewProps) {
  const [targetSubdir, setTargetSubdir] = useState<string>("");

  useEffect(() => {
    async function loadSettings() {
      try {
        const store = await load("settings.json", { autoSave: true, defaults: {} });
        const savedSubdir =
          (await store.get<{ value: string }>("target_subdirectory")) ||
          (await store.get<{ value: string }>("test_subdirectory"));
        if (savedSubdir && typeof savedSubdir.value === "string") {
          setTargetSubdir(savedSubdir.value);
        } else {
          setTargetSubdir("");
        }
      } catch (e) {
        console.error("Failed to load target subdirectory setting:", e);
      }
    }
    loadSettings();
  }, []);

  const handleChange = async (newVal: string) => {
    setTargetSubdir(newVal);
    try {
      const store = await load("settings.json", { autoSave: true, defaults: {} });
      await store.set("target_subdirectory", { value: newVal });
    } catch (e) {
      console.error("Failed to save target subdirectory setting:", e);
    }
  };

  return (
    <>
      <TextInput
        type="text"
        value={targetSubdir}
        onChange={(e) => handleChange(e.target.value)}
        placeholder="Target Subdirectory..."
        className="w-[80%]"
      />
      <button
        type="button"
        onClick={onClose}
        className="w-[20%] text-center ml-2 px-2 py-1 text-xs border border-white/20 hover:border-white/40 focus:border-white/60 bg-transparent text-white outline-none cursor-pointer hover:bg-white/5 active:scale-95 transition-all text-nowrap"
      >
        Close Settings
      </button>
    </>
  );
}

export default SettingsView;
