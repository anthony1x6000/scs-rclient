import { InputHTMLAttributes } from "react";

interface TextInputProps extends InputHTMLAttributes<HTMLInputElement> {
  lowercase?: boolean;
  status?: 'success' | 'error' | 'testing' | 'idle';
}

function TextInput({ lowercase, status = 'idle', className = "", ...props }: TextInputProps) {
  const baseClass = "ml-2 px-2 py-1 text-xs border bg-transparent text-white outline-none transition-all";
  
  let statusClass = "border-white/20 hover:border-white/40 focus:border-white/60";
  if (status === 'success') {
    statusClass = "bg-emerald-950/80 border-emerald-500 text-emerald-200";
  } else if (status === 'error') {
    statusClass = "bg-red-950/80 border-red-500 text-red-200";
  } else if (status === 'testing') {
    statusClass = "bg-amber-950/40 border-amber-500/50 text-amber-200";
  }

  const caseClass = lowercase ? "lowercase" : "";

  return (
    <input
      {...props}
      className={`${baseClass} ${statusClass} ${caseClass} ${className}`}
    />
  );
}

export default TextInput;
