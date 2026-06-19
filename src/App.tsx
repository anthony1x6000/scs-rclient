import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import Dropdown from "./components/Dropdown";
import BackgroundWrapper from "./BackgroundWrapper";
import BaseWebDAVURL from "./components/BaseWebDAVUrl";
import CommandExecutor from "./components/CommandExecutor";

function App() {
  const [mountDir, setMountDir] = useState<string>("");

  useEffect(() => {
    invoke<string>("get_mount_dir")
      .then(setMountDir)
      .catch(console.error);
  }, []);

  return (
    <BackgroundWrapper>

      <div className="bottom-0 absolute p-[2%] w-[100%] text-white flex flex-col gap-4">
        <div className="p-[2%] font-['Roboto'] font-light">
          <div className="text-nowrap">
          <Dropdown />
          <div className="inline italic">
            a subdirectory of your WebDAV drive
          </div>
          </div>
          {mountDir && (
            <div className="text-[10px] text-gray-400 font-mono mt-1 opacity-70">
              mount directory: {mountDir}
            </div>
          )}
        </div>
        <CommandExecutor />
        <BaseWebDAVURL />
      </div>
    </BackgroundWrapper>
  );
}

export default App;
