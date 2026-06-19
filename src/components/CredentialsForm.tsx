import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { load } from "@tauri-apps/plugin-store";
import { Command } from "@tauri-apps/plugin-shell";
import TextInput from "./TextInput";

function CredentialsForm() {
  const [username, setUsername] = useState<string>("");
  const [password, setPassword] = useState<string>("");
  const [status, setStatus] = useState<'idle' | 'testing' | 'success' | 'error'>('idle');

  useEffect(() => {
    async function loadSavedUser() {
      try {
        const store = await load("settings.json", { autoSave: true, defaults: {} });
        const savedUser = await store.get<{ value: string }>("saved_username");
        if (savedUser && savedUser.value) {
          setUsername(savedUser.value);
          const savedPass = await invoke<string>("get_credentials", { username: savedUser.value });
          if (savedPass) {
            setPassword(savedPass);
          }
        }
      } catch (e) {
        console.error("Failed to load saved credentials:", e);
      }
    }
    loadSavedUser();
  }, []);

  const handleUsernameChange = async (val: string) => {
    setUsername(val);
    setStatus('idle');
    try {
      const savedPass = await invoke<string>("get_credentials", { username: val });
      if (savedPass) {
        setPassword(savedPass);
      } else {
        setPassword("");
      }
    } catch {
      setPassword("");
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      handleLogin();
    }
  };

  const handleLogin = async () => {
    setStatus('testing');
    try {
      const store = await load("settings.json", { autoSave: true, defaults: {} });
      await store.set("saved_username", { value: username });

      await invoke("save_credentials", { username, secret: password });

      console.log("Testing credentials with rclone...");
      
      // 1. Obscure the password because rclone's backend expects obscured password values
      const obscureCommand = Command.create("rclone", ["obscure", password]);
      const obscureResult = await obscureCommand.execute();
      if (obscureResult.code !== 0) {
        throw new Error(`Failed to obscure password: ${obscureResult.stderr}`);
      }
      const obscuredPassword = obscureResult.stdout.trim();

      // 2. Perform test rclone ls command with obscured password
      const command = Command.create("rclone", [
        "ls",
        ":webdav:",
        "--webdav-url=VITE_WEBDAV_TEST_URL_PLACEHOLDER",
        `--webdav-user=${username}`,
        `--webdav-pass=${obscuredPassword}`
      ]);
      const result = await command.execute();
      if (result.code === 0) {
        console.log("Rclone authentication test succeeded! Output:\n", result.stdout);
        setStatus('success');
      } else {
        console.error(`Rclone authentication test failed with code ${result.code}:\n`, result.stderr);
        setStatus('error');
      }
    } catch (e) {
      console.error("Error during credentials save/test:", e);
      setStatus('error');
    }
  };

  return (
    <>
      <TextInput 
        type="text" 
        value={username} 
        onChange={(e) => handleUsernameChange(e.target.value)} 
        onKeyDown={handleKeyDown}
        placeholder="Username..." 
        status={status}
        className="w-[20%]"
      />
      <TextInput 
        type="password" 
        value={password} 
        onChange={(e) => {
          setPassword(e.target.value);
          setStatus('idle');
        }} 
        onKeyDown={handleKeyDown}
        placeholder="Password..." 
        status={status}
        className="w-[20%]"
      />
    </>
  );
}

export default CredentialsForm;
