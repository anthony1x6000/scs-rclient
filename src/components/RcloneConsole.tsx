import { useEffect, useRef, useState } from "react";

interface RcloneConsoleProps {
  logs: string;
  onClear?: () => void;
}

export function RcloneConsole({ logs, onClear }: RcloneConsoleProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const [follow, setFollow] = useState(true);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    if (!follow) return;
    el.scrollTop = el.scrollHeight;
  }, [logs, follow]);

  const handleScroll = () => {
    const el = textareaRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setFollow(nearBottom);
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
    <div className="flex flex-col flex-1 min-h-0">
      <div className="flex gap-2 px-2 pb-1 text-xs">
        <button
          type="button"
          onClick={() => setFollow((f) => !f)}
          aria-pressed={follow}
          className="px-2 py-1 border border-white/20 hover:border-white/40 bg-transparent text-white cursor-pointer"
        >
          {follow ? "Pause follow" : "Follow output"}
        </button>
        {onClear && (
          <button
            type="button"
            onClick={onClear}
            className="px-2 py-1 border border-white/20 hover:border-white/40 bg-transparent text-white cursor-pointer"
          >
            Clear
          </button>
        )}
        <button
          type="button"
          onClick={handleCopy}
          className="px-2 py-1 border border-white/20 hover:border-white/40 bg-transparent text-white cursor-pointer"
          aria-live="polite"
        >
          {copied ? "Copied!" : "Copy"}
        </button>
      </div>
      <label className="sr-only" htmlFor="scs-console">Rclone output</label>
      <textarea
        id="scs-console"
        ref={textareaRef}
        readOnly
        value={logs}
        onScroll={handleScroll}
        placeholder="Click a command above to run and view output..."
        wrap="off"
        className="w-full flex-1 min-h-0 p-2 font-mono text-xs"
      />
    </div>
  );
}
export default RcloneConsole;
