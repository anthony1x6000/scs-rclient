import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { load } from "@tauri-apps/plugin-store";
import { Command } from "@tauri-apps/plugin-shell";
import TextInput from "./TextInput";

function CredentialsForm() {
  const [username, setUsername] = useState<string>("");
  const [password, setPassword] = useState<string>("");
  const [status, setStatus] = useState<'idle' | 'testing' | 'success' | 'error'>('idle');


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

  const validateCredentials = async (userVal: string, passVal: string) => {
    setStatus('testing');
    try {
      console.log("Testing credentials with rclone...");
      
      // 1. Obscure the password because rclone's backend expects obscured password values
      const obscureCommand = Command.create("rclone", ["obscure", passVal]);
      const obscureResult = await obscureCommand.execute();
      if (obscureResult.code !== 0) {
        throw new Error(`Failed to obscure password: ${obscureResult.stderr}`);
      }
      const obscuredPassword = obscureResult.stdout.trim();

      // 2. Resolve the test URL dynamically from the store
      const store = await load("settings.json", { autoSave: true, defaults: {} });
      const savedBase = await store.get<{ value: string }>("webdav_url");
      const savedSub = await store.get<{ value: string }>("test_subdirectory");
      
      const baseUrl = savedBase?.value || import.meta.env.VITE_WEBDAV_BASE_URL || "";
      const testSub = savedSub?.value !== undefined ? savedSub.value : "";
      
      let fullTestUrl = baseUrl;
      if (!fullTestUrl.endsWith("/")) {
        fullTestUrl += "/";
      }
      fullTestUrl += testSub;

      // 3. Perform test rclone ls command with obscured password
      const command = Command.create("rclone", [
        "ls",
        ":webdav:",
        `--webdav-url=${fullTestUrl}`,
        `--webdav-user=${userVal}`,
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
      console.error("Error during validation:", e);
      setStatus('error');
    }
  };

  const handleLogin = async () => {
    try {
      const store = await load("settings.json", { autoSave: true, defaults: {} });
      await store.set("saved_username", { value: username });

      await invoke("save_credentials", { username, secret: password });

      // Trigger the rclone validation process
      await validateCredentials(username, password);
    } catch (e) {
      console.error("Error during credentials save/test:", e);
      setStatus('error');
    }
  };

  useEffect(() => {
    async function loadSavedAndTest() {
      try {
        const store = await load("settings.json", { autoSave: true, defaults: {} });
        const savedUser = await store.get<{ value: string }>("saved_username");
        if (savedUser && savedUser.value) {
          const userVal = savedUser.value;
          setUsername(userVal);
          const savedPass = await invoke<string>("get_credentials", { username: userVal });
          if (savedPass) {
            setPassword(savedPass);
            // Run the validation check automatically on startup/reload
            validateCredentials(userVal, savedPass);
          }
        }
      } catch (e) {
        console.error("Failed to load saved credentials:", e);
      }
    }
    loadSavedAndTest();
  }, []);

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
