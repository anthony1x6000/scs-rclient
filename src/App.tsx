import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import Dropdown from "./components/Dropdown";
import BackgroundWrapper from "./BackgroundWrapper";
import BaseWebDAVURL from "./components/BaseWebDAVUrl";
import CredentialsForm from "./components/CredentialsForm";
import SettingsView from "./components/SettingsView";
import { RcloneActions } from "./components/RcloneActions";
import RcloneConsole from "./components/RcloneConsole";
import { getTargetSubdir } from "./settings";

function App() {
  const [mountDir, setMountDir] = useState<string>("");
  const [targetSubdir, setTargetSubdir] = useState<string>("");
  const [selectedSubdir, setSelectedSubdir] = useState<string>("");
  const [showSettings, setShowSettings] = useState<boolean>(false);
  const [logs, setLogs] = useState<string>("");
  const [isRunning, setIsRunning] = useState<boolean>(false);
  const mountSeq = useRef(0);

  const updateMountDir = async (subdir?: string) => {
    const seq = ++mountSeq.current;
    try {
      const resolved = await invoke<string>("get_mount_dir", {
        targetSubdir: subdir ? subdir : undefined,
      });
      if (mountSeq.current === seq) setMountDir(resolved);
    } catch (e) {
      console.error("Failed to get mount dir:", e);
    }
  };

  useEffect(() => {
    // Fetch target subdirectory from the single settings module.
    getTargetSubdir()
      .then((sub) => {
        setTargetSubdir(sub);
        updateMountDir(sub);
      })
      .catch((e) => {
        console.error(e);
        updateMountDir();
      });
  }, []);

  useEffect(() => {
    const trimmed = selectedSubdir.trim();
    const title = trimmed
      ? `scs-rclient: ${trimmed.endsWith("/") ? trimmed : `${trimmed}/`}`
      : "scs-rclient";

    document.title = title;

    import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => {
        return getCurrentWindow().setTitle(title);
      })
      .catch((err) => {
        console.warn("Could not set native window title:", err);
      });
  }, [selectedSubdir]);

  const handleTargetSubdirChange = (newSub: string) => {
    setTargetSubdir(newSub);
    void updateMountDir(newSub);
  };

  return (
    <BackgroundWrapper>
      <div className="flex flex-col h-screen w-full box-border overflow-hidden p-2 justify-between">
        <div className="shrink-0">
          <RcloneActions
            onLog={setLogs}
            isRunning={isRunning}
            setIsRunning={setIsRunning}
          />
        </div>

        <div className="flex-1 min-h-0 flex flex-col overflow-hidden my-1 hide-on-compact-height">
          <RcloneConsole logs={logs} onClear={() => setLogs("")} />
        </div>

        <div className="shrink-0 text-white flex flex-col gap-2 w-full">
          <div className="directory-pane px-2 font-light hide-on-short-height">
            <div className="flex items-baseline justify-between gap-4 w-full min-w-0">
              <Dropdown onSelect={setSelectedSubdir} />
              <div
                className="italic truncate shrink-[9999] min-w-0 ml-auto"
                title="a subdirectory of your WebDAV drive"
              >
                a subdirectory of your WebDAV drive
              </div>
            </div>
            {mountDir && (
              <div className="text-[10px] text-gray-400 mt-1 opacity-70">
                Mount directory: {mountDir}
              </div>
            )}
          </div>
          <div className="settings-pane flex items-center gap-2 w-full hide-on-compact-height">
            <div className={showSettings ? "hidden" : "flex items-center gap-2 w-full"}>
              <CredentialsForm />
              <BaseWebDAVURL />
              <button
                type="button"
                onClick={() => setShowSettings(true)}
                className="shrink-0 px-3 py-1 text-xs border border-white/20 hover:border-white/50 focus:border-white/60 bg-transparent text-white outline-none cursor-pointer active:scale-95 transition-all duration-150 text-nowrap"
              >
                Settings
              </button>
            </div>
            {showSettings && (
              <SettingsView
                onClose={() => setShowSettings(false)}
                targetSubdir={targetSubdir}
                onTargetSubdirChange={handleTargetSubdirChange}
              />
            )}
          </div>
        </div>
      </div>
    </BackgroundWrapper>
  );
}

export default App;
