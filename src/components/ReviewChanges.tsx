import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { excludedFoldersFor, loadAppSettings } from "../settings";
import { resolveExtensionFilter } from "../extensionFilter";
import {
  barSegments,
  countStatuses,
  formatBytes,
  formatMtime,
  localEntries,
  remoteEntries,
  sideSize,
  statusGlyph,
  statusLabel,
  statusTextClass,
  type ChangeEntry,
  type ChangePreview,
  type PreviewSide,
  type StatusCounts,
} from "../changes";
import { canDiffText, diffLines, diffStats, MAX_DIFF_LINES, type DiffRow } from "../diff";

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
        setSelected(changes[0]?.path ?? null);
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

  const counts = useMemo(() => countStatuses(entries ?? []), [entries]);
  const leftEntries = useMemo(() => localEntries(entries ?? []), [entries]);
  const rightEntries = useMemo(() => remoteEntries(entries ?? []), [entries]);
  const selectedEntry = entries?.find((entry) => entry.path === selected) ?? null;

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
              {entries !== null && ` · ${entries.length} changed file${entries.length === 1 ? "" : "s"}`}
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
              />
              <ChangeList
                title="Remote drive"
                entries={rightEntries}
                side="remote"
                selected={selected}
                onSelect={setSelected}
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
}: {
  title: string;
  entries: ChangeEntry[];
  side: "local" | "remote";
  selected: string | null;
  onSelect: (path: string) => void;
}) {
  return (
    <div className="flex min-h-0 flex-col border border-white/10 bg-black/20">
      <div className="shrink-0 border-b border-white/10 px-2 py-1 text-[10px] uppercase tracking-wide text-gray-400">
        {title} · {entries.length}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto" role="listbox" aria-label={title}>
        {entries.length === 0 && (
          <p className="px-2 py-2 text-[11px] text-gray-500">No changes</p>
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

function PreviewPane({
  entry,
  preview,
  loading,
  error,
}: {
  entry: ChangeEntry | null;
  preview: ChangePreview | null;
  loading: boolean;
  error: string | null;
}) {
  if (!entry) {
    return (
      <div className="flex flex-1 items-center justify-center px-4 text-center text-[11px] text-gray-500">
        Select a changed file to see what differs.
      </div>
    );
  }

  const localSide = preview?.local ?? previewSideFromEntry(entry, "local");
  const remoteSide = preview?.remote ?? previewSideFromEntry(entry, "remote");
  const diffable =
    (localSide.kind === "text" || localSide.kind === "missing") &&
    (remoteSide.kind === "text" || remoteSide.kind === "missing");
  const localText = localSide.text ?? "";
  const remoteText = remoteSide.text ?? "";
  const tooManyLines = diffable && !canDiffText(localText, remoteText);
  const rows =
    diffable && !tooManyLines && preview ? diffLines(localText, remoteText) : null;
  const stats = rows ? diffStats(rows) : null;
  const hasLineChanges = stats !== null && stats.added + stats.removed > 0;

  return (
    <section className="mt-2 flex min-h-0 flex-1 flex-col border-t border-white/15">
      <div className="flex shrink-0 items-center justify-between gap-2 px-2 py-1.5">
        <span className="min-w-0 truncate font-mono text-[11px]" title={entry.path}>
          {entry.path}
        </span>
        <span className={`shrink-0 text-[10px] ${statusTextClass(entry.status)}`}>
          {statusLabel(entry.status)}
        </span>
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

      <div className="min-h-0 flex-1 overflow-auto border-t border-white/10 font-mono text-[11px] leading-5">
        {loading && <p className="px-2 py-2 text-gray-500">Loading preview…</p>}
        {!loading && error && (
          <p className="px-2 py-2 text-red-300">Could not load preview: {error}</p>
        )}
        {!loading && !error && preview && !diffable && (
          <p className="px-2 py-2 text-gray-400">
            {previewReason(localSide, remoteSide)} Metadata is shown above.
          </p>
        )}
        {!loading && !error && preview && diffable && tooManyLines && (
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
          <span
            className={`whitespace-pre pr-3 ${
              row.kind === "added"
                ? "text-emerald-100"
                : row.kind === "removed"
                  ? "text-red-100"
                  : "text-gray-300"
            }`}
          >
            {row.text || " "}
          </span>
        </div>
      ))}
    </div>
  );
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

