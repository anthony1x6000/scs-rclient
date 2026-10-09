import { useState } from "react";
import { useRcloneExecution, type RcloneActionType } from "../hooks/useRcloneExecution";

interface RcloneActionsProps {
  onLog: (text: string | ((prev: string) => string)) => void;
  isRunning: boolean;
  setIsRunning: (running: boolean) => void;
}

interface ActionDef {
  id: RcloneActionType;
  label: string;
}

const ACTIONS: ActionDef[] = [
  { id: "put", label: "Upload (Local to Remote)" },
  { id: "get", label: "Download (Remote to Local)" },
  { id: "put-dry", label: "Preview Upload (Dry Run)" },
  { id: "get-dry", label: "Preview Download (Dry Run)" },
  { id: "check", label: "Compare Local vs Remote" },
  { id: "ls", label: "List Remote" },
  { id: "get-backup-dated", label: "Backup" },
];

const ACTION_BUTTON_CLASS =
  "w-full bg-gray-800/25 border border-white/30 hover:border-white/70 text-white p-3 cursor-pointer transition-all duration-150 disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline focus-visible:outline-2 focus-visible:outline-white";

export function RcloneActions({ onLog, isRunning, setIsRunning }: RcloneActionsProps) {
  const { runRclone, cancelCommand } = useRcloneExecution(onLog, isRunning, setIsRunning);
  const [overwriteArmed, setOverwriteArmed] = useState(false);

  const runAction = (action: RcloneActionType) => {
    if (isRunning) return;
    setOverwriteArmed(false);
    runRclone(action);
  };

  const handleOverwriteClick = () => {
    if (isRunning) return;
    if (!overwriteArmed) {
      setOverwriteArmed(true);
      return;
    }
    setOverwriteArmed(false);
    runRclone("sync");
  };

  return (
    <div className="p-2">
      <ul className="grid grid-cols-2 gap-2 list-none m-0 p-0">
        {ACTIONS.map((action) => (
          <li key={action.id} className="p-0 text-center">
            <button
              type="button"
              disabled={isRunning}
              onClick={() => runAction(action.id)}
              className={ACTION_BUTTON_CLASS}
            >
              {action.label}
            </button>
          </li>
        ))}
        <li className="p-0 text-center col-span-2">
          <button
            type="button"
            disabled={isRunning}
            onClick={handleOverwriteClick}
            className={ACTION_BUTTON_CLASS}
          >
            {overwriteArmed
              ? "Overwrite will DELETE remote files missing locally. Press again to confirm."
              : "Overwrite remote"}
          </button>
        </li>
      </ul>
      {isRunning && (
        <div className="mt-2">
          <button
            type="button"
            onClick={cancelCommand}
            className="w-full bg-red-950/25 border border-red-500/50 hover:border-red-400 text-red-400 p-3 text-center cursor-pointer transition-all duration-150 active:scale-[0.99] select-none"
          >
            Cancel operation
          </button>
        </div>
      )}
    </div>
  );
}
