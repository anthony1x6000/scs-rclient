import { useState, useEffect, useRef, useMemo, useLayoutEffect, useCallback } from "react";
import {
  getSubdirectories,
  getSelectedSubdir,
  setSubdirectories,
  setSelectedSubdir,
  getBulkSelection,
  setBulkSelection,
  getBulkPresets,
  setBulkPresets,
} from "../settings";
import { addFolderFromInput, commitSubdirFromSearch, normalizeBulkSelection, toggleBulkTick, BULK_MIN_FOLDERS } from "../bulk";
import { moveHighlight } from "../listNavigation";
import {
  type BulkPreset,
  livePresetFolders,
  buildPresetLabels,
  presetTooltip,
  filterBulkPresets,
} from "../bulkPresets";

interface DropdownProps {
  onSelect?: (item: string) => void;
  onBulkChange?: (ticks: string[]) => void;
}

const ROW_H = 34; // px approx per row (py5 + text-sm)

function Dropdown({ onSelect, onBulkChange }: DropdownProps) {
  const [items, setItems] = useState<string[]>([]);
  const [bulkTicks, setBulkTicks] = useState<string[]>([]);
  const [presets, setPresets] = useState<BulkPreset[]>([]);
  const [selectedItem, setSelectedItem] = useState<string>("");
  const [isOpen, setIsOpen] = useState<boolean>(false);
  const [searchText, setSearchText] = useState<string>("");
  const [addText, setAddText] = useState<string>("");
  const [addError, setAddError] = useState<string>("");
  const [searchError, setSearchError] = useState<string>("");
  const [saveError, setSaveError] = useState<string>("");
  const [highlight, setHighlight] = useState(-1);
  const [availableHeight, setAvailableHeight] = useState(280);

  const inputRef = useRef<HTMLInputElement>(null);
  const addInputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const listScrollRef = useRef<HTMLDivElement>(null);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const onBulkChangeRef = useRef(onBulkChange);
  onBulkChangeRef.current = onBulkChange;

  // Load
  useEffect(() => {
    async function loadStoredSettings() {
      try {
        const [savedItems, savedSelected, savedTicks, savedPresets] = await Promise.all([
          getSubdirectories(),
          getSelectedSubdir(),
          getBulkSelection(),
          getBulkPresets(),
        ]);
        const normalizedTicks = normalizeBulkSelection(savedItems, savedTicks);
        setItems(savedItems);
        setBulkTicks(normalizedTicks);
        setSelectedItem(savedSelected);
        setPresets(savedPresets);
        if (savedSelected) onSelectRef.current?.(savedSelected);
        onBulkChangeRef.current?.(normalizedTicks);
      } catch (e) {
        console.error("Failed to load subdirectories settings:", e);
      }
    }
    loadStoredSettings();
  }, []);

  useEffect(() => {
    if (isOpen) inputRef.current?.focus();
  }, [isOpen]);

  // Measure available height above trigger (how much space we can grow upward without hitting top)
  const recomputeAvailable = useCallback(() => {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    // rect.top is distance from viewport top to the inline button/input row.
    // Dropdown grows upward (bottom-full), so max we can use is rect.top - margin.
    const margin = 16;
    const topReserve = 12; // keep a little breathing room from top edge / header
    const avail = Math.max(120, rect.top - margin - topReserve);
    // Cap to avoid gigantic list on tall windows; pocket should stay compact + encourage filtering
    const capped = Math.min(avail, 340);
    setAvailableHeight(capped);
  }, []);

  useLayoutEffect(() => {
    if (!isOpen) return;
    recomputeAvailable();
    const onResize = () => recomputeAvailable();
    window.addEventListener("resize", onResize);
    // Also watch container moves due to parent layout (pb-44 etc.)
    const ro = new ResizeObserver(onResize);
    if (containerRef.current) ro.observe(containerRef.current);
    return () => {
      window.removeEventListener("resize", onResize);
      ro.disconnect();
    };
  }, [isOpen, recomputeAvailable]);

  const saveSettings = async (
    updatedItems: string[],
    updatedSelected: string,
    updatedTicks: string[]
  ) => {
    setSaveError("");
    try {
      await Promise.all([
        setSubdirectories(updatedItems),
        setSelectedSubdir(updatedSelected),
        setBulkSelection(normalizeBulkSelection(updatedItems, updatedTicks)),
      ]);
    } catch (e) {
      console.error("Failed to save subdirectories settings:", e);
      setSaveError("Could not save selection.");
    }
  };

  // Presets are re-read on every open so runs finished elsewhere show up fresh.
  const refreshPresets = async () => {
    try {
      setPresets(await getBulkPresets());
    } catch (e) {
      console.error("Failed to load bulk presets:", e);
    }
  };

  const applyTicks = (updatedTicks: string[]) => {
    setBulkTicks(updatedTicks);
    onBulkChangeRef.current?.(updatedTicks);
  };

  // Selecting one folder overrides both the stored single subdir and the bulk
  // ticks; `nextItems` lets a freshly created folder be saved in the same write.
  const handleSelectItem = (item: string, nextItems: string[] = items) => {
    setSelectedItem(item);
    onSelect?.(item);
    applyTicks([]); // choosing one folder means abandoning the bulk selection
    setIsOpen(false);
    setSearchText("");
    setAddText("");
    setAddError("");
    setSearchError("");
    setHighlight(-1);
    void saveSettings(nextItems, item, []);
  };

  // Big-box Enter: an exact saved match is selected; otherwise an explicitly
  // highlighted row wins so arrows still work on a partial filter; otherwise
  // the typed name is created. Every commit overrides the ticks and single subdir.
  const commitSearchText = () => {
    const result = commitSubdirFromSearch(items, searchText);
    if (result.ok && !result.created) {
      handleSelectItem(result.name, result.items);
      return;
    }
    if (highlight >= 0 && highlight < filteredItems.length) {
      const chosen = filteredItems[highlight];
      if (chosen) {
        handleSelectItem(chosen);
        return;
      }
    }
    if (!result.ok) {
      setSearchError(result.error);
      return;
    }
    setSearchError("");
    setItems(result.items);
    handleSelectItem(result.name, result.items);
  };

  const handleToggleTick = (item: string) => {
    const updatedTicks = normalizeBulkSelection(items, toggleBulkTick(bulkTicks, item));
    applyTicks(updatedTicks);
    void setBulkSelection(updatedTicks);
  };

  // Preset click = bulk ticking its live members: no action fires, single selection stays.
  const handleApplyPreset = (preset: BulkPreset) => {
    const live = normalizeBulkSelection(items, livePresetFolders(preset, items));
    applyTicks(live);
    void setBulkSelection(live);
    setIsOpen(false);
    setSearchText("");
    setAddText("");
    setAddError("");
    setSearchError("");
    setHighlight(-1);
  };

  const handleDeletePreset = (preset: BulkPreset) => {
    const updated = presets.filter((p) => p !== preset);
    setPresets(updated);
    setBulkPresets(updated).catch((e) => console.error("Failed to save bulk presets:", e));
  };

  // Bottom add-input: Enter normalizes the text, appends the folder when new,
  // ticks it for bulk, and clears the input so the next one can be typed.
  const handleAddFromInput = () => {
    const result = addFolderFromInput(items, bulkTicks, addText);
    if (!result.ok) {
      setAddError(result.error);
      return;
    }
    setAddError("");
    setAddText("");
    setItems(result.items);
    applyTicks(result.ticks);
    void saveSettings(result.items, selectedItem, result.ticks);
  };

  const handleDeleteItem = (itemToDelete: string) => {
    const updatedItems = items.filter((item) => item !== itemToDelete);
    setItems(updatedItems);
    const updatedTicks = normalizeBulkSelection(updatedItems, bulkTicks);
    applyTicks(updatedTicks);
    let updatedSelected = selectedItem;
    if (selectedItem === itemToDelete) {
      updatedSelected = "";
      setSelectedItem("");
      onSelect?.("");
    }
    // keep page in bounds after delete
    void saveSettings(updatedItems, updatedSelected, updatedTicks);
  };

  const filteredItems = useMemo(
    () => items.filter((item) => item.toLowerCase().includes(searchText.toLowerCase())),
    [items, searchText]
  );

  // Presets follow the folder list, most recent first; labels align with the filtered rows.
  const visiblePresets = useMemo(() => filterBulkPresets(presets, searchText), [presets, searchText]);
  const presetLabels = useMemo(() => buildPresetLabels(visiblePresets), [visiblePresets]);

  // A new filter starts the scan from the top of the list.
  useEffect(() => {
    setHighlight(-1);
    if (listScrollRef.current) listScrollRef.current.scrollTop = 0;
  }, [searchText]);

  // Keep the keyboard-highlighted row visible inside the scroll viewport.
  useEffect(() => {
    if (highlight < 0) return;
    document.getElementById(`scs-opt-${highlight}`)?.scrollIntoView({ block: "nearest" });
  }, [highlight]);

  // PageUp/PageDown move roughly one viewport of rows, keeping one row of overlap.
  const pageRows = () => {
    const height = listScrollRef.current?.clientHeight ?? 0;
    return Math.max(1, Math.floor(height / ROW_H) - 1);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    const total = filteredItems.length;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setHighlight((h) => moveHighlight(h, 1, total));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlight((h) => moveHighlight(h, -1, total));
    } else if (e.key === "PageDown" || e.key === "PageUp") {
      e.preventDefault();
      const delta = e.key === "PageDown" ? pageRows() : -pageRows();
      setHighlight((h) => moveHighlight(h, delta, total));
    } else if (e.key === "Home" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      setHighlight(total > 0 ? 0 : -1);
    } else if (e.key === "End" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      setHighlight(total > 0 ? total - 1 : -1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      commitSearchText();
    } else if (e.key === "Escape") {
      setIsOpen(false);
      setSearchText("");
      setAddText("");
      setAddError("");
      setSearchError("");
      setHighlight(-1);
    }
  };

  const handleBlur = (e: React.FocusEvent<HTMLDivElement>) => {
    if (!e.currentTarget.contains(e.relatedTarget)) {
      setIsOpen(false);
      setSearchText("");
      setAddText("");
      setAddError("");
      setSearchError("");
      setHighlight(-1);
    }
  };

  // Bulk (2+ ticks) shows a count; a single tick is that folder's name.
  const isBulk = bulkTicks.length >= BULK_MIN_FOLDERS;
  const displayLabel = isBulk
    ? `${bulkTicks.length} folders`
    : (bulkTicks[0] ?? selectedItem) || "select";

  return (
    <div className="relative inline-block min-w-0 max-w-full" onBlur={handleBlur} ref={containerRef}>
      <label className="sr-only" htmlFor="scs-subdir-search">
        Sync subdirectory
      </label>
      {isOpen ? (
        <input
          id="scs-subdir-search"
          ref={inputRef}
          type="text"
          value={searchText}
          onChange={(e) => {
            setSearchText(e.target.value);
            if (searchError) setSearchError("");
          }}
          onKeyDown={handleKeyDown}
          placeholder={displayLabel}
          aria-expanded={isOpen}
          aria-haspopup="listbox"
          aria-controls="scs-subdir-listbox"
          aria-invalid={searchError ? true : undefined}
          aria-describedby={searchError ? "scs-subdir-search-error" : undefined}
          aria-activedescendant={
            highlight >= 0 ? `scs-opt-${highlight}` : undefined
          }
          className="text-5xl text-gray-300 cursor-text bg-transparent border-none outline-none w-full min-w-0 placeholder:text-gray-500"
        />
      ) : (
        <button
          type="button"
          onClick={() => {
            setIsOpen(true);
            setSearchText("");
            setSearchError("");
            setHighlight(-1);
            void refreshPresets();
            recomputeAvailable();
          }}
          aria-expanded={isOpen}
          aria-haspopup="listbox"
          aria-label={
            isBulk
              ? `Bulk selection: ${bulkTicks.length} subdirectories. Activate to change.`
              : selectedItem
                ? `Sync subdirectory: ${selectedItem}. Activate to change.`
                : "Select sync subdirectory"
          }
          className="text-5xl text-gray-300 lowercase cursor-pointer text-left truncate max-w-full"
          title={displayLabel}
        >
          {displayLabel}
        </button>
      )}

      {searchError && (
        <span id="scs-subdir-search-error" role="alert" className="block mt-1 text-xs text-red-400">
          {searchError}
        </span>
      )}

      {isOpen && (
        <div
          className="absolute bottom-full left-0 mb-2 z-20"
          // keep dropdown width reasonable, not full viewport
          style={{ width: "min(360px, 86vw)" }}
        >
          <div className="relative bg-black/85 backdrop-blur-md border border-white/15 shadow-[0_12px_40px_rgba(0,0,0,0.6)] overflow-hidden">
            {/* Scroll viewport — native scrollbar, no paging */}
            <div
              ref={listScrollRef}
              className="scs-scrollbar overflow-y-auto overflow-x-hidden overscroll-contain"
              style={{ maxHeight: availableHeight }}
              tabIndex={-1}
            >
              <ul
                id="scs-subdir-listbox"
                className="list-none p-1 m-0"
                role="listbox"
                aria-label="Sync subdirectories"
              >
                {filteredItems.map((item, globalIdx) => {
                  const isSelected = item === selectedItem;
                  const isHighlighted = globalIdx === highlight;
                  const isTicked = bulkTicks.includes(item);
                  return (
                    <li
                      key={item}
                      id={`scs-opt-${globalIdx}`}
                      className={`flex items-center ${isHighlighted ? "bg-white/10 ring-1 ring-white/20" : ""} ${isSelected ? "bg-white/[0.06]" : ""}`}
                      role="option"
                      aria-selected={isSelected}
                    >
                      <input
                        type="checkbox"
                        checked={isTicked}
                        onChange={() => handleToggleTick(item)}
                        onMouseDown={(e) => e.preventDefault()}
                        aria-label={`Include ${item} in bulk selection`}
                        title="Include in bulk selection"
                        className="shrink-0 ml-2 w-3.5 h-3.5 accent-white cursor-pointer"
                      />
                      <button
                        type="button"
                        onMouseDown={(e) => e.preventDefault()}
                        onMouseEnter={() => setHighlight(globalIdx)}
                        onClick={() => handleSelectItem(item)}
                        className={`flex-1 text-left bg-transparent border-none py-[5px] px-[10px] cursor-pointer text-sm truncate ${isSelected ? "text-white" : "text-gray-200"} ${isHighlighted ? "text-white" : ""}`}
                        title={item}
                      >
                        {item}
                      </button>
                      <button
                        type="button"
                        aria-label={`Remove ${item}`}
                        onMouseDown={(e) => e.preventDefault()}
                        onClick={() => handleDeleteItem(item)}
                        className="shrink-0 bg-transparent border-none py-[5px] px-[10px] cursor-pointer text-xs text-red-400 hover:text-red-300 font-bold"
                      >
                        ×
                      </button>
                    </li>
                  );
                })}
                {filteredItems.length === 0 && (
                  <li className="py-2 px-2 text-sm text-gray-400 italic">
                    {searchText.trim()
                      ? `No match — press Enter to add "${searchText.trim()}"`
                      : "Type to filter, or press Enter to add…"}
                  </li>
                )}
              </ul>
              {/* Saved bulk presets — act like ticking, not selecting; outside the folder list */}
              {visiblePresets.length > 0 && (
                <>
                  <div aria-hidden className="border-t border-white/10 mx-1.5" />
                  <ul className="list-none p-1 pt-0 m-0" aria-label="Bulk folder presets">
                    {visiblePresets.map((preset, i) => {
                      // Labels are unique by construction (collision suffixes), unlike folder joins.
                      const label = presetLabels[i] ?? preset.folders[0] ?? "preset";
                      return (
                        <li key={label} className="flex items-center">
                          <button
                            type="button"
                            onMouseDown={(e) => e.preventDefault()}
                            onClick={() => handleApplyPreset(preset)}
                            title={presetTooltip(preset)}
                            aria-label={`Bulk preset: ${label}`}
                            className="flex-1 text-left bg-transparent border-none py-[5px] px-[10px] cursor-pointer text-sm text-gray-200 hover:text-white focus:text-white focus:outline-none truncate"
                          >
                            {label}
                          </button>
                          <button
                            type="button"
                            aria-label={`Remove preset ${label}`}
                            onMouseDown={(e) => e.preventDefault()}
                            onClick={() => handleDeletePreset(preset)}
                            className="shrink-0 bg-transparent border-none py-[5px] px-[10px] cursor-pointer text-xs text-red-400 hover:text-red-300 font-bold"
                          >
                            ×
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </>
              )}
            </div>
            {/* Footer — item count + scroll hint (never scrolls) */}
            <div className="flex items-center justify-between gap-2 px-2 py-1.5 border-t border-white/10 bg-black/40 text-[11px] leading-none">
              <span className="text-gray-400 tabular-nums" aria-live="polite">
                {filteredItems.length} {filteredItems.length === 1 ? "item" : "items"}
              </span>
              <span className="text-gray-500 italic">scroll · ↑↓</span>
            </div>
            {/* Add-input — Enter appends (or finds) the folder, ticks it for bulk, clears */}
            <div className="flex items-center gap-2 px-2 py-1.5 border-t border-white/10 bg-black/40">
              <label className="sr-only" htmlFor="scs-subdir-add">
                Add a subdirectory to the bulk selection
              </label>
              <input
                id="scs-subdir-add"
                ref={addInputRef}
                type="text"
                value={addText}
                onChange={(e) => {
                  setAddText(e.target.value);
                  if (addError) setAddError("");
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    handleAddFromInput();
                  } else if (e.key === "Escape") {
                    setIsOpen(false);
                    setSearchText("");
                    setAddText("");
                    setAddError("");
                    setSearchError("");
                    setHighlight(-1);
                  }
                }}
                placeholder="add subfolder + Enter (bulk-select)"
                className="flex-1 min-w-0 bg-transparent border-none outline-none text-sm text-gray-200 placeholder:text-gray-500 py-[5px] cursor-text"
              />
              {addError && (
                <span className="text-[11px] text-red-400 shrink-0 max-w-[55%] truncate" role="alert" title={addError}>
                  {addError}
                </span>
              )}
            </div>
          </div>
          {/* Keyboard hint */}
          <div className="mt-1.5 text-[10px] leading-none text-gray-500 px-1">
            <span className="hidden sm:inline">↑↓ move · PgUp/PgDn scroll · Enter select/add · tick ☑ for bulk</span>
            <span className="sm:hidden">swipe · tap · ☑ = bulk</span>
          </div>
        </div>
      )}
      {saveError && (
        <span className="text-xs text-red-400 ml-2" role="alert">
          {saveError}
        </span>
      )}
    </div>
  );
}

export default Dropdown;
