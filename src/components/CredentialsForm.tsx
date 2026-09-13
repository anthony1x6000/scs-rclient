import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createRcloneCommand, resolveRemoteUrl, obscurePassword, ensureRcloneDetected } from "../utils/rclone";
import { getWebDAVBase, getSelectedSubdir, getSavedUsername, setSavedUsername } from "../settings";
import TextInput from "./TextInput";

function CredentialsForm() {
  const [username, setUsername] = useState<string>("");
  const [password, setPassword] = useState<string>("");
  const [status, setStatus] = useState<'idle' | 'testing' | 'success' | 'error'>('idle');
  const [statusText, setStatusText] = useState<string>("");
  const [hasSavedCredentials, setHasSavedCredentials] = useState<boolean>(false);
  const lookupSeq = useRef(0);

  // Debounced keyring lookup: only the latest username's result wins.
  useEffect(() => {
    const name = username.trim();
    if (!name) {
      setHasSavedCredentials(false);
      return;
    }
    const seq = ++lookupSeq.current;
    const timer = setTimeout(async () => {
      try {
        const savedPass = await invoke<string>("get_credentials", { username: name });
        if (lookupSeq.current !== seq) return;
        if (savedPass) {
          setPassword(savedPass);
          setHasSavedCredentials(true);
        } else {
          setHasSavedCredentials(false);
        }
      } catch {
        if (lookupSeq.current !== seq) return;
        setHasSavedCredentials(false);
      }
    }, 300);
    return () => clearTimeout(timer);
  }, [username]);

  const handleUsernameChange = (val: string) => {
    setUsername(val);
    setStatus('idle');
    setStatusText("");
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      handleLogin();
    }
  };

  const validateCredentials = async (userVal: string, passVal: string) => {
    setStatus('testing');
    setStatusText("Testing credentials…");
    try {
      await ensureRcloneDetected();
      // Use the same source of truth as execution: selected subdirectory only.
      const [baseUrl, selectedSubdir] = await Promise.all([
        getWebDAVBase(),
        getSelectedSubdir(),
      ]);

      const fullTestUrl = resolveRemoteUrl(baseUrl, selectedSubdir);

      const obscuredPassword = passVal ? await obscurePassword(passVal) : "";

      const args = [
        "lsf",
        ":webdav:",
        `--webdav-url=${fullTestUrl}`
      ];
      if (userVal) {
        args.push(`--webdav-user=${userVal}`);
      }

      const env = obscuredPassword ? { RCLONE_WEBDAV_PASS: obscuredPassword } : undefined;
      const command = createRcloneCommand(args, env);
      const result = await command.execute();
      if (result.code === 0) {
        setStatus('success');
        setStatusText("Credentials valid.");
      } else {
        setStatus('error');
        setStatusText(`Authentication test failed (exit ${result.code}).`);
      }
    } catch (e) {
      setStatus('error');
      setStatusText(`Error during validation: ${e}`);
    }
  };

  const handleLogin = async () => {
    const user = username.trim();
    if (!user) {
      setStatus('error');
      setStatusText("Username is required.");
      return;
    }
    try {
      await setSavedUsername(user);
      await invoke("save_credentials", { username: user, secret: password });
      setHasSavedCredentials(true);
      // Explicit user action only — no auto-validation elsewhere.
      await validateCredentials(user, password);
    } catch (e) {
      setStatus('error');
      setStatusText(`Error during credentials save/test: ${e}`);
    }
  };

  const handleForget = async () => {
    const user = username.trim();
    try {
      if (user) {
        await invoke("delete_credentials", { username: user });
      }
    } catch (e) {
      setStatusText(`Could not delete stored credentials: ${e}`);
    }
    setPassword("");
    setHasSavedCredentials(false);
    setStatus('idle');
  };

  useEffect(() => {
    async function loadSaved() {
      try {
        const userVal = await getSavedUsername();
        if (!userVal) return;
        setUsername(userVal);
        try {
          const savedPass = await invoke<string>("get_credentials", { username: userVal });
          if (savedPass) {
            setPassword(savedPass);
            setHasSavedCredentials(true);
            // Do NOT auto-validate on launch: user clicks Test/Login explicitly.
          }
        } catch {
          // No saved secret — user enters it manually.
        }
      } catch (e) {
        setStatusText(`Failed to load saved credentials: ${e}`);
      }
    }
    loadSaved();
  }, []);

  return (
    <>
      <label className="sr-only" htmlFor="scs-username">Username</label>
      <TextInput
        id="scs-username"
        type="text"
        value={username}
        onChange={(e) => handleUsernameChange(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder="Username..."
        status={status}
        className="w-[20%]"
        autoComplete="username"
      />
      <label className="sr-only" htmlFor="scs-password">Password</label>
      <TextInput
        id="scs-password"
        type="password"
        value={password}
        onChange={(e) => {
          setPassword(e.target.value);
          setStatus('idle');
          setStatusText("");
        }}
        onKeyDown={handleKeyDown}
        placeholder="Password..."
        status={status}
        className="w-[20%]"
        autoComplete="current-password"
      />
      <button
        type="button"
        onClick={handleLogin}
        disabled={status === 'testing' || !username.trim()}
        className="ml-2 px-3 py-1 text-xs border border-white/20 hover:border-white/40 bg-transparent text-white cursor-pointer disabled:opacity-40"
      >
        Test / Login
      </button>
      {hasSavedCredentials && (
        <button
          type="button"
          onClick={handleForget}
          className="ml-2 px-3 py-1 text-xs border border-white/20 hover:border-white/40 bg-transparent text-white cursor-pointer"
        >
          Forget
        </button>
      )}
      <span className="sr-only" role="status" aria-live="polite">{statusText}</span>
    </>
  );
}

export default CredentialsForm;
