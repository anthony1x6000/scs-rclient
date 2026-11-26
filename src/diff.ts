/** One run of identical or changed words inside a rendered diff line. */
export interface WordSegment {
  kind: "equal" | "added" | "removed";
  text: string;
}

/** Both sides of a word-level diff, ready to render on their own lines. */
export interface WordDiff {
  old: WordSegment[];
  new: WordSegment[];
  /** Share of unchanged words; punctuation and whitespace do not count. */
  similarity: number;
}

/** One rendered line of a unified text diff. */
export interface DiffRow {
  kind: "context" | "added" | "removed";
  oldLine: number | null;
  newLine: number | null;
  text: string;
  /** Present when this row is paired with a similar row and differs by words. */
  segments?: WordSegment[];
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

/** Word, punctuation, and whitespace tokens that reconstruct the input. */
const WORD_TOKEN = /\s+|[\p{L}\p{N}_]+|[^\s\p{L}\p{N}_]/gu;

/**
 * A line pair below this share of unchanged words reads as a rewrite, so
 * highlighting every token would be noise; such pairs keep the line tint.
 */
const WORD_DIFF_MIN_SIMILARITY = 0.5;

/** Token ceiling per side; word LCS on a minified line would be quadratic. */
const MAX_WORD_TOKENS = 400;

/**
 * Ceiling on candidate pairs tried per changed block. Past it, pairing falls
 * back to matching rows by position so a huge block cannot stall the renderer.
 */
const MAX_WORD_MATCH_CANDIDATES = 16;

function tokenize(text: string): string[] {
  return text.match(WORD_TOKEN) ?? [];
}

/** Letters, digits, and underscores; punctuation is matched but not scored. */
function isWordToken(token: string): boolean {
  return /^[\p{L}\p{N}_]+$/u.test(token);
}

function pushSegment(
  segments: WordSegment[],
  kind: WordSegment["kind"],
  text: string,
): void {
  const last = segments[segments.length - 1];
  if (last !== undefined && last.kind === kind) {
    last.text += text;
  } else {
    segments.push({ kind, text });
  }
}

/**
 * Word-level LCS diff for one removed/added line pair. Returns null when the
 * lines share too little, one side is empty, or the token count is past the
 * cap — callers then fall back to the whole-line tint.
 */
export function diffWords(oldText: string, newText: string): WordDiff | null {
  const oldTokens = tokenize(oldText);
  const newTokens = tokenize(newText);
  const n = oldTokens.length;
  const m = newTokens.length;
  if (n === 0 || m === 0 || n > MAX_WORD_TOKENS || m > MAX_WORD_TOKENS) return null;

  const width = m + 1;
  const dp = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      const at = i * width + j;
      if (oldTokens[i] === newTokens[j]) {
        dp[at] = (dp[(i + 1) * width + j + 1] ?? 0) + 1;
      } else {
        dp[at] = Math.max(
          dp[(i + 1) * width + j] ?? 0,
          dp[i * width + j + 1] ?? 0,
        );
      }
    }
  }

  const oldSegments: WordSegment[] = [];
  const newSegments: WordSegment[] = [];
  let oldWords = 0;
  let newWords = 0;
  let commonWords = 0;
  for (const token of oldTokens) if (isWordToken(token)) oldWords += 1;
  for (const token of newTokens) if (isWordToken(token)) newWords += 1;

  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    const oldToken = oldTokens[i];
    const newToken = newTokens[j];
    if (oldToken === undefined || newToken === undefined) break;
    if (oldToken === newToken) {
      pushSegment(oldSegments, "equal", oldToken);
      pushSegment(newSegments, "equal", newToken);
      if (isWordToken(oldToken)) commonWords += 1;
      i += 1;
      j += 1;
    } else if (
      (dp[(i + 1) * width + j] ?? 0) >= (dp[i * width + j + 1] ?? 0)
    ) {
      pushSegment(oldSegments, "removed", oldToken);
      i += 1;
    } else {
      pushSegment(newSegments, "added", newToken);
      j += 1;
    }
  }
  while (i < n) {
    const token = oldTokens[i];
    if (token === undefined) break;
    pushSegment(oldSegments, "removed", token);
    i += 1;
  }
  while (j < m) {
    const token = newTokens[j];
    if (token === undefined) break;
    pushSegment(newSegments, "added", token);
    j += 1;
  }

  const totalWords = oldWords + newWords;
  const similarity = totalWords === 0 ? 0 : (2 * commonWords) / totalWords;
  if (similarity < WORD_DIFF_MIN_SIMILARITY) return null;
  return { old: oldSegments, new: newSegments, similarity };
}

/**
 * Pairs removed and added rows inside each changed block and attaches word
 * segments where the pair is a genuine edit rather than a rewrite. Small
 * blocks choose the most similar counterpart; larger blocks fall back to
 * positional pairing so the search stays bounded. Pairs below the similarity
 * floor keep their whole-line tint.
 */
export function annotateWordDiffs(rows: readonly DiffRow[]): DiffRow[] {
  const annotated = rows.map((row) => ({ ...row }));
  let index = 0;
  while (index < annotated.length) {
    if (annotated[index]?.kind === "context") {
      index += 1;
      continue;
    }
    const start = index;
    while (index < annotated.length && annotated[index]?.kind !== "context") {
      index += 1;
    }

    const removedIndices: number[] = [];
    const addedIndices: number[] = [];
    for (let rowIndex = start; rowIndex < index; rowIndex += 1) {
      const row = annotated[rowIndex];
      if (row?.kind === "removed") removedIndices.push(rowIndex);
      else if (row?.kind === "added") addedIndices.push(rowIndex);
    }

    const candidateCount = removedIndices.length * addedIndices.length;
    if (candidateCount > 0 && candidateCount <= MAX_WORD_MATCH_CANDIDATES) {
      const matches: { removed: number; added: number; words: WordDiff }[] = [];
      for (const removedIndex of removedIndices) {
        const removedRow = annotated[removedIndex];
        if (removedRow === undefined) continue;
        for (const addedIndex of addedIndices) {
          const addedRow = annotated[addedIndex];
          if (addedRow === undefined) continue;
          const words = diffWords(removedRow.text, addedRow.text);
          if (words !== null) {
            matches.push({ removed: removedIndex, added: addedIndex, words });
          }
        }
      }
      // Strongest textual match first; ties prefer rows closest together.
      matches.sort(
        (a, b) =>
          b.words.similarity - a.words.similarity ||
          Math.abs(a.removed - a.added) - Math.abs(b.removed - b.added),
      );

      const usedRemoved = new Set<number>();
      const usedAdded = new Set<number>();
      for (const match of matches) {
        if (usedRemoved.has(match.removed) || usedAdded.has(match.added)) continue;
        attachWordDiff(annotated, match.removed, match.added, match.words);
        usedRemoved.add(match.removed);
        usedAdded.add(match.added);
      }
    } else {
      const pairCount = Math.min(removedIndices.length, addedIndices.length);
      for (let pair = 0; pair < pairCount; pair += 1) {
        const removedIndex = removedIndices[pair];
        const addedIndex = addedIndices[pair];
        if (removedIndex === undefined || addedIndex === undefined) continue;
        const removedRow = annotated[removedIndex];
        const addedRow = annotated[addedIndex];
        if (removedRow === undefined || addedRow === undefined) continue;
        const words = diffWords(removedRow.text, addedRow.text);
        if (words !== null) attachWordDiff(annotated, removedIndex, addedIndex, words);
      }
    }
  }
  return annotated;
}

function attachWordDiff(
  rows: DiffRow[],
  removedIndex: number,
  addedIndex: number,
  words: WordDiff,
): void {
  const removedRow = rows[removedIndex];
  const addedRow = rows[addedIndex];
  if (removedRow === undefined || addedRow === undefined) return;
  removedRow.segments = words.old;
  addedRow.segments = words.new;
}

/** A run of consecutive added/removed rows, with `end` exclusive. */
export interface ChangedBlock {
  start: number;
  end: number;
}

export function changedBlocks(rows: readonly DiffRow[]): ChangedBlock[] {
  const blocks: ChangedBlock[] = [];
  let start = -1;
  for (let index = 0; index <= rows.length; index += 1) {
    const changed = index < rows.length && rows[index]?.kind !== "context";
    if (changed && start < 0) start = index;
    if (!changed && start >= 0) {
      blocks.push({ start, end: index });
      start = -1;
    }
  }
  return blocks;
}

/** Wraps around both ends; an empty review returns -1. */
export function stepChangedBlock(
  current: number,
  total: number,
  direction: 1 | -1,
): number {
  if (total <= 0) return -1;
  return (current + direction + total) % total;
}
