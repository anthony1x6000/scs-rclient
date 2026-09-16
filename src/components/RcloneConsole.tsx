import { useEffect, useRef, useState, useLayoutEffect } from "react";

interface RcloneConsoleProps {
  logs: string;
  onClear?: () => void;
}

export function RcloneConsole({ logs, onClear }: RcloneConsoleProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const [copied, setCopied] = useState(false);
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
    <div className="flex flex-col flex-1 min-h-0 overflow-hidden">
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
      <div ref={wrapRef} className="relative flex-1 min-h-0 flex flex-col overflow-hidden px-2 pb-1">
        {/* top fade */}
        <div
          aria-hidden
          className={`pointer-events-none absolute inset-x-2 top-0 h-6 bg-gradient-to-b from-black/40 to-transparent transition-opacity duration-200 z-10 ${hasOverflow && !atTop ? "opacity-100" : "opacity-0"}`}
        />
        <textarea
          id="scs-console"
          ref={textareaRef}
          readOnly
          value={logs}
          onScroll={handleScroll}
          placeholder="Click a command above to run and view output..."
          wrap="off"
          className="w-full flex-1 min-h-0 p-2 font-mono text-xs bg-black/25 border border-white/10 rounded-sm outline-none no-scrollbar overflow-auto overscroll-contain resize-none"
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
