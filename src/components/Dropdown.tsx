import { useState } from "react";

interface DropdownProps {
  onSelect?: (item: string) => void;
}

function Dropdown({ onSelect }: DropdownProps) {
  const [items, setItems] = useState<string[]>([]);
  const [selectedItem, setSelectedItem] = useState<string>("");
  const [isOpen, setIsOpen] = useState<boolean>(false);
  const [newItemText, setNewItemText] = useState<string>("");

  const handleAddItem = (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = newItemText.trim();
    if (trimmed) {
      if (!items.includes(trimmed)) {
        setItems((prev) => [...prev, trimmed]);
      }
      setSelectedItem(trimmed);
      if (onSelect) onSelect(trimmed);
      setNewItemText("");
      setIsOpen(false);
    }
  };

  const handleSelectItem = (item: string) => {
    setSelectedItem(item);
    if (onSelect) onSelect(item);
    setIsOpen(false);
  };

  return (
    <div className="relative inline-block">
      {/* Dropdown trigger */}
      <button 
        type="button" 
        onClick={() => setIsOpen((prev) => !prev)}
        className="text-5xl text-gray-300 uppercase cursor-pointer"
      >
        {selectedItem || "Select"}
      </button>

      {/* Dropdown menu */}
      {isOpen && (
        <div className="absolute">
          {/* Input at the top to type new name */}
          <form onSubmit={handleAddItem} className="flex p-[5px]">
            <input 
              type="text" 
              value={newItemText} 
              onChange={(e) => setNewItemText(e.target.value)} 
              placeholder="Type new name..." 
              className="flex-1 px-1 text-sm outline-none"
            />
            <button type="submit" className="cursor-pointer bg-gray-500 text-white pl-2 pr-2">Add</button>
          </form>

          {/* List of names */}
          <ul className="list-none p-0 m-0">
            {items.map((item) => (
              <li key={item}>
                <button 
                  type="button" 
                  onClick={() => handleSelectItem(item)}
                  className="w-full text-left bg-transparent border-none py-[5px] px-[10px] cursor-pointer hover:bg-gray-100 text-sm"
                >
                  {item}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

export default Dropdown;
