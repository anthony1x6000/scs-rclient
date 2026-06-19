import { useState, useEffect } from "react";
import { load } from "@tauri-apps/plugin-store";

interface DropdownProps {
  onSelect?: (item: string) => void;
}

function Dropdown({ onSelect }: DropdownProps) {
  const [items, setItems] = useState<string[]>([]);
  const [selectedItem, setSelectedItem] = useState<string>("");
  const [isOpen, setIsOpen] = useState<boolean>(false);
  const [searchText, setSearchText] = useState<string>("");

  useEffect(() => {
    async function loadStoredSettings() {
      try {
        const store = await load("settings.json", { autoSave: true, defaults: {} });
        const savedItems = await store.get<{ value: string[] }>("subdirectories");
        const savedSelected = await store.get<{ value: string }>("selected_subdirectory");
        
        if (savedItems && Array.isArray(savedItems.value)) {
          setItems(savedItems.value);
        }
        if (savedSelected && typeof savedSelected.value === "string") {
          setSelectedItem(savedSelected.value);
          if (onSelect) onSelect(savedSelected.value);
        }
      } catch (e) {
        console.error("Failed to load subdirectories settings:", e);
      }
    }
    loadStoredSettings();
  }, []);

  const saveSettings = async (updatedItems: string[], updatedSelected: string) => {
    try {
      const store = await load("settings.json", { autoSave: true, defaults: {} });
      await store.set("subdirectories", { value: updatedItems });
      await store.set("selected_subdirectory", { value: updatedSelected });
    } catch (e) {
      console.error("Failed to save subdirectories settings:", e);
    }
  };

  const handleSelectItem = (item: string) => {
    setSelectedItem(item);
    if (onSelect) onSelect(item);
    setIsOpen(false);
    setSearchText("");
    saveSettings(items, item);
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
        if (onSelect) onSelect(trimmed);
        setSearchText("");
        setIsOpen(false);
        saveSettings(updatedItems, trimmed);
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
      {/* Dropdown trigger */}
      {isOpen ? (
        <input 
          type="text" 
          value={searchText} 
          onChange={(e) => setSearchText(e.target.value)} 
          onKeyDown={handleKeyDown}
          autoFocus
          placeholder={selectedItem || "select"}
          className="text-5xl text-gray-300 cursor-text bg-transparent border-none outline-none w-full"
        />
      ) : (
        <button 
          type="button" 
          onClick={() => {
            setIsOpen(true);
            setSearchText(selectedItem);
          }}
          className="text-5xl text-gray-300 lowercase cursor-pointer"
        >
          {selectedItem || "select"}
        </button>
      )}

      {/* Dropdown menu */}
      {isOpen && (
        <div className="absolute bottom-full left-0">
          {/* List of names */}
          <ul className="list-none p-0 m-0">
            {filteredItems.map((item) => (
              <li key={item}>
                <button 
                  type="button" 
                  onMouseDown={(e) => {
                    // Prevent input blur before click is handled
                    e.preventDefault();
                  }}
                  onClick={() => handleSelectItem(item)}
                  className="w-full text-left bg-transparent border-none py-[5px] px-[10px] cursor-pointer text-sm"
                >
                  {item}
                </button>
              </li>
            ))}
            {filteredItems.length === 0 && (
              <li className="py-[5px] px-[10px] text-sm text-gray-400 italic">
                {searchText.trim() ? `Press Enter to add "${searchText}"` : "Type to search/add... (1052427-MATH_1060_OLP_S26)? "}
              </li>
            )}
          </ul>
        </div>
      )}
    </div>
  );
}

export default Dropdown;
