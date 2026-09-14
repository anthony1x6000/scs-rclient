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
  const lastSavedUserRef = useRef<string>("");
  const lookupSeq = useRef(0);

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
      console.log("Testing credentials with rclone...", fullTestUrl);

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
        console.log("Rclone authentication test succeeded! Output:\n", result.stdout);
        setStatus('success');
        setStatusText("Credentials valid.");
      } else {
        console.error(`Rclone authentication test failed with code ${result.code}:\n`, result.stderr);
        setStatus('error');
        setStatusText(`Authentication test failed (exit ${result.code}).`);
      }
    } catch (e) {
      console.error("Error during validation:", e);
      setStatus('error');
      setStatusText(`Error during validation: ${e}`);
    }
  };

  const handleSaveAndValidate = async (userVal: string, passVal: string) => {
    const user = userVal.trim();
    const pass = passVal.trim();
    if (!user || !pass) return;
    try {
      await setSavedUsername(user);
      await invoke("save_credentials", { username: user, secret: pass });
      lastSavedUserRef.current = user;
      await validateCredentials(user, pass);
    } catch (e) {
      console.error("Error saving credentials:", e);
      setStatus('error');
      setStatusText(`Error saving credentials: ${e}`);
    }
  };

  const forgetCredentials = async (userToForget?: string) => {
    const user = userToForget || username.trim() || lastSavedUserRef.current;
    if (user) {
      try {
        await invoke("delete_credentials", { username: user });
      } catch (e) {
        console.error("Error deleting credentials:", e);
      }
    }
    if (!username.trim() || user === lastSavedUserRef.current) {
      try {
        await setSavedUsername("");
      } catch (e) {
        console.error("Error clearing saved username:", e);
      }
      lastSavedUserRef.current = "";
    }
    setStatus('idle');
    setStatusText("");
  };

  const handleUsernameChange = (val: string) => {
    setUsername(val);
    setStatus('idle');
    setStatusText("");

    if (!val.trim()) {
      // User removed username -> forget!
      void forgetCredentials(lastSavedUserRef.current);
      setPassword("");
      return;
    }

    // Debounced keyring lookup for the new username
    const trimmed = val.trim();
    const seq = ++lookupSeq.current;
    setTimeout(async () => {
      if (lookupSeq.current !== seq) return;
      try {
        const savedPass = await invoke<string>("get_credentials", { username: trimmed });
        if (lookupSeq.current !== seq) return;
        if (savedPass) {
          setPassword(savedPass);
          lastSavedUserRef.current = trimmed;
          await setSavedUsername(trimmed);
          void validateCredentials(trimmed, savedPass);
        }
      } catch {
        // No saved credentials for this username
      }
    }, 300);
  };

  const handlePasswordChange = (val: string) => {
    setPassword(val);
    setStatus('idle');
    setStatusText("");

    if (!val.trim()) {
      // User removed password -> forget!
      void forgetCredentials(username.trim() || lastSavedUserRef.current);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      const user = username.trim();
      const pass = password.trim();
      if (user && pass) {
        void handleSaveAndValidate(user, pass);
      }
    }
  };

  const handleBlur = () => {
    const user = username.trim();
    const pass = password.trim();
    if (user && pass) {
      void handleSaveAndValidate(user, pass);
    } else if (!user || !pass) {
      void forgetCredentials();
    }
  };

  useEffect(() => {
    async function loadSaved() {
      try {
        const userVal = await getSavedUsername();
        if (!userVal) return;
        setUsername(userVal);
        lastSavedUserRef.current = userVal;
        try {
          const savedPass = await invoke<string>("get_credentials", { username: userVal });
          if (savedPass) {
            setPassword(savedPass);
            // Automatically test login on mount
            void validateCredentials(userVal, savedPass);
          }
        } catch {
          // No saved secret
        }
      } catch (e) {
        console.error("Failed to load saved credentials:", e);
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
        onBlur={handleBlur}
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
        onChange={(e) => handlePasswordChange(e.target.value)}
        onKeyDown={handleKeyDown}
        onBlur={handleBlur}
        placeholder="Password..."
        status={status}
        className="w-[20%]"
        autoComplete="current-password"
      />
      <span className="sr-only" role="status" aria-live="polite">{statusText}</span>
    </>
  );
}

export default CredentialsForm;
