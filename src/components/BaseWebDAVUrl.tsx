import { useState, useEffect } from "react";
import { load } from "@tauri-apps/plugin-store";

function BaseWebDAVURL() {
  const [url, setUrl] = useState<string>("");

  useEffect(() => {
    async function loadStoredUrl() {
      try {
        const store = await load("settings.json", { autoSave: true, defaults: {} });
        const saved = await store.get<{ value: string }>("webdav_url");
        if (saved) {
          setUrl(saved.value);
        }
      } catch (e) {
        console.error("Failed to load WebDAV URL:", e);
      }
    }
    loadStoredUrl();
  }, []);

  const handleChange = async (newVal: string) => {
    setUrl(newVal);
    try {
      const store = await load("settings.json", { autoSave: true, defaults: {} });
      await store.set("webdav_url", { value: newVal });
    } catch (e) {
      console.error("Failed to save WebDAV URL:", e);
    }
  };

  return (
    <input 
      type="text" 
      value={url} 
      onChange={(e) => handleChange(e.target.value)} 
      placeholder="Paste WebDAV URL..." 
      className="w-[50%] ml-2 px-2 py-1 text-xs border inline-block outline-none"
    />
  );
}

export default BaseWebDAVURL;
