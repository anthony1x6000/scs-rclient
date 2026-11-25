/** One rendered line of a unified text diff. */
export interface DiffRow {
  kind: "context" | "added" | "removed";
  oldLine: number | null;
  newLine: number | null;
  text: string;
}

/**
 * Line count ceiling per side. The LCS table is O(n*m) cells, so this caps a
 * preview at ~9 MB and keeps a pathological file from freezing the renderer.
 */
export const MAX_DIFF_LINES = 1500;

export function splitLines(text: string): string[] {
  if (text === "") return [];
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const lines = normalized.split("\n");
  // A single trailing newline is the normal POSIX ending, not an extra blank
  // line; keeping it would put a spurious blank row at the bottom of every diff.
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

export function canDiffText(oldText: string, newText: string): boolean {
  return (
    splitLines(oldText).length <= MAX_DIFF_LINES &&
    splitLines(newText).length <= MAX_DIFF_LINES
  );
}

/**
 * Classic LCS line diff (same family as `git diff` output, without hunk
 * headers or context folding). Removed rows carry only the old line number,
 * added rows only the new one, and context rows both.
 */
export function diffLines(oldText: string, newText: string): DiffRow[] {
  const oldLines = splitLines(oldText);
  const newLines = splitLines(newText);
  const n = oldLines.length;
  const m = newLines.length;
  if (n > MAX_DIFF_LINES || m > MAX_DIFF_LINES) {
    throw new Error(`File exceeds the ${MAX_DIFF_LINES}-line diff limit.`);
  }

  const width = m + 1;
  const dp = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i -= 1) {
    const oldLine = oldLines[i] ?? "";
    for (let j = m - 1; j >= 0; j -= 1) {
      const at = i * width + j;
      if (oldLine === (newLines[j] ?? "")) {
        dp[at] = (dp[(i + 1) * width + j + 1] ?? 0) + 1;
      } else {
        const down = dp[(i + 1) * width + j] ?? 0;
        const right = dp[i * width + j + 1] ?? 0;
        dp[at] = Math.max(down, right);
      }
    }
  }

  const rows: DiffRow[] = [];
  let i = 0;
  let j = 0;
  let oldLine = 1;
  let newLine = 1;
  while (i < n && j < m) {
    if ((oldLines[i] ?? "") === (newLines[j] ?? "")) {
      rows.push({
        kind: "context",
        oldLine,
        newLine,
        text: oldLines[i] ?? "",
      });
      i += 1;
      j += 1;
      oldLine += 1;
      newLine += 1;
    } else if ((dp[(i + 1) * width + j] ?? 0) >= (dp[i * width + j + 1] ?? 0)) {
      rows.push({ kind: "removed", oldLine, newLine: null, text: oldLines[i] ?? "" });
      i += 1;
      oldLine += 1;
    } else {
      rows.push({ kind: "added", oldLine: null, newLine, text: newLines[j] ?? "" });
      j += 1;
      newLine += 1;
    }
  }
  while (i < n) {
    rows.push({ kind: "removed", oldLine, newLine: null, text: oldLines[i] ?? "" });
    i += 1;
    oldLine += 1;
  }
  while (j < m) {
    rows.push({ kind: "added", oldLine: null, newLine, text: newLines[j] ?? "" });
    j += 1;
    newLine += 1;
  }
  return rows;
}

export interface DiffStats {
  added: number;
  removed: number;
}

export function diffStats(rows: readonly DiffRow[]): DiffStats {
  let added = 0;
  let removed = 0;
  for (const row of rows) {
    if (row.kind === "added") added += 1;
    else if (row.kind === "removed") removed += 1;
  }
  return { added, removed };
}
