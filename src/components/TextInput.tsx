import { InputHTMLAttributes } from "react";

export type TextInputStatus = 'idle' | 'success' | 'error' | 'testing';

export interface TextInputProps extends InputHTMLAttributes<HTMLInputElement> {
  lowercase?: boolean;
  status?: TextInputStatus;
}

function TextInput({ lowercase, status = 'idle', className = "", ...props }: TextInputProps) {
  const baseClass = "px-2 py-1 text-xs border rounded-none bg-transparent text-white outline-none transition-all duration-150";
  
  let statusClass = "border-white/20 hover:border-white/40 focus:border-white/60";
  if (status === 'success') {
    statusClass = "border-emerald-500 hover:border-emerald-400 focus:border-emerald-400 text-emerald-200";
  } else if (status === 'error') {
    statusClass = "border-red-500 hover:border-red-400 focus:border-red-400 text-red-200";
  } else if (status === 'testing') {
    statusClass = "border-amber-500/50 hover:border-amber-400/50 focus:border-amber-400/50 text-amber-200";
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
