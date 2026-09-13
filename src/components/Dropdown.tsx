import { useState, useEffect, useRef } from "react";
import { getSubdirectories, getSelectedSubdir, setSubdirectories, setSelectedSubdir } from "../settings";

interface DropdownProps {
  onSelect?: (item: string) => void;
}

function Dropdown({ onSelect }: DropdownProps) {
  const [items, setItems] = useState<string[]>([]);
  const [selectedItem, setSelectedItem] = useState<string>("");
  const [isOpen, setIsOpen] = useState<boolean>(false);
  const [searchText, setSearchText] = useState<string>("");
  const [saveError, setSaveError] = useState<string>("");
  const inputRef = useRef<HTMLInputElement>(null);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;

  useEffect(() => {
    async function loadStoredSettings() {
      try {
        const [savedItems, savedSelected] = await Promise.all([
          getSubdirectories(),
          getSelectedSubdir(),
        ]);
        setItems(savedItems);
        setSelectedItem(savedSelected);
        // Initial value sync to parent (no network side effect).
        if (savedSelected) onSelectRef.current?.(savedSelected);
      } catch (e) {
        console.error("Failed to load subdirectories settings:", e);
      }
    }
    loadStoredSettings();
  }, []);

  useEffect(() => {
    if (isOpen) inputRef.current?.focus();
  }, [isOpen]);

  const saveSettings = async (updatedItems: string[], updatedSelected: string) => {
    setSaveError("");
    try {
      await Promise.all([
        setSubdirectories(updatedItems),
        setSelectedSubdir(updatedSelected),
      ]);
    } catch (e) {
      console.error("Failed to save subdirectories settings:", e);
      setSaveError("Could not save selection.");
    }
  };

  const handleSelectItem = (item: string) => {
    setSelectedItem(item);
    onSelect?.(item);
    setIsOpen(false);
    setSearchText("");
    void saveSettings(items, item);
  };

  const handleDeleteItem = (itemToDelete: string) => {
    const updatedItems = items.filter((item) => item !== itemToDelete);
    setItems(updatedItems);

    let updatedSelected = selectedItem;
    if (selectedItem === itemToDelete) {
      updatedSelected = "";
      setSelectedItem("");
      onSelect?.("");
    }

    void saveSettings(updatedItems, updatedSelected);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      const trimmed = searchText.trim();
      if (trimmed) {
        let updatedItems = items;
        if (!items.includes(trimmed)) {
          updatedItems = [...items, trimmed];
          setItems(updatedItems);
        }
        setSelectedItem(trimmed);
        onSelect?.(trimmed);
        setSearchText("");
        setIsOpen(false);
        void saveSettings(updatedItems, trimmed);
      }
    } else if (e.key === "Escape") {
      setIsOpen(false);
      setSearchText("");
    }
  };

  const handleBlur = (e: React.FocusEvent<HTMLDivElement>) => {
    // If focus moves outside the entire dropdown container, close it
    if (!e.currentTarget.contains(e.relatedTarget)) {
      setIsOpen(false);
      setSearchText("");
    }
  };

  const filteredItems = items.filter((item) =>
    item.toLowerCase().includes(searchText.toLowerCase())
  );

  return (
    <div className="relative inline-block" onBlur={handleBlur}>
      <label className="sr-only" htmlFor="scs-subdir-search">Sync subdirectory</label>
      {isOpen ? (
        <input
          id="scs-subdir-search"
          ref={inputRef}
          type="text"
          value={searchText}
          onChange={(e) => setSearchText(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={selectedItem || "select"}
          aria-expanded={isOpen}
          aria-haspopup="listbox"
          className="text-5xl text-gray-300 cursor-text bg-transparent border-none outline-none w-full"
        />
      ) : (
        <button
          type="button"
          onClick={() => {
            setIsOpen(true);
            setSearchText("");
          }}
          aria-expanded={isOpen}
          aria-haspopup="listbox"
          aria-label={selectedItem ? `Sync subdirectory: ${selectedItem}. Activate to change.` : "Select sync subdirectory"}
          className="text-5xl text-gray-300 lowercase cursor-pointer"
        >
          {selectedItem || "select"}
        </button>
      )}

      {isOpen && (
        <div className="absolute bottom-full left-0">
          <ul className="list-none p-0 m-0" role="listbox" aria-label="Sync subdirectories">
            {filteredItems.map((item) => (
              <li key={item} className="flex items-center" role="option" aria-selected={item === selectedItem}>
                <button
                  type="button"
                  onMouseDown={(e) => {
                    e.preventDefault();
                  }}
                  onClick={() => handleSelectItem(item)}
                  className="text-left bg-transparent border-none py-[5px] px-[10px] cursor-pointer text-sm"
                >
                  {item}
                </button>
                <button
                  type="button"
                  aria-label={`Remove ${item}`}
                  onMouseDown={(e) => {
                    e.preventDefault();
                  }}
                  onClick={() => handleDeleteItem(item)}
                  className="bg-transparent border-none py-[5px] px-[10px] cursor-pointer text-xs text-red-500 font-bold"
                >
                  ×
                </button>
              </li>
            ))}
            {filteredItems.length === 0 && (
              <li className="py-[5px] px-[10px] text-sm text-gray-400 italic">
                {searchText.trim() ? `Press Enter to add "${searchText}"` : "Type to search or add a subdirectory…"}
              </li>
            )}
          </ul>
        </div>
      )}
      {saveError && (
        <span className="text-xs text-red-400 ml-2" role="alert">{saveError}</span>
      )}
    </div>
  );
}

export default Dropdown;
