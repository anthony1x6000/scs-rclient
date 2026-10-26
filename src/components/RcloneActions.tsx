import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useRcloneExecution, type RcloneActionType } from "../hooks/useRcloneExecution";
import { BULK_MIN_FOLDERS } from "../bulk";

interface RcloneActionsProps {
  onLog: (text: string | ((prev: string) => string)) => void;
  isRunning: boolean;
  setIsRunning: (running: boolean) => void;
  /** Ticked subfolders; 2+ turns every action into a sequential bulk run. */
  bulkFolders?: string[];
  /** Fired when an action actually runs on a bulk selection (2+ folders). */
  onBulkRun?: (folders: string[]) => void;
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

export function RcloneActions({
  onLog,
  isRunning,
  setIsRunning,
  bulkFolders = [],
  onBulkRun,
}: RcloneActionsProps) {
  const { runRclone, cancelCommand } = useRcloneExecution(onLog, isRunning, setIsRunning);
  const [overwriteArmed, setOverwriteArmed] = useState(false);
  const uploadButtonRef = useRef<HTMLButtonElement>(null);
  const isBulk = bulkFolders.length >= BULK_MIN_FOLDERS;
  const bulkSuffix = isBulk ? ` · ${bulkFolders.length} folders` : "";
  const runFolders = bulkFolders.length > 0 ? { folders: bulkFolders } : undefined;

  // The webview starts sequential Tab navigation at the first control without ever
  // focusing it in the DOM, so the first Tab press lands on Download, not Upload
  // (blurring activeElement cannot fix this — the parked position is not DOM focus).
  // Instead, grab the first bare Tab press and focus Upload explicitly.
  //
  // That shim must also cover coming back to the window: the webview keeps the last
  // clicked control as the Tab origin, so restoring from the taskbar resumed after
  // whatever was clicked before. Re-arm and drop the stale control whenever the window
  // loses focus or is minimized/hidden, making every refocus start from Upload again.
  // A fresh pointer interaction disarms it so the user's new click becomes the origin.
  useEffect(() => {
    let armed = true;
    let disposed = false;
    let unlistenBlur: (() => void) | undefined;

    const resetForRefocus = () => {
      armed = true;
      const active = document.activeElement;
      if (active instanceof HTMLElement && active !== document.body) {
        active.blur();
      }
    };

    const disarm = () => {
      armed = false;
    };

    const onKeyDown = (e: KeyboardEvent) => {
      if (!armed) return;
      if (e.key !== "Tab" || e.shiftKey || e.ctrlKey || e.metaKey || e.altKey) {
        armed = false;
        return;
      }
      const upload = uploadButtonRef.current;
      armed = false;
      if (!upload || upload.disabled) return;
      e.preventDefault();
      upload.focus();
    };

    const onVisibilityChange = () => {
      if (document.visibilityState !== "visible") resetForRefocus();
    };

    // The native window event is the reliable signal for "the app went away" on
    // WebView2/WebKitGTK; the DOM events cover browsers and webviews that forward them.
    try {
      void getCurrentWindow()
        .onFocusChanged(({ payload: focused }) => {
          if (!focused) resetForRefocus();
        })
        .then((unlisten) => {
          if (disposed) unlisten();
          else unlistenBlur = unlisten;
        })
        .catch(() => {});
    } catch {
      // No Tauri runtime (plain browser); the DOM listeners below still apply.
    }

    window.addEventListener("blur", resetForRefocus);
    document.addEventListener("visibilitychange", onVisibilityChange);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("pointerdown", disarm, true);
    return () => {
      disposed = true;
      unlistenBlur?.();
      window.removeEventListener("blur", resetForRefocus);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("pointerdown", disarm, true);
    };
  }, []);

  const runAction = (action: RcloneActionType) => {
    if (isRunning) return;
    setOverwriteArmed(false);
    if (isBulk) onBulkRun?.(bulkFolders);
    runRclone(action, runFolders);
  };

  const handleOverwriteClick = () => {
    if (isRunning) return;
    if (!overwriteArmed) {
      setOverwriteArmed(true);
      return;
    }
    setOverwriteArmed(false);
    if (isBulk) onBulkRun?.(bulkFolders);
    runRclone("sync", runFolders);
  };

  return (
    <div className="p-2">
      <ul className="grid grid-cols-2 gap-2 list-none m-0 p-0">
        {ACTIONS.map((action, index) => (
          <li key={action.id} className="p-0 text-center">
            <button
              ref={index === 0 ? uploadButtonRef : undefined}
              type="button"
              disabled={isRunning}
              onClick={() => runAction(action.id)}
              className={ACTION_BUTTON_CLASS}
            >
              {action.label}
              {bulkSuffix}
            </button>
          </li>
        ))}
        <li className="p-0 text-center">
          <button
            type="button"
            disabled={isRunning}
            onClick={handleOverwriteClick}
            className={ACTION_BUTTON_CLASS}
          >
            {overwriteArmed
              ? `Press again to confirm overwrite${isBulk ? ` (${bulkFolders.length} folders)` : ""}`
              : `Overwrite remote${bulkSuffix}`}
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
