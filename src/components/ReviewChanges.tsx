import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { excludedFoldersFor, loadAppSettings } from "../settings";
import { resolveExtensionFilter } from "../extensionFilter";
import {
  barSegments,
  countChangeFilters,
  countStatuses,
  filterChangeEntries,
  formatBytes,
  formatMtime,
  localEntries,
  remoteEntries,
  sideSize,
  statusGlyph,
  statusLabel,
  statusTextClass,
  type ChangeEntry,
  type ChangeFilter,
  type ChangeFilterCounts,
  type ChangePreview,
  type PreviewSide,
  type StatusCounts,
} from "../changes";
import {
  annotateWordDiffs,
  canDiffText,
  changedBlocks,
  diffLines,
  diffStats,
  stepChangedBlock,
  MAX_DIFF_LINES,
  type DiffRow,
  type WordSegment,
} from "../diff";

interface ReviewChangesProps {
  subdir: string;
  targetSubdir: string;
  onClose: () => void;
}

/**
 * GitHub-Desktop-style change review: a Local bar and a Remote bar whose
 * segments show removed/added/modified counts, two matching file lists, and a
 * preview pane with a line diff for text (metadata only for binaries).
 */
export default function ReviewChanges({
  subdir,
  targetSubdir,
  onClose,
}: ReviewChangesProps) {
  const [entries, setEntries] = useState<ChangeEntry[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [preview, setPreview] = useState<ChangePreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [filter, setFilter] = useState<ChangeFilter>("all");

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    let cancelled = false;
    setEntries(null);
    setLoadError(null);
    setSelected(null);
    setPreview(null);

    (async () => {
      try {
        const settings = await loadAppSettings();
        const extensionFilter = resolveExtensionFilter(subdir, settings.extensionFilterState);
        const changes = await invoke<ChangeEntry[]>("review_changes", {
          subdir,
          targetSubdir: targetSubdir || undefined,
          concurrency: settings.scanConcurrency,
          excludeFolders: excludedFoldersFor(
            settings.excludedFoldersByDir,
            subdir,
            settings.excludedFoldersLegacy,
          ),
          extensionFilter: extensionFilter ?? undefined,
        });
        if (cancelled) return;
        setEntries(changes);
      } catch (error: any) {
        if (cancelled) return;
        setLoadError(error?.message || String(error));
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [subdir, targetSubdir, reloadKey]);

  useEffect(() => {
    if (!selected) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    let cancelled = false;
    setPreview(null);
    setPreviewError(null);
    setPreviewLoading(true);
    invoke<ChangePreview>("read_change_preview", {
      subdir,
      relPath: selected,
      targetSubdir: targetSubdir || undefined,
    })
      .then((result) => {
        if (!cancelled) setPreview(result);
      })
      .catch((error: any) => {
        if (!cancelled) setPreviewError(error?.message || String(error));
      })
      .finally(() => {
        if (!cancelled) setPreviewLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [selected, subdir, targetSubdir]);

  const filterCounts = useMemo(() => countChangeFilters(entries ?? []), [entries]);
  const visibleEntries = useMemo(
    () => filterChangeEntries(entries ?? [], filter),
    [entries, filter],
  );
  const counts = useMemo(() => countStatuses(visibleEntries), [visibleEntries]);
  const leftEntries = useMemo(() => localEntries(visibleEntries), [visibleEntries]);
  const rightEntries = useMemo(() => remoteEntries(visibleEntries), [visibleEntries]);
  const selectedEntry = visibleEntries.find((entry) => entry.path === selected) ?? null;
  const summary =
    entries === null
      ? null
      : filter === "all"
        ? `${entries.length} changed file${entries.length === 1 ? "" : "s"}`
        : `${visibleEntries.length} of ${entries.length} changed files`;

  // A filter can hide the selected row; move to the first row still on screen
  // instead of leaving the preview pointing at an invisible file.
  useEffect(() => {
    if (visibleEntries.some((entry) => entry.path === selected)) return;
    setSelected(visibleEntries[0]?.path ?? null);
  }, [visibleEntries, selected]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-2"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Review local and remote changes"
        className="flex h-full max-h-[94vh] w-full max-w-[820px] flex-col border border-white/20 bg-[#0b0b0e] text-white shadow-2xl"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="flex shrink-0 items-center justify-between gap-2 border-b border-white/15 px-3 py-2">
          <div className="min-w-0">
            <h2 className="text-sm font-medium">Review changes</h2>
            <p className="truncate text-[10px] text-gray-400">
              {subdir ? `${subdir}/` : "mount root"}
              {summary !== null && ` · ${summary}`}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-1">
            <button
              type="button"
              onClick={() => setReloadKey((key) => key + 1)}
              className="border border-white/20 px-2 py-1 text-[10px] text-gray-200 hover:border-white/50"
            >
              Refresh
            </button>
            <button
              type="button"
              onClick={onClose}
              aria-label="Close review"
              className="border border-white/20 px-2 py-1 text-xs text-gray-200 hover:border-white/50"
            >
              ×
            </button>
          </div>
        </header>

        {entries === null && !loadError && (
          <div className="flex flex-1 items-center justify-center text-xs text-gray-400" role="status">
            Scanning local folder and remote drive…
          </div>
        )}

        {loadError && (
          <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 text-center">
            <p className="text-xs text-red-300">Could not review changes: {loadError}</p>
            <p className="text-[10px] text-gray-500">
              Check the WebDAV URL, credentials, and that the selected folder exists.
            </p>
          </div>
        )}

        {entries !== null && (
          <>
            <FilterBar filter={filter} counts={filterCounts} onChange={setFilter} />

            <div className="grid shrink-0 grid-cols-2 gap-2 px-2 pt-2">
              <SideBar side="local" counts={counts} />
              <SideBar side="remote" counts={counts} />
            </div>

            <div className="grid h-[28%] min-h-[96px] shrink-0 grid-cols-2 gap-2 px-2 pt-2">
              <ChangeList
                title="Local working copy"
                entries={leftEntries}
                side="local"
                selected={selected}
                onSelect={setSelected}
                emptyMessage={emptyMessage(filter)}
              />
              <ChangeList
                title="Remote drive"
                entries={rightEntries}
                side="remote"
                selected={selected}
                onSelect={setSelected}
                emptyMessage={emptyMessage(filter)}
              />
            </div>

            <PreviewPane
              entry={selectedEntry}
              preview={preview}
              loading={previewLoading}
              error={previewError}
            />
          </>
        )}
      </div>
    </div>
  );
}

const FILTER_LABELS: { key: ChangeFilter; label: string }[] = [
  { key: "all", label: "All" },
  { key: "content", label: "Content" },
  { key: "metadata", label: "Metadata" },
];

function emptyMessage(filter: ChangeFilter): string {
  if (filter === "metadata") return "No metadata-only changes";
  if (filter === "content") return "No content changes";
  return "No changes";
}

function FilterBar({
  filter,
  counts,
  onChange,
}: {
  filter: ChangeFilter;
  counts: ChangeFilterCounts;
  onChange: (filter: ChangeFilter) => void;
}) {
  return (
    <div
      className="flex shrink-0 flex-wrap items-center gap-1 px-2 pt-2"
      role="group"
      aria-label="Filter changes by kind"
    >
      <span className="mr-1 text-[10px] uppercase tracking-wide text-gray-500">Show</span>
      {FILTER_LABELS.map(({ key, label }) => {
        const active = key === filter;
        return (
          <button
            key={key}
            type="button"
            aria-pressed={active}
            onClick={() => onChange(key)}
            className={`border px-2 py-0.5 text-[10px] tabular-nums transition-colors ${
              active
                ? "border-white/70 bg-white/15 text-white"
                : "border-white/20 text-gray-300 hover:border-white/50"
            }`}
          >
            {label}{" "}
            <span className={active ? "text-gray-300" : "text-gray-500"}>{counts[key]}</span>
          </button>
        );
      })}
    </div>
  );
}

function SideBar({ side, counts }: { side: "local" | "remote"; counts: StatusCounts }) {
  const segments = barSegments(side, counts);
  const total = segments.reduce((sum, segment) => sum + segment.count, 0);
  return (
    <div className="border border-white/15 bg-black/30 px-2 py-1.5">
      <div className="flex items-baseline justify-between gap-2 text-[10px] uppercase tracking-wide text-gray-400">
        <span>{side === "local" ? "Local" : "Remote"}</span>
        <span>{total === 1 ? "1 changed file" : `${total} changed files`}</span>
      </div>
      <div className="mt-1 flex h-2 w-full overflow-hidden bg-white/10">
        {total > 0 &&
          segments.map(
            (segment) =>
              segment.count > 0 && (
                <div
                  key={segment.key}
                  className={segment.className}
                  style={{ width: `${(segment.count / total) * 100}%` }}
                  title={`${segment.count} ${segment.label}`}
                />
              ),
          )}
      </div>
      <div className="mt-1 flex flex-wrap gap-x-3 text-[10px] text-gray-300">
        {segments.map((segment) => (
          <span key={segment.key} className="inline-flex items-center gap-1">
            <span className={`inline-block h-1.5 w-1.5 ${segment.className}`} />
            {segment.count} {segment.label}
          </span>
        ))}
      </div>
    </div>
  );
}

function ChangeList({
  title,
  entries,
  side,
  selected,
  onSelect,
  emptyMessage,
}: {
  title: string;
  entries: ChangeEntry[];
  side: "local" | "remote";
  selected: string | null;
  onSelect: (path: string) => void;
  emptyMessage: string;
}) {
  return (
    <div className="flex min-h-0 flex-col border border-white/10 bg-black/20">
      <div className="shrink-0 border-b border-white/10 px-2 py-1 text-[10px] uppercase tracking-wide text-gray-400">
        {title} · {entries.length}
      </div>
      <div
        className="scs-scrollbar min-h-0 flex-1 overflow-y-auto overscroll-contain"
        role="listbox"
        aria-label={title}
      >
        {entries.length === 0 && (
          <p className="px-2 py-2 text-[11px] text-gray-500">{emptyMessage}</p>
        )}
        {entries.map((entry) => {
          const size = sideSize(entry, side);
          return (
            <button
              key={`${side}:${entry.path}`}
              type="button"
              role="option"
              aria-selected={entry.path === selected}
              onClick={() => onSelect(entry.path)}
              title={entry.path}
              className={`flex w-full items-center gap-1.5 px-2 py-0.5 text-left text-[11px] ${
                entry.path === selected ? "bg-white/15" : "hover:bg-white/5"
              }`}
            >
              <span className={`w-2 shrink-0 font-mono ${statusTextClass(entry.status)}`}>
                {statusGlyph(entry.status)}
              </span>
              <span className="truncate">{entry.path}</span>
              <span className="ml-auto shrink-0 text-[10px] tabular-nums text-gray-500">
                {size === null ? "—" : formatBytes(size)}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

interface PreviewProps {
  entry: ChangeEntry | null;
  preview: ChangePreview | null;
  loading: boolean;
  error: string | null;
}

function PreviewPane({ entry, preview, loading, error }: PreviewProps) {
  if (!entry) {
    return (
      <div className="flex flex-1 items-center justify-center px-4 text-center text-[11px] text-gray-500">
        Select a changed file to see what differs.
      </div>
    );
  }
  return <PreviewBody entry={entry} preview={preview} loading={loading} error={error} />;
}

function PreviewBody({
  entry,
  preview,
  loading,
  error,
}: Omit<PreviewProps, "entry"> & { entry: ChangeEntry }) {
  // Ignore a preview from the previous selection while the new one loads.
  const shownPreview = preview !== null && preview.path === entry.path ? preview : null;
  const localSide = shownPreview?.local ?? previewSideFromEntry(entry, "local");
  const remoteSide = shownPreview?.remote ?? previewSideFromEntry(entry, "remote");
  const diffable =
    (localSide.kind === "text" || localSide.kind === "missing") &&
    (remoteSide.kind === "text" || remoteSide.kind === "missing");
  const localText = localSide.text ?? "";
  const remoteText = remoteSide.text ?? "";
  const tooManyLines = useMemo(
    () => diffable && !canDiffText(localText, remoteText),
    [diffable, localText, remoteText],
  );
  const rows = useMemo(() => {
    if (shownPreview === null || !diffable || tooManyLines) return null;
    return annotateWordDiffs(diffLines(localText, remoteText));
  }, [shownPreview, diffable, tooManyLines, localText, remoteText]);
  const blocks = useMemo(() => (rows === null ? [] : changedBlocks(rows)), [rows]);
  const stats = useMemo(() => (rows === null ? null : diffStats(rows)), [rows]);
  const hasLineChanges = blocks.length > 0;

  const [blockIndex, setBlockIndex] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);
  const pendingScroll = useRef(false);

  useEffect(() => {
    pendingScroll.current = false;
    setBlockIndex(0);
    containerRef.current?.scrollTo({ top: 0, left: 0 });
  }, [entry.path]);

  useEffect(() => {
    setBlockIndex((current) =>
      Math.min(current, Math.max(blocks.length - 1, 0)),
    );
  }, [blocks.length]);

  const scrollToBlock = useCallback(
    (index: number) => {
      const block = blocks[index];
      if (block === undefined) return;
      containerRef.current
        ?.querySelector<HTMLElement>(`[data-row-index="${block.start}"]`)
        ?.scrollIntoView({ block: "center" });
    },
    [blocks],
  );

  const navigate = useCallback(
    (direction: 1 | -1) => {
      if (blocks.length === 0) return;
      const next = stepChangedBlock(blockIndex, blocks.length, direction);
      if (next === blockIndex) {
        scrollToBlock(blockIndex);
        return;
      }
      pendingScroll.current = true;
      setBlockIndex(next);
    },
    [blockIndex, blocks.length, scrollToBlock],
  );

  useEffect(() => {
    if (!pendingScroll.current) return;
    pendingScroll.current = false;
    scrollToBlock(blockIndex);
  }, [blockIndex, scrollToBlock]);

  // Alt+↑/↓ mirrors the arrow buttons while the diff has changes to visit.
  useEffect(() => {
    if (blocks.length === 0) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
      if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
      event.preventDefault();
      navigate(event.key === "ArrowDown" ? 1 : -1);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [blocks.length, navigate]);

  return (
    <section className="mt-2 flex min-h-0 flex-1 flex-col border-t border-white/15">
      <div className="flex shrink-0 items-center justify-between gap-2 px-2 py-1.5">
        <span className="min-w-0 truncate font-mono text-[11px]" title={entry.path}>
          {entry.path}
        </span>
        <div className="flex shrink-0 items-center gap-2">
          <span className={`text-[10px] ${statusTextClass(entry.status)}`}>
            {statusLabel(entry.status)}
          </span>
          {hasLineChanges && (
            <ChangeNav index={blockIndex} count={blocks.length} onStep={navigate} />
          )}
        </div>
      </div>

      <div className="flex shrink-0 gap-2 px-2 pb-1.5">
        <MetaCard label="Local" side={localSide} />
        <MetaCard label="Remote" side={remoteSide} />
        {stats && (
          <div className="flex shrink-0 items-center gap-2 border border-white/10 px-2 text-[10px] tabular-nums">
            <span className="text-emerald-300">+{stats.added}</span>
            <span className="text-red-300">−{stats.removed}</span>
          </div>
        )}
      </div>

      <div
        ref={containerRef}
        className="scs-scrollbar min-h-0 flex-1 overflow-auto overscroll-contain border-t border-white/10 font-mono text-[11px] leading-5"
      >
        {loading && <p className="px-2 py-2 text-gray-500">Loading preview…</p>}
        {!loading && error && (
          <p className="px-2 py-2 text-red-300">Could not load preview: {error}</p>
        )}
        {!loading && !error && shownPreview && !diffable && (
          <p className="px-2 py-2 text-gray-400">
            {previewReason(localSide, remoteSide)} Metadata is shown above.
          </p>
        )}
        {!loading && !error && shownPreview && diffable && tooManyLines && (
          <p className="px-2 py-2 text-gray-400">
            Too many lines to preview inline (limit {MAX_DIFF_LINES}); showing metadata
            only.
          </p>
        )}
        {!loading && !error && rows && !hasLineChanges && (
          <p className="px-2 py-2 text-gray-400">
            No line changes — only the file metadata differs.
          </p>
        )}
        {!loading && !error && rows && hasLineChanges && <DiffView rows={rows} />}
      </div>
    </section>
  );
}

function ChangeNav({
  index,
  count,
  onStep,
}: {
  index: number;
  count: number;
  onStep: (direction: 1 | -1) => void;
}) {
  return (
    <div
      className="flex items-center gap-1 border border-white/10 px-1 text-[10px]"
      role="group"
      aria-label="Change navigation"
    >
      <button
        type="button"
        onClick={() => onStep(-1)}
        aria-label="Previous change"
        title="Previous change (Alt+↑)"
        className="px-1 text-gray-300 hover:text-white"
      >
        ↑
      </button>
      <span className="min-w-8 text-center tabular-nums text-gray-400" aria-live="polite">
        {Math.min(index + 1, count)} / {count}
      </span>
      <button
        type="button"
        onClick={() => onStep(1)}
        aria-label="Next change"
        title="Next change (Alt+↓)"
        className="px-1 text-gray-300 hover:text-white"
      >
        ↓
      </button>
    </div>
  );
}

function MetaCard({ label, side }: { label: string; side: PreviewSide }) {
  return (
    <div className="min-w-0 flex-1 border border-white/10 px-2 py-1">
      <div className="text-[9px] uppercase tracking-wide text-gray-500">{label}</div>
      <div className="truncate text-[11px]">
        {side.kind === "missing" ? "Not present" : formatBytes(side.size)}
      </div>
      <div className="text-[9px] text-gray-500">{formatMtime(side.mtime)}</div>
    </div>
  );
}

function DiffView({ rows }: { rows: DiffRow[] }) {
  return (
    <div role="table" aria-label="Line differences" className="min-w-max">
      {rows.map((row, index) => (
        <div
          key={`${index}:${row.kind}`}
          role="row"
          data-row-index={index}
          className={`grid grid-cols-[3rem_3rem_1.25rem_1fr] ${
            row.kind === "added"
              ? "bg-emerald-950/40"
              : row.kind === "removed"
                ? "bg-red-950/40"
                : ""
          }`}
        >
          <span className="select-none pr-2 text-right text-gray-600 tabular-nums">
            {row.oldLine ?? ""}
          </span>
          <span className="select-none pr-2 text-right text-gray-600 tabular-nums">
            {row.newLine ?? ""}
          </span>
          <span
            className={`select-none ${
              row.kind === "added"
                ? "text-emerald-300"
                : row.kind === "removed"
                  ? "text-red-300"
                  : "text-gray-500"
            }`}
          >
            {row.kind === "added" ? "+" : row.kind === "removed" ? "−" : " "}
          </span>
          <span className={`whitespace-pre pr-3 ${lineTextClass(row.kind)}`}>
            {row.segments === undefined ? (
              row.text || " "
            ) : (
              row.segments.map((segment, segmentIndex) => (
                <span key={segmentIndex} className={wordSegmentClass(segment)}>
                  {segment.text}
                </span>
              ))
            )}
          </span>
        </div>
      ))}
    </div>
  );
}

function lineTextClass(kind: DiffRow["kind"]): string {
  if (kind === "added") return "text-emerald-100";
  if (kind === "removed") return "text-red-100";
  return "text-gray-300";
}

/** Changed words keep the line tint; the changed run gets its own solid tint. */
function wordSegmentClass(segment: WordSegment): string {
  if (segment.kind === "added") {
    return "bg-emerald-500/40 text-emerald-50";
  }
  if (segment.kind === "removed") {
    return "bg-red-500/40 text-red-50 line-through decoration-red-300/80";
  }
  return "";
}

/** Metadata-only fallback used until the preview request resolves. */
function previewSideFromEntry(entry: ChangeEntry, side: "local" | "remote"): PreviewSide {
  const size = side === "local" ? entry.localSize : entry.remoteSize;
  const mtime = side === "local" ? entry.localMtime : entry.remoteMtime;
  return {
    kind: size === null ? "missing" : "text",
    size: size ?? 0,
    mtime,
    text: null,
  };
}

function previewReason(local: PreviewSide, remote: PreviewSide): string {
  const side = local.kind !== "text" && local.kind !== "missing" ? local : remote;
  if (side.kind === "binary") return "Binary file — no line diff.";
  if (side.kind === "tooLarge") {
    return `File is larger than ${formatBytes(512 * 1024)} — no line diff.`;
  }
  return "No inline preview available.";
}

