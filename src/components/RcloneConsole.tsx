import { useEffect, useRef, useState, useLayoutEffect } from "react";
import ExcludeFolders from "./ExcludeFolders";
import ExtensionFilterPanel from "./ExtensionFilterPanel";
import ExcludeFolderSelect from "./ExcludeFolderSelect";

interface RcloneConsoleProps {
  logs: string;
  onClear?: () => void;
  onOpenFolder?: () => void;
  /** Live remote selection; exclusion panels remount when it changes. */
  selectedSubdir?: string;
}

/** Which inline editor panel is open under the button row (one at a time). */
type OpenPanel = "exclude" | "excludeSelect" | "whitelist" | "blacklist" | null;

export function RcloneConsole({ logs, onClear, onOpenFolder, selectedSubdir = "" }: RcloneConsoleProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const [copied, setCopied] = useState(false);
  const [openPanel, setOpenPanel] = useState<OpenPanel>(null);
  const [hasOverflow, setHasOverflow] = useState(false);
  const [atTop, setAtTop] = useState(true);
  const [atBottom, setAtBottom] = useState(true);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    if (!follow) return;
    el.scrollTop = el.scrollHeight;
  }, [logs, follow]);

  const updateOverflow = () => {
    const el = textareaRef.current;
    if (!el) return;
    const overflow = el.scrollHeight > el.clientHeight + 4;
    setHasOverflow(overflow);
    setAtTop(el.scrollTop <= 4);
    setAtBottom(el.scrollHeight - el.scrollTop - el.clientHeight < 12);
  };

  useLayoutEffect(() => {
    updateOverflow();
  }, [logs]);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    const ro = new ResizeObserver(updateOverflow);
    ro.observe(el);
    if (wrapRef.current) ro.observe(wrapRef.current);
    return () => ro.disconnect();
  }, []);

  const handleScroll = () => {
    const el = textareaRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setFollow(nearBottom);
    updateOverflow();
  };

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(logs);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div className="flex flex-col flex-1 min-h-0 overflow-hidden logs-pane">
      <div className="flex flex-wrap gap-2 px-2 pb-1 text-xs">
        <button
          type="button"
          onClick={() => setFollow((f) => !f)}
          aria-pressed={follow}
          className="px-2 py-1 border border-white/20 hover:border-white/50 bg-transparent text-white cursor-pointer transition-all duration-150"
        >
          {follow ? "Pause follow" : "Follow output"}
        </button>
        {onClear && (
          <button
            type="button"
            onClick={onClear}
            className="px-2 py-1 border border-white/20 hover:border-white/50 bg-transparent text-white cursor-pointer transition-all duration-150"
          >
            Clear
          </button>
        )}
        <button
          type="button"
          onClick={handleCopy}
          className="px-2 py-1 border border-white/20 hover:border-white/50 bg-transparent text-white cursor-pointer transition-all duration-150"
          aria-live="polite"
        >
          {copied ? "Copied!" : "Copy"}
        </button>
        {onOpenFolder && (
          <button
            type="button"
            onClick={onOpenFolder}
            className="px-2 py-1 border border-white/20 hover:border-white/50 bg-transparent text-white cursor-pointer transition-all duration-150"
          >
            Open folder
          </button>
        )}
        <button
          type="button"
          onClick={() => setOpenPanel((open) => (open === "exclude" ? null : "exclude"))}
          aria-expanded={openPanel === "exclude"}
          aria-controls="scs-exclude-folders"
          className={`px-2 py-1 border bg-transparent text-white cursor-pointer transition-all duration-150 ${openPanel === "exclude" ? "border-white/60" : "border-white/20 hover:border-white/50"}`}
        >
          Exclude folder
        </button>
        <button
          type="button"
          onClick={() => setOpenPanel((open) => (open === "excludeSelect" ? null : "excludeSelect"))}
          aria-expanded={openPanel === "excludeSelect"}
          aria-controls="scs-exclude-folder-select"
          className={`px-2 py-1 border bg-transparent text-white cursor-pointer transition-all duration-150 ${openPanel === "excludeSelect" ? "border-white/60" : "border-white/20 hover:border-white/50"}`}
        >
          Exclude folder select
        </button>
        {(["whitelist", "blacklist"] as const).map((mode) => (
          <button
            key={mode}
            type="button"
            onClick={() => setOpenPanel((open) => (open === mode ? null : mode))}
            aria-expanded={openPanel === mode}
            aria-controls={`scs-extension-${mode}`}
            className={`px-2 py-1 border bg-transparent text-white cursor-pointer transition-all duration-150 ${openPanel === mode ? "border-white/60" : "border-white/20 hover:border-white/50"}`}
          >
            {mode === "whitelist" ? "Whitelist extension" : "Blacklist extension"}
          </button>
        ))}
      </div>
      {openPanel === "exclude" && <ExcludeFolders key={selectedSubdir} subdir={selectedSubdir} />}
      {openPanel === "excludeSelect" && (
        <ExcludeFolderSelect key={selectedSubdir} subdir={selectedSubdir} />
      )}
      {openPanel === "whitelist" && (
        <ExtensionFilterPanel key={selectedSubdir} mode="whitelist" selectedSubdir={selectedSubdir} />
      )}
      {openPanel === "blacklist" && (
        <ExtensionFilterPanel key={selectedSubdir} mode="blacklist" selectedSubdir={selectedSubdir} />
      )}
      <label className="sr-only" htmlFor="scs-console">WebDAV console output</label>
      <div ref={wrapRef} className="relative flex-1 min-h-0 flex flex-col overflow-hidden px-2 pb-1">
        {/* top fade */}
        <div
          aria-hidden
          className={`pointer-events-none absolute inset-x-0 top-0 h-6 bg-gradient-to-b from-black/40 to-transparent transition-opacity duration-200 z-10 ${hasOverflow && !atTop ? "opacity-100" : "opacity-0"}`}
        />
        <textarea
          id="scs-console"
          ref={textareaRef}
          readOnly
          value={logs}
          onScroll={handleScroll}
          placeholder="Click an action above to run and view output..."
          wrap="off"
          className="w-full flex-1 min-h-0 p-2 text-xs bg-black/25 border border-white/10 outline-none no-scrollbar overflow-auto overscroll-contain resize-none transition-all duration-150 font-sans font-light"
        />
        {/* bottom fade */}
        <div
          aria-hidden
          className={`pointer-events-none absolute inset-x-2 bottom-1 h-6 bg-gradient-to-t from-black/40 to-transparent transition-opacity duration-200 z-10 ${hasOverflow && !atBottom ? "opacity-100" : "opacity-0"}`}
        />
      </div>
    </div>
  );
}

export default RcloneConsole;
