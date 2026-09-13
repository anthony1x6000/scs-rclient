import { useState, useEffect } from "react";
import { getWebDAVBase, setWebDAVBase } from "../settings";
import TextInput from "./TextInput";

function BaseWebDAVURL() {
  const [url, setUrl] = useState<string>("");
  const [saveError, setSaveError] = useState<string>("");

  useEffect(() => {
    async function loadStoredUrl() {
      try {
        setUrl(await getWebDAVBase());
      } catch (e) {
        console.error("Failed to load WebDAV URL:", e);
      }
    }
    loadStoredUrl();
  }, []);

  const handleChange = async (newVal: string) => {
    setUrl(newVal);
    setSaveError("");
    try {
      await setWebDAVBase(newVal.trim());
    } catch (e) {
      console.error("Failed to save WebDAV URL:", e);
      setSaveError("Could not save URL.");
    }
  };

  return (
    <>
      <label className="sr-only" htmlFor="scs-webdav-url">WebDAV base URL</label>
      <TextInput
        id="scs-webdav-url"
        type="text"
        value={url}
        onChange={(e) => handleChange(e.target.value)}
        placeholder="Paste WebDAV URL..."
        className="w-[50%]"
        autoComplete="url"
        inputMode="url"
      />
      {saveError && (
        <span className="text-xs text-red-400 ml-2" role="alert">{saveError}</span>
      )}
    </>
  );
}

export default BaseWebDAVURL;
