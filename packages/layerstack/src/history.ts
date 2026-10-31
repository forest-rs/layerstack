// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { LayerstackError } from "./errors.js";
import { EditReceipt, Stage, takeReceipt } from "./stage.js";
import type { Layer } from "./stage.js";

/** Options of an {@link UndoHistory}. */
export interface UndoHistoryOptions {
  /**
   * The most steps it keeps, undo and redo together; recording past it
   * drops the oldest undo step ({@link Recorded.dropped}). Omitted or
   * `null`: no bound. `0` keeps none.
   */
  readonly limit?: number | null | undefined;
}

/**
 * One step of an {@link UndoHistory}, as a UI lists it. A step is the
 * same object on the undo and the redo stack, from when it is recorded
 * until it is dropped or forgotten, so an app can key what it shows for
 * a step (a `WeakMap`) on it.
 */
export interface HistoryStep {
  /** The label it was recorded with, or its group's. */
  readonly label: string;
  /**
   * The layers its receipts wrote ({@link EditReceipt.layers}), each
   * once: those undoing it, or redoing it, writes.
   */
  readonly layers: readonly Layer[];
  /** The number of receipts it combines: one for a {@link UndoHistory.record} outside a group. */
  readonly receipts: number;
}

/** What {@link UndoHistory.record} or {@link UndoHistory.endGroup} did. */
export interface Recorded {
  /**
   * `"step"`: a step went onto the undo stack and the redo stack was
   * cleared. `"grouped"`: the receipt joined the open group, which becomes
   * one step when its outermost level ends; the redo stack was cleared.
   * `"nothing"`: the receipt wrote no layer, or the group recorded none,
   * or a nested group ended inside its outer one.
   */
  readonly kind: "step" | "grouped" | "nothing";
  /** The oldest step, which the bound dropped to make room, if any. */
  readonly dropped: HistoryStep | null;
}

/** A point to unwind back to ({@link UndoHistory.unwindTo}), from {@link UndoHistory.mark}. */
export interface HistoryMark {
  /** @internal */
  readonly history: UndoHistory;
  /** @internal */
  readonly epoch: number;
  /** @internal The newest undo step's id when the mark was taken, or -1. */
  readonly top: number;
  /** @internal Steps dropped when the mark was taken. */
  readonly dropped: number;
}

/** A step and the receipts that undo (or redo) it, oldest first. */
interface Entry {
  readonly id: number;
  readonly step: HistoryStep;
  readonly receipts: readonly EditReceipt[];
}

/** The receipts recorded while a group is open. */
interface OpenGroup {
  readonly receipts: EditReceipt[];
  /** Each open level, outermost first: its label and how many receipts were recorded before it. */
  readonly levels: { readonly label: string; readonly start: number }[];
}

/**
 * An undo stack and a redo stack of steps over a stage's
 * {@link EditReceipt}s, with grouping.
 *
 * The history applies nothing on its own: apply with {@link Stage.apply}
 * (or `move`, `delete`, ...) and hand the receipt to
 * {@link UndoHistory.record}, which takes it (the receipt then reads as
 * released) and clears the redo stack. {@link UndoHistory.undo} undoes
 * the newest step and moves it to the redo stack.
 *
 * - **Grouping.** {@link UndoHistory.group} (or
 *   {@link UndoHistory.beginGroup} and {@link UndoHistory.endGroup}, for
 *   a drag that spans events) makes what is recorded inside one step,
 *   which undoes as one atomic transaction ({@link Stage.undo} of several
 *   receipts). Groups nest and flatten into the outermost, whose label
 *   names the step. A group that recorded nothing adds no step, nor does
 *   a receipt that wrote no layer.
 * - **Staleness.** An edit the history did not record (a receipt not
 *   recorded, or a native edit) that changed a slot a step restores makes
 *   the step stale: undoing it throws `history.stale`, changes nothing and
 *   keeps the step. The history never skips it; {@link UndoHistory.reset}
 *   forgets every step.
 * - **Bound.** `limit` keeps at most that many steps, undo and redo
 *   together; recording past it drops the oldest undo step, which
 *   {@link Recorded.dropped} reports.
 * - **Unwinding.** {@link UndoHistory.abandonGroup} undoes what the open
 *   group recorded; {@link UndoHistory.unwindTo} undoes every step since a
 *   {@link UndoHistory.mark}. Both undo as one transaction and forget what
 *   they undid: nothing goes to the redo stack.
 *
 * Errors are {@link LayerstackError}s: `history.empty` (nothing to undo
 * or redo), `history.group_open` (undo, redo or unwind while a group is
 * open), `history.no_group`, `history.mark_gone` (the mark's step was
 * undone, dropped or reset), `history.stale`, and `history.rejected` (the
 * undo was refused otherwise; the native error is the `cause`), besides
 * `argument.invalid`, `receipt.affinity` and `receipt.released` for a
 * receipt that cannot be recorded. A failed operation leaves the stage and
 * the history as they were.
 *
 * Owns the receipts it holds: {@link UndoHistory.release} it (or declare
 * it with `using`) when done.
 *
 * ```ts
 * const history = new UndoHistory(stage, { limit: 100 });
 * history.group("define and set radius", () => {
 *   history.record("define", stage.apply([{ type: "definePrim", path: "/World/Ball", typeName: "Sphere" }]));
 *   history.record("set radius", stage.apply([{ type: "setValue", path: "/World/Ball.radius", value: { type: "double", value: 2 } }]));
 * });
 * history.undoSteps.map((step) => step.label); // ["define and set radius"]
 * history.undo(); // both edits, as one transaction
 * history.redo();
 * ```
 */
export class UndoHistory implements Disposable {
  /** The stage whose receipts it records. */
  readonly stage: Stage;
  /** The most steps it keeps, `null` for no bound. */
  readonly limit: number | null;
  readonly #undo: Entry[] = [];
  readonly #redo: Entry[] = [];
  #group: OpenGroup | null = null;
  #nextId = 0;
  #dropped = 0;
  #epoch = 0;

  constructor(stage: Stage, options: UndoHistoryOptions = {}) {
    if (!(stage instanceof Stage)) {
      throw new LayerstackError("argument.invalid", "UndoHistory", "expected a Stage");
    }
    const limit = options.limit ?? null;
    if (limit !== null && !(Number.isInteger(limit) && limit >= 0)) {
      throw new LayerstackError("argument.invalid", "UndoHistory", "limit must be a non-negative integer or null");
    }
    this.stage = stage;
    this.limit = limit;
  }

  /**
   * Records `receipt` as a step labelled `label` and clears the redo
   * stack; inside a group the receipt joins it, and the group's label
   * names the step. The history takes the receipt: it reads as released
   * afterwards, and undoes through the history. A receipt that wrote no
   * layer records nothing (it is released).
   *
   * Do not record the receipts {@link UndoHistory.undo} and
   * {@link UndoHistory.redo} return: the history holds them already.
   *
   * Throws `argument.invalid` for a label that is not a string or a
   * value that is not a receipt, `receipt.affinity` for another stage's
   * receipt and `receipt.released` for a spent one.
   */
  record(label: string, receipt: EditReceipt): Recorded {
    if (typeof label !== "string") {
      throw new LayerstackError("argument.invalid", "record", "expected a label string");
    }
    if (receipt instanceof EditReceipt && receipt.stage !== this.stage) {
      throw new LayerstackError("receipt.affinity", "record", "the receipt belongs to another stage");
    }
    const taken = takeReceipt(receipt, "record");
    if (taken.layers.length === 0) {
      taken.release();
      return Object.freeze({ kind: "nothing", dropped: null });
    }
    this.#clearRedo();
    if (this.#group !== null) {
      this.#group.receipts.push(taken);
      return Object.freeze({ kind: "grouped", dropped: null });
    }
    return this.#push(label, [taken]);
  }

  /**
   * Runs `fn` as a group labelled `label`: what it records becomes one
   * step ({@link UndoHistory.beginGroup}). Returns what `fn` returns.
   *
   * If `fn` throws, what it recorded is undone and forgotten
   * ({@link UndoHistory.abandonGroup}) and its error is thrown: the group
   * is all or nothing. If that undo is refused (an edit `fn` made outside
   * the history changed what it restores), the group is kept as a step
   * instead, and `fn`'s error is still thrown. `fn` runs synchronously: a
   * promise it returns is not awaited, so edits after its first `await`
   * are not in the group.
   */
  group<T>(label: string, fn: () => T): T {
    if (typeof fn !== "function") {
      throw new LayerstackError("argument.invalid", "group", "expected a function");
    }
    this.beginGroup(label);
    const depth = this.groupDepth;
    let result: T;
    try {
      result = fn();
    } catch (error) {
      try {
        this.#abandon("group", depth);
      } catch {
        while (this.groupDepth >= depth) this.endGroup();
      }
      throw error;
    }
    // Levels `fn` left open close with this one.
    while (this.groupDepth >= depth) this.endGroup();
    return result;
  }

  /**
   * Opens a group labelled `label`: what is recorded until the matching
   * {@link UndoHistory.endGroup} becomes one step. Inside an open group it
   * opens a nested level, which flattens into the outer one.
   */
  beginGroup(label: string): void {
    if (typeof label !== "string") {
      throw new LayerstackError("argument.invalid", "beginGroup", "expected a label string");
    }
    this.#group ??= { receipts: [], levels: [] };
    this.#group.levels.push({ label, start: this.#group.receipts.length });
  }

  /**
   * Closes the innermost open group. Closing the outermost pushes what the
   * group recorded as one step named by its label; a group that recorded
   * nothing adds no step.
   *
   * Throws `history.no_group` when no group is open.
   */
  endGroup(): Recorded {
    const group = this.#group;
    if (group === null) throw new LayerstackError("history.no_group", "endGroup", "no group is open");
    const { label } = group.levels.pop()!;
    if (group.levels.length > 0 || group.receipts.length === 0) {
      if (group.levels.length === 0) this.#group = null;
      return Object.freeze({ kind: "nothing", dropped: null });
    }
    this.#group = null;
    return this.#push(label, group.receipts);
  }

  /**
   * Undoes what the innermost open group recorded, as one transaction, and
   * closes it, adding no step. An enclosing group keeps what it recorded
   * before. Returns the undo's receipt (already released: nothing redoes
   * it), or `null` when the group recorded nothing.
   *
   * Throws `history.no_group` when no group is open, and `history.stale`
   * or `history.rejected` when the undo is refused: nothing changes and
   * the group stays open.
   */
  abandonGroup(): EditReceipt | null {
    return this.#abandon("abandonGroup", this.groupDepth);
  }

  /** The outermost open group's label, or `null` when no group is open. */
  get openGroup(): string | null {
    return this.#group?.levels[0]?.label ?? null;
  }

  /** The number of open group levels: 0 when no group is open. */
  get groupDepth(): number {
    return this.#group?.levels.length ?? 0;
  }

  /**
   * Undoes the newest step and moves it to the redo stack. Returns the
   * undo's receipt, which the history holds for redo: read it, do not
   * record or release it.
   *
   * Throws `history.empty` when there is no step, `history.group_open`
   * while a group is open, `history.stale` when an edit the history did
   * not record changed what the step restores, and `history.rejected`
   * when the undo is refused otherwise. Either way nothing changes and the
   * step stays.
   */
  undo(): EditReceipt {
    return this.#move("undo", this.#undo, this.#redo);
  }

  /** Redoes the most recently undone step, and moves it back to the undo stack. Throws as {@link UndoHistory.undo}. */
  redo(): EditReceipt {
    return this.#move("redo", this.#redo, this.#undo);
  }

  /** Whether {@link UndoHistory.undo} has a step, and no group is open. */
  get canUndo(): boolean {
    return this.#group === null && this.#undo.length > 0;
  }

  /** Whether {@link UndoHistory.redo} has a step, and no group is open. */
  get canRedo(): boolean {
    return this.#group === null && this.#redo.length > 0;
  }

  /** The undo steps, oldest first: the next to undo is last. */
  get undoSteps(): readonly HistoryStep[] {
    return this.#undo.map((entry) => entry.step);
  }

  /** The redo steps, the most recently undone last: the next to redo is last. */
  get redoSteps(): readonly HistoryStep[] {
    return this.#redo.map((entry) => entry.step);
  }

  /** The step {@link UndoHistory.undo} would undo, or `null`. */
  get nextUndo(): HistoryStep | null {
    return this.#undo[this.#undo.length - 1]?.step ?? null;
  }

  /** The step {@link UndoHistory.redo} would redo, or `null`. */
  get nextRedo(): HistoryStep | null {
    return this.#redo[this.#redo.length - 1]?.step ?? null;
  }

  /** The number of steps the bound dropped since the history was made. */
  get dropped(): number {
    return this.#dropped;
  }

  /**
   * Forgets every step, both stacks and any open group, and releases
   * their receipts, without undoing anything: the stage keeps every edit.
   * Marks taken before are gone.
   */
  reset(): void {
    for (const entry of [...this.#undo, ...this.#redo]) for (const receipt of entry.receipts) receipt.release();
    for (const receipt of this.#group?.receipts ?? []) receipt.release();
    this.#undo.length = 0;
    this.#redo.length = 0;
    this.#group = null;
    this.#epoch += 1;
  }

  /** A mark of where the history stands, for {@link UndoHistory.unwindTo}. */
  mark(): HistoryMark {
    return Object.freeze({
      history: this,
      epoch: this.#epoch,
      top: this.#undo[this.#undo.length - 1]?.id ?? -1,
      dropped: this.#dropped,
    });
  }

  /**
   * Undoes every step recorded since `mark`, as one transaction, and
   * forgets them: they do not go to the redo stack, which is left as it
   * is. Returns the undo's receipt (already released), or `null` when no
   * step was recorded since.
   *
   * Throws `argument.invalid` for a mark of another history,
   * `history.group_open` while a group is open, `history.mark_gone` when
   * the mark's step was undone (redo it first), dropped by the bound or
   * reset, and `history.stale` or `history.rejected` when the undo is
   * refused. Either way nothing changes.
   */
  unwindTo(mark: HistoryMark): EditReceipt | null {
    if (typeof mark !== "object" || mark === null || mark.history !== this) {
      throw new LayerstackError("argument.invalid", "unwindTo", "expected a mark of this history");
    }
    this.#checkClosed("unwindTo");
    const gone = () => new LayerstackError("history.mark_gone", "unwindTo", "the mark's step was undone, dropped or reset");
    if (mark.epoch !== this.#epoch) throw gone();
    let keep = 0;
    if (mark.top >= 0) {
      keep = this.#undo.findIndex((entry) => entry.id === mark.top) + 1;
      if (keep === 0) throw gone();
    } else if (mark.dropped !== this.#dropped) {
      throw gone();
    }
    const since = this.#undo.slice(keep);
    if (since.length === 0) return null;
    const undone = this.#undoReceipts(
      "unwindTo",
      since[since.length - 1]!.step.label,
      since.flatMap((entry) => entry.receipts),
    );
    undone.release();
    this.#undo.length = keep;
    return undone;
  }

  /** Releases every receipt it holds, as {@link UndoHistory.reset} does. */
  release(): void {
    this.reset();
  }

  /** Releases it, for `using`. */
  [Symbol.dispose](): void {
    this.release();
  }

  #move(operation: "undo" | "redo", from: Entry[], to: Entry[]): EditReceipt {
    this.#checkClosed(operation);
    const entry = from[from.length - 1];
    if (entry === undefined) {
      throw new LayerstackError("history.empty", operation, `there is nothing to ${operation}`);
    }
    const receipt = this.#undoReceipts(operation, entry.step.label, entry.receipts);
    from.pop();
    // Undoing writes the layers the edits wrote: the step is the same.
    to.push({ id: entry.id, step: entry.step, receipts: [receipt] });
    return receipt;
  }

  /** Undoes `receipts` as one transaction, translating a refusal into a history error. */
  #undoReceipts(operation: string, label: string, receipts: readonly EditReceipt[]): EditReceipt {
    try {
      return this.stage.undo(receipts);
    } catch (error) {
      if (!(error instanceof LayerstackError) || !error.code.startsWith("edit.")) throw error;
      const stale = error.code === "edit.stale" || error.code === "edit.rejected.diverged";
      const message = error.message.slice(error.operation.length + 2);
      throw new LayerstackError(
        stale ? "history.stale" : "history.rejected",
        operation,
        `${JSON.stringify(label)} ${stale ? "is stale" : "was refused"}: ${message}`,
        { cause: error },
      );
    }
  }

  /** Undoes what the group levels from `depth` (1 the outermost) recorded, as one transaction, and closes them. */
  #abandon(operation: string, depth: number): EditReceipt | null {
    const group = this.#group;
    if (group === null) throw new LayerstackError("history.no_group", operation, "no group is open");
    const level = group.levels[depth - 1]!;
    const recorded = group.receipts.slice(level.start);
    let undone: EditReceipt | null = null;
    if (recorded.length > 0) {
      undone = this.#undoReceipts(operation, group.levels[group.levels.length - 1]!.label, recorded);
      undone.release();
      group.receipts.length = level.start;
    }
    group.levels.length = depth - 1;
    if (group.levels.length === 0) this.#group = null;
    return undone;
  }

  #checkClosed(operation: string): void {
    const open = this.openGroup;
    if (open !== null) {
      throw new LayerstackError("history.group_open", operation, `the group ${JSON.stringify(open)} is open`);
    }
  }

  #clearRedo(): void {
    for (const entry of this.#redo.splice(0)) for (const receipt of entry.receipts) receipt.release();
  }

  #push(label: string, receipts: readonly EditReceipt[]): Recorded {
    const layers = [...new Set(receipts.flatMap((receipt) => receipt.layers))];
    const step: HistoryStep = Object.freeze({ label, layers: Object.freeze(layers), receipts: receipts.length });
    this.#undo.push({ id: this.#nextId++, step, receipts });
    let dropped: HistoryStep | null = null;
    if (this.limit !== null && this.#undo.length + this.#redo.length > this.limit) {
      const oldest = this.#undo.shift()!;
      for (const receipt of oldest.receipts) receipt.release();
      dropped = oldest.step;
      this.#dropped += 1;
    }
    return Object.freeze({ kind: "step", dropped });
  }
}
