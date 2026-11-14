// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A layer as text: what `Stage.exportLayer(layer, "usda")` writes, read
// again after each change, and which of its lines the change touched,
// against the text read before it. This is what an edit authored, in the
// layer it went to: the opinion as USDA. Large layers are cut at
// `MAX_TEXT_BYTES`, so a page never holds more text than it can show.
import type { HistoryStep, Layer, Stage } from "@forest-rs/layerstack";

/** The most bytes of a layer's text read: past this the text is cut at a line, and says so. */
export const MAX_TEXT_BYTES = 4 * 1024 * 1024;

/**
 * The most differences {@link lineChanges} traces line by line: past
 * this, the changed region is marked whole, from the first changed line
 * to the last.
 */
const MAX_EDITS = 1000;

/**
 * The most characters of one line drawn: a mesh's points or indices are one
 * line of USDA, a million characters and more, and drawing every token of
 * it holds the page for minutes.
 */
export const MAX_LINE_CHARS = 2000;

/** A line as drawn: its first {@link MAX_LINE_CHARS} characters, and how many more it has. */
export interface DrawnLine {
  readonly text: string;
  /** The characters left out; 0 when the line is drawn whole. */
  readonly more: number;
}

/** `line` as the Text tab draws it ({@link DrawnLine}). */
export function drawnLine(line: string): DrawnLine {
  return line.length <= MAX_LINE_CHARS ? { text: line, more: 0 } : { text: line.slice(0, MAX_LINE_CHARS), more: line.length - MAX_LINE_CHARS };
}

/** Which lines of a text changed against the text before it. */
export interface LineChanges {
  /** Indices of lines that are new or changed, ascending. */
  readonly added: readonly number[];
  /** Indices of lines that lines were removed just before (the text's length for its end), ascending. */
  readonly removedBefore: readonly number[];
  /** How many lines of the text before are gone. */
  readonly removed: number;
}

/** No change. */
export const NO_CHANGES: LineChanges = { added: [], removedBefore: [], removed: 0 };

/** A layer's text as last read. */
export interface LayerTextRead {
  readonly layer: Layer;
  /** Its lines (the text cut at {@link MAX_TEXT_BYTES} when {@link LayerTextRead.truncated}). */
  readonly lines: readonly string[];
  /** The bytes of the whole text. */
  readonly bytes: number;
  /** Whether the lines are only the first part of the text. */
  readonly truncated: boolean;
  /** What changed since the text read before this one; nothing for the first read. */
  readonly changes: LineChanges;
  /**
   * How many steps of the history that write the layer lie between the two
   * reads `changes` compares (undone or new): 1 when it is what the last change
   * did, more when the layer was not read between changes (the Text tab
   * was not shown), 0 when the reads were not given the history.
   */
  readonly steps: number;
  /** How long exporting and reading it took, in milliseconds. */
  readonly ms: number;
}

/**
 * The lines of `before` and `after` that differ: the lines of `after`
 * not in a longest common subsequence of the two (Myers' O(ND) diff, after
 * the common head and tail are set aside), and where lines of `before`
 * were dropped. Past `maxEdits` differences, everything between the
 * common head and tail is marked changed.
 */
export function lineChanges(before: readonly string[], after: readonly string[], maxEdits = MAX_EDITS): LineChanges {
  let head = 0;
  while (head < before.length && head < after.length && before[head] === after[head]) head++;
  let tail = 0;
  while (tail < before.length - head && tail < after.length - head && before[before.length - 1 - tail] === after[after.length - 1 - tail]) tail++;
  const n = before.length - head - tail;
  const m = after.length - head - tail;
  if (n === 0 && m === 0) return NO_CHANGES;
  const range = (from: number, count: number) => Array.from({ length: count }, (_, index) => from + index);
  if (n === 0) return { added: range(head, m), removedBefore: [], removed: 0 };
  if (m === 0) return { added: [], removedBefore: [head], removed: n };
  const script = myers(before, after, head, n, m, maxEdits);
  if (script === null) return { added: range(head, m), removedBefore: [head + m], removed: n };
  const added: number[] = [];
  const removedBefore = new Set<number>();
  let removed = 0;
  for (const step of script) {
    if (step.kind === "insert") added.push(head + step.after);
    else {
      removed += 1;
      removedBefore.add(head + step.after);
    }
  }
  return { added, removedBefore: [...removedBefore].sort((a, b) => a - b), removed };
}

/** One step of an edit script: a line of `after` inserted, or a line of `before` deleted before line `after` of `after`. */
interface Step {
  readonly kind: "insert" | "delete";
  /** The line of `after` inserted, or the line of `after` the deletion comes before. */
  readonly after: number;
}

/**
 * The shortest edit script from `before` to `after` over the `n` and `m`
 * lines from `offset` (Myers 1986, keeping each round's furthest reaching
 * paths to trace back); `null` past `maxEdits` differences.
 */
function myers(before: readonly string[], after: readonly string[], offset: number, n: number, m: number, maxEdits: number): Step[] | null {
  const max = Math.min(n + m, maxEdits);
  // `v[k + max]` is the furthest x on diagonal k; `trace[d]` the v of round d, restricted to its diagonals.
  const v = new Int32Array(2 * max + 2);
  const trace: Int32Array[] = [];
  for (let d = 0; d <= max; d++) {
    trace.push(v.slice(max - d, max + d + 2));
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[k - 1 + max]! < v[k + 1 + max]!) ? v[k + 1 + max]! : v[k - 1 + max]! + 1;
      let y = x - k;
      while (x < n && y < m && before[offset + x] === after[offset + y]) {
        x++;
        y++;
      }
      v[k + max] = x;
      if (x >= n && y >= m) return backtrack(trace, d, n, m);
    }
  }
  return null;
}

/** The steps of the path that reached (n, m) in round `found`, read back from each round's diagonals. */
function backtrack(trace: readonly Int32Array[], found: number, n: number, m: number): Step[] {
  const steps: Step[] = [];
  let x = n;
  let y = m;
  for (let d = found; d > 0; d--) {
    // Round d's v, as it was before round d ran: diagonals -d..d at `index + d`.
    const v = trace[d]!;
    const at = (k: number) => v[k + d]!;
    const k = x - y;
    const down = k === -d || (k !== d && at(k - 1) < at(k + 1));
    const previousK = down ? k + 1 : k - 1;
    const previousX = at(previousK);
    const previousY = previousX - previousK;
    while (x > previousX && y > previousY) {
      x--;
      y--;
    }
    if (down) steps.push({ kind: "insert", after: previousY });
    else steps.push({ kind: "delete", after: previousY });
    x = previousX;
    y = previousY;
  }
  return steps.reverse();
}

/**
 * Reads layers as text, keeping each one's last text per stage, so that a
 * read after a change says which lines it touched ({@link LayerTextRead.changes}).
 * A read whose text is unchanged keeps the changes of the read before it,
 * so an edit elsewhere does not clear what the last edit here showed.
 */
export class LayerTexts {
  readonly #reads = new WeakMap<Stage, Map<bigint, LayerTextRead>>();
  readonly #decoder = new TextDecoder();

  /** The history's undo steps at each layer's last read. */
  readonly #history = new WeakMap<LayerTextRead, readonly HistoryStep[]>();

  /**
   * `layer` of `stage` as USDA text (`Stage.exportLayer`), with what
   * changed since it was last read, and across how many steps of
   * `history` (the undo steps as they stand, oldest first). Throws what
   * the export throws.
   */
  read(stage: Stage, layer: Layer, history: readonly HistoryStep[] = []): LayerTextRead {
    const start = performance.now();
    const bytes = stage.exportLayer(layer, "usda");
    const truncated = bytes.byteLength > MAX_TEXT_BYTES;
    let text = this.#decoder.decode(truncated ? bytes.subarray(0, MAX_TEXT_BYTES) : bytes);
    // Cut at the last whole line.
    if (truncated) text = text.slice(0, Math.max(0, text.lastIndexOf("\n")));
    const lines = text.endsWith("\n") ? text.slice(0, -1).split("\n") : text.split("\n");
    let byLayer = this.#reads.get(stage);
    if (byLayer === undefined) {
      byLayer = new Map();
      this.#reads.set(stage, byLayer);
    }
    const last = byLayer.get(layer.id);
    const same = last !== undefined && last.lines.length === lines.length && last.lines.every((line, index) => line === lines[index]);
    const changes = last === undefined ? NO_CHANGES : same ? last.changes : lineChanges(last.lines, lines);
    // The steps both reads share are passed over: those undone since and those new since are what lies between.
    const then = last === undefined ? history : (this.#history.get(last) ?? []);
    let shared = 0;
    while (shared < then.length && shared < history.length && then[shared] === history[shared]) shared++;
    const between = [...then.slice(shared), ...history.slice(shared)].filter((step) => step.layers.some((written) => written.id === layer.id));
    const steps = last === undefined ? 0 : same ? last.steps : between.length;
    const read: LayerTextRead = { layer, lines: same ? last.lines : lines, bytes: bytes.byteLength, truncated, changes, steps, ms: performance.now() - start };
    byLayer.set(layer.id, read);
    this.#history.set(read, history);
    return read;
  }

  /** The last read of `layer` of `stage`, without reading it again; `null` when it was never read. */
  last(stage: Stage, layer: Layer): LayerTextRead | null {
    return this.#reads.get(stage)?.get(layer.id) ?? null;
  }
}
