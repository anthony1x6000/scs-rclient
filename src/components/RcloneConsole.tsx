import { useEffect, useRef } from "react";

interface RcloneConsoleProps {
  logs: string;
}

export function RcloneConsole({ logs }: RcloneConsoleProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.scrollTop = textareaRef.current.scrollHeight;
    }
  }, [logs]);

  return (
    <textarea
      ref={textareaRef}
      readOnly
      value={logs}
      placeholder="Click a command above to run and view output..."
      className="w-screen h-fit min-h-0 p-2"
    />
  );
}
export default RcloneConsole;
