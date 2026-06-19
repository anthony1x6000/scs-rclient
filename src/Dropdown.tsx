import { useState } from "react";

interface DropdownProps {
  initialItems?: string[];
  onSelect?: (item: string) => void;
}

function Dropdown({ initialItems = ["CIS1050", "CIS1500", "CIS2500"], onSelect }: DropdownProps) {
  const [items, setItems] = useState<string[]>(initialItems);
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
    <div style={{ position: "relative", display: "inline-block" }}>
      {/* Dropdown trigger */}
      <button 
        type="button" 
        onClick={() => setIsOpen((prev) => !prev)}
      >
        {selectedItem || "Select a course..."}
      </button>

      {/* Dropdown menu */}
      {isOpen && (
        <div style={{
          position: "absolute",
          top: "100%",
          left: 0,
          background: "white",
          border: "1px solid #ccc",
          zIndex: 1000,
          minWidth: "160px"
        }}>
          {/* Input at the top to type new name */}
          <form onSubmit={handleAddItem} style={{ display: "flex", padding: "5px", borderBottom: "1px solid #ccc" }}>
            <input 
              type="text" 
              value={newItemText} 
              onChange={(e) => setNewItemText(e.target.value)} 
              placeholder="Type new name..." 
              style={{ flex: 1 }}
            />
            <button type="submit">Add</button>
          </form>

          {/* List of names */}
          <ul style={{ listStyle: "none", padding: 0, margin: 0 }}>
            {items.map((item) => (
              <li key={item}>
                <button 
                  type="button" 
                  onClick={() => handleSelectItem(item)}
                  style={{
                    width: "100%",
                    textAlign: "left",
                    background: "none",
                    border: "none",
                    padding: "5px 10px",
                    cursor: "pointer"
                  }}
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
