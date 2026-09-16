import { useState, useEffect, useRef, useMemo, useLayoutEffect, useCallback } from "react";
import { getSubdirectories, getSelectedSubdir, setSubdirectories, setSelectedSubdir } from "../settings";

interface DropdownProps {
  onSelect?: (item: string) => void;
  variant?: "pocket" | "marquee" | "palette";
}

const ROW_H = 34; // px approx per row (py5 + text-sm)
const MIN_PAGE = 3;
const MAX_PAGE_SIZE = 9;
const DEFAULT_PAGE = 7;

function Dropdown({ onSelect, variant = "pocket" }: DropdownProps) {
  const [items, setItems] = useState<string[]>([]);
  const [selectedItem, setSelectedItem] = useState<string>("");
  const [isOpen, setIsOpen] = useState<boolean>(false);
  const [searchText, setSearchText] = useState<string>("");
  const [saveError, setSaveError] = useState<string>("");
  const [page, setPage] = useState(0);
  const [highlight, setHighlight] = useState(-1);
  const [availableHeight, setAvailableHeight] = useState(280);

  const inputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const listWrapRef = useRef<HTMLDivElement>(null);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;

  // Load
  useEffect(() => {
    async function loadStoredSettings() {
      try {
        const [savedItems, savedSelected] = await Promise.all([
          getSubdirectories(),
          getSelectedSubdir(),
        ]);
        setItems(savedItems);
        setSelectedItem(savedSelected);
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
    setPage(0);
    setHighlight(-1);
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
    // keep page in bounds after delete
    void saveSettings(updatedItems, updatedSelected);
  };

  const filteredItems = useMemo(
    () => items.filter((item) => item.toLowerCase().includes(searchText.toLowerCase())),
    [items, searchText]
  );

  // Derive paging
  const pageSize = useMemo(() => {
    if (variant === "marquee") return 12; // horizontal variant not paged same way
    const computed = Math.floor(availableHeight / ROW_H);
    return Math.max(MIN_PAGE, Math.min(MAX_PAGE_SIZE, computed || DEFAULT_PAGE));
  }, [availableHeight, variant]);

  const pageCount = Math.max(1, Math.ceil(filteredItems.length / pageSize));
  const clampedPage = Math.min(page, pageCount - 1);
  const visibleItems = useMemo(
    () => filteredItems.slice(clampedPage * pageSize, (clampedPage + 1) * pageSize),
    [filteredItems, clampedPage, pageSize]
  );

  // Reset page/highlight when filter changes
  useEffect(() => {
    setPage(0);
    setHighlight(-1);
  }, [searchText]);

  // Keep page in bounds when items change (delete etc.)
  useEffect(() => {
    if (page >= pageCount) setPage(pageCount - 1);
  }, [page, pageCount]);

  // If highlighted item is off current page, auto-jump page to show it
  useEffect(() => {
    if (highlight < 0) return;
    const targetPage = Math.floor(highlight / pageSize);
    if (targetPage !== clampedPage) setPage(targetPage);
  }, [highlight, pageSize, clampedPage]);

  // Wheel -> page (hidden scrollbar alternative)
  const wheelLock = useRef(0);
  const onWheel = (e: React.WheelEvent) => {
    if (filteredItems.length <= pageSize) return;
    const now = Date.now();
    if (now - wheelLock.current < 180) return;
    if (Math.abs(e.deltaY) < 8 && Math.abs(e.deltaX) < 8) return;
    // Prefer vertical, but also treat horizontal wheel as paging
    const delta = Math.abs(e.deltaY) > Math.abs(e.deltaX) ? e.deltaY : e.deltaX;
    if (delta > 0 && clampedPage < pageCount - 1) {
      wheelLock.current = now;
      e.preventDefault();
      setPage((p) => Math.min(p + 1, pageCount - 1));
    } else if (delta < 0 && clampedPage > 0) {
      wheelLock.current = now;
      e.preventDefault();
      setPage((p) => Math.max(p - 1, 0));
    }
  };

  // Touch swipe for page
  const touchStart = useRef<number | null>(null);
  const onTouchStart = (e: React.TouchEvent) => {
    touchStart.current = e.touches[0]?.clientY ?? null;
  };
  const onTouchEnd = (e: React.TouchEvent) => {
    if (touchStart.current == null) return;
    const end = e.changedTouches[0]?.clientY ?? touchStart.current;
    const dy = touchStart.current - end;
    touchStart.current = null;
    if (Math.abs(dy) < 24) return;
    if (dy > 0 && clampedPage < pageCount - 1) setPage((p) => p + 1);
    else if (dy < 0 && clampedPage > 0) setPage((p) => p - 1);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    const total = filteredItems.length;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (total === 0) return;
      setHighlight((h) => {
        const next = h < 0 ? 0 : Math.min(h + 1, total - 1);
        return next;
      });
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (total === 0) return;
      setHighlight((h) => {
        const next = h < 0 ? total - 1 : Math.max(h - 1, 0);
        return next;
      });
    } else if (e.key === "PageDown") {
      e.preventDefault();
      setPage((p) => Math.min(p + 1, pageCount - 1));
      setHighlight((h) => {
        const base = clampedPage * pageSize;
        return h < 0 ? base : Math.min(total - 1, h + pageSize);
      });
    } else if (e.key === "PageUp") {
      e.preventDefault();
      setPage((p) => Math.max(p - 1, 0));
      setHighlight((h) => (h < 0 ? 0 : Math.max(0, h - pageSize)));
    } else if (e.key === "Home" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      setPage(0);
      setHighlight(0);
    } else if (e.key === "End" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      setPage(pageCount - 1);
      setHighlight(total - 1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      // If a list item is highlighted, select it
      if (highlight >= 0 && highlight < filteredItems.length) {
        const chosen = filteredItems[highlight];
        if (chosen) {
          handleSelectItem(chosen);
          return;
        }
      }
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
        setPage(0);
        setHighlight(-1);
        void saveSettings(updatedItems, trimmed);
      }
    } else if (e.key === "Escape") {
      setIsOpen(false);
      setSearchText("");
      setPage(0);
      setHighlight(-1);
    }
  };

  const handleBlur = (e: React.FocusEvent<HTMLDivElement>) => {
    if (!e.currentTarget.contains(e.relatedTarget)) {
      setIsOpen(false);
      setSearchText("");
      setHighlight(-1);
      // keep page at 0 for next open
      setPage(0);
    }
  };

  const hasOverflow = filteredItems.length > pageSize;
  const showTopFade = hasOverflow && clampedPage > 0;
  const showBottomFade = hasOverflow && clampedPage < pageCount - 1;
  const startIdx = filteredItems.length === 0 ? 0 : clampedPage * pageSize + 1;
  const endIdx = Math.min((clampedPage + 1) * pageSize, filteredItems.length);

  // Selected item may live off current page — offer jump hint
  const selectedOffPage =
    selectedItem && filteredItems.includes(selectedItem) && !visibleItems.includes(selectedItem);
  const selectedPage = selectedItem ? Math.floor(filteredItems.indexOf(selectedItem) / pageSize) : -1;

  return (
    <div className="relative inline-block" onBlur={handleBlur} ref={containerRef}>
      <label className="sr-only" htmlFor="scs-subdir-search">
        Sync subdirectory
      </label>
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
          aria-controls="scs-subdir-listbox"
          aria-activedescendant={
            highlight >= 0 ? `scs-opt-${highlight}` : undefined
          }
          className="text-5xl text-gray-300 cursor-text bg-transparent border-none outline-none w-full placeholder:text-gray-500"
        />
      ) : (
        <button
          type="button"
          onClick={() => {
            setIsOpen(true);
            setSearchText("");
            setHighlight(-1);
            recomputeAvailable();
          }}
          aria-expanded={isOpen}
          aria-haspopup="listbox"
          aria-label={
            selectedItem
              ? `Sync subdirectory: ${selectedItem}. Activate to change.`
              : "Select sync subdirectory"
          }
          className="text-5xl text-gray-300 lowercase cursor-pointer text-left max-w-[28vw] truncate"
          title={selectedItem || "select"}
        >
          {selectedItem || "select"}
        </button>
      )}

      {isOpen && (
        <div
          className="absolute bottom-full left-0 mb-2 z-20"
          // keep dropdown width reasonable, not full viewport
          style={{ width: "min(360px, 86vw)" }}
        >
          <div
            ref={listWrapRef}
            onWheel={onWheel}
            onTouchStart={onTouchStart}
            onTouchEnd={onTouchEnd}
            className="relative bg-black/85 backdrop-blur-md border border-white/15 rounded-sm shadow-[0_12px_40px_rgba(0,0,0,0.6)] overflow-hidden"
          >
            {/* Top fade */}
            <div
              aria-hidden
              className={`pointer-events-none absolute inset-x-0 top-0 h-6 bg-gradient-to-b from-black/70 to-transparent transition-opacity duration-200 z-10 ${showTopFade ? "opacity-100" : "opacity-0"}`}
            />
            {/* Scroll viewport — hidden scrollbar, but still scrollable programmatically */}
            <div
              className="no-scrollbar overflow-y-auto overflow-x-hidden overscroll-contain scroll-smooth"
              style={{ maxHeight: pageSize * ROW_H + 2 }}
              tabIndex={-1}
            >
              <ul
                id="scs-subdir-listbox"
                className="list-none p-1 m-0"
                role="listbox"
                aria-label="Sync subdirectories"
              >
                {visibleItems.map((item) => {
                  const globalIdx = filteredItems.indexOf(item);
                  const isSelected = item === selectedItem;
                  const isHighlighted = globalIdx === highlight;
                  return (
                    <li
                      key={item}
                      id={`scs-opt-${globalIdx}`}
                      className={`flex items-center rounded-sm ${isHighlighted ? "bg-white/10 ring-1 ring-white/20" : ""} ${isSelected ? "bg-white/[0.06]" : ""}`}
                      role="option"
                      aria-selected={isSelected}
                    >
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
                      ? `Press Enter to add "${searchText.trim()}"`
                      : "Type to search or add a subdirectory…"}
                  </li>
                )}
                {/* Off-page selected hint */}
                {selectedOffPage && filteredItems.length > 0 && (
                  <li className="mt-1 py-1.5 px-2 text-[11px] text-amber-200/80 bg-amber-950/20 border border-amber-500/20 rounded-sm">
                    Selected “{selectedItem}” is on page {selectedPage + 1} ·{" "}
                    <button
                      type="button"
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => {
                        setPage(selectedPage);
                        setHighlight(filteredItems.indexOf(selectedItem));
                      }}
                      className="underline underline-offset-2 hover:text-amber-100 cursor-pointer"
                    >
                      jump there
                    </button>
                  </li>
                )}
              </ul>
            </div>
            {/* Bottom fade */}
            <div
              aria-hidden
              className={`pointer-events-none absolute inset-x-0 bottom-[34px] h-6 bg-gradient-to-t from-black/70 to-transparent transition-opacity duration-200 z-10 ${showBottomFade ? "opacity-100" : "opacity-0"}`}
            />
            {/* Footer — pagination + hint (never scrolls) */}
            <div className="flex items-center justify-between gap-2 px-2 py-1.5 border-t border-white/10 bg-black/40 text-[11px] leading-none">
              <span className="text-gray-400 tabular-nums" aria-live="polite">
                {filteredItems.length === 0
                  ? "0 items"
                  : filteredItems.length <= pageSize
                    ? `${filteredItems.length} ${filteredItems.length === 1 ? "item" : "items"}`
                    : `${startIdx}–${endIdx} of ${filteredItems.length}`}
                {hasOverflow && filteredItems.length > pageSize * 1.5 && (
                  <span className="ml-1.5 text-gray-500 hidden sm:inline">· keep typing to filter</span>
                )}
              </span>
              {pageCount > 1 ? (
                <span className="flex items-center gap-1 shrink-0" role="navigation" aria-label="Pagination">
                  <button
                    type="button"
                    aria-label="Previous page"
                    disabled={clampedPage === 0}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => setPage((p) => Math.max(0, p - 1))}
                    className="w-6 h-6 grid place-items-center rounded-sm border border-white/15 hover:border-white/30 disabled:opacity-30 disabled:cursor-not-allowed cursor-pointer bg-white/5 hover:bg-white/10"
                  >
                    ‹
                  </button>
                  <span className="flex items-center gap-1 px-1" aria-hidden>
                    {Array.from({ length: Math.min(pageCount, 7) }).map((_, i) => {
                      // windowed dots when many pages
                      let pageIdx = i;
                      if (pageCount > 7) {
                        const start = Math.max(0, Math.min(pageCount - 7, clampedPage - 3));
                        pageIdx = start + i;
                      }
                      const active = pageIdx === clampedPage;
                      return (
                        <span
                          key={pageIdx}
                          className={`h-1.5 rounded-full transition-all ${active ? "w-4 bg-white" : "w-1.5 bg-white/30"}`}
                        />
                      );
                    })}
                    {pageCount > 7 && <span className="text-gray-500 text-[10px] ml-0.5">·{pageCount}</span>}
                  </span>
                  <button
                    type="button"
                    aria-label="Next page"
                    disabled={clampedPage >= pageCount - 1}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => setPage((p) => Math.min(pageCount - 1, p + 1))}
                    className="w-6 h-6 grid place-items-center rounded-sm border border-white/15 hover:border-white/30 disabled:opacity-30 disabled:cursor-not-allowed cursor-pointer bg-white/5 hover:bg-white/10"
                  >
                    ›
                  </button>
                </span>
              ) : (
                <span className="text-gray-500 italic">scroll · swipe · arrows</span>
              )}
            </div>
          </div>
          {/* Keyboard hint */}
          <div className="mt-1.5 text-[10px] leading-none text-gray-500 px-1">
            <span className="hidden sm:inline">↑↓ highlight · PgUp/PgDn page · wheel/swipe · Enter select</span>
            <span className="sm:hidden">tap · swipe to page</span>
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
