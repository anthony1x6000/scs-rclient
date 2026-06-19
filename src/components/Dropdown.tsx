import { useState } from "react";

interface DropdownProps {
  onSelect?: (item: string) => void;
}

function Dropdown({ onSelect }: DropdownProps) {
  const [items, setItems] = useState<string[]>([]);
  const [selectedItem, setSelectedItem] = useState<string>("");
  const [isOpen, setIsOpen] = useState<boolean>(false);
  const [searchText, setSearchText] = useState<string>("");

  const handleSelectItem = (item: string) => {
    setSelectedItem(item);
    if (onSelect) onSelect(item);
    setIsOpen(false);
    setSearchText("");
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      const trimmed = searchText.trim();
      if (trimmed) {
        if (!items.includes(trimmed)) {
          setItems((prev) => [...prev, trimmed]);
        }
        setSelectedItem(trimmed);
        if (onSelect) onSelect(trimmed);
        setSearchText("");
        setIsOpen(false);
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
