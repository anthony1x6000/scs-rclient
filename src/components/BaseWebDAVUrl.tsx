import { useState, useEffect } from "react";
import { load } from "@tauri-apps/plugin-store";
import TextInput from "./TextInput";

function BaseWebDAVURL() {
  const [url, setUrl] = useState<string>("VITE_WEBDAV_BASE_URL_PLACEHOLDER");

  useEffect(() => {
    async function loadStoredUrl() {
      try {
        const store = await load("settings.json", { autoSave: true, defaults: {} });
        const saved = await store.get<{ value: string }>("webdav_url");
        if (saved && saved.value) {
          setUrl(saved.value);
        } else {
          setUrl("VITE_WEBDAV_BASE_URL_PLACEHOLDER");
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
    <TextInput 
      type="text" 
      value={url} 
      onChange={(e) => handleChange(e.target.value)} 
      placeholder="Paste WebDAV URL..." 
      lowercase
      className="w-[50%]"
    />
  );
}

export default BaseWebDAVURL;
