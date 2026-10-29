// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's model, with no DOM: a stage, the host's fetch, the undo
// stack the app keeps, and logs of what crossed the boundary. The page
// (`main.ts`) and the headless acceptance script drive the same object.
import type {
  Changes,
  Edit,
  EditReceipt,
  Layer,
  Layerstack,
  ResolveResult,
  SaveResult,
  Stage,
  ValueInput,
} from "@forest-rs/layerstack";

/**
 * Where the editor gets an asset's bytes: HTTP in a browser, the disk in
 * Node. `null` means the asset does not exist; a throw means the fetch
 * failed.
 */
export type Fetcher = (identifier: string) => Promise<Uint8Array | null>;

/** A layer's bytes under its identifier, as `Stage.save` writes them. */
export interface LayerFile {
  readonly identifier: string;
  readonly bytes: Uint8Array;
}

/** One step of the asset loop: what the stage wanted, what the host fetched and supplied, what the stage read. */
export interface LoopEntry {
  /** The action that ran the loop (`open`, `finish=chrome`). */
  readonly cause: string;
  readonly phase: "want" | "fetch" | "supply" | "load" | "fail";
  readonly identifier: string;
  /** The rest, in words. */
  readonly detail: string;
}

/** One recomposition the stage reported, and the action that caused it. */
export interface ChangeEntry {
  readonly cause: string;
  readonly changes: Changes;
}

/** An action on the undo or redo stack: its label and the receipt that reverts it. */
export interface UndoEntry {
  readonly label: string;
  readonly receipt: EditReceipt;
}

/** Options of {@link Editor.open}. */
export interface EditorOptions {
  /** Bytes the host already has (layers it saved), read before fetching. */
  readonly local?: ReadonlyMap<string, Uint8Array>;
  /** Called after each step of the asset loop, so a page can show it while fetches are pending. */
  readonly progress?: (() => void) | undefined;
}

/** The most rounds of the explicit asset loop before the editor gives up. */
const MAX_ROUNDS = 16;

/**
 * A stage being edited. Edits go to `stage.editLayer` (the root layer of
 * the sample); the editor keeps the receipts for undo and redo, because the
 * stage keeps no history.
 */
export class Editor implements Disposable {
  /** The instance the stage runs in. */
  readonly layerstack: Layerstack;
  /** The stage. */
  readonly stage: Stage;
  /** The root layer's identifier. */
  readonly root: string;
  /** Undoable actions, oldest first. */
  readonly undoStack: UndoEntry[] = [];
  /** Undone actions, the most recently undone last. */
  readonly redoStack: UndoEntry[] = [];
  /** Every recomposition, oldest first. */
  readonly changeLog: ChangeEntry[] = [];
  /** Every step of the asset loop, oldest first. */
  readonly loopLog: LoopEntry[] = [];
  /** The layers last saved: what a reopen reads before fetching. */
  saved: readonly LayerFile[] = [];
  readonly #fetch: Fetcher;
  readonly #local: ReadonlyMap<string, Uint8Array>;
  readonly #progress: (() => void) | undefined;
  #cause = "open";
  #fetched = 0;
  #fetchedBytes = 0;

  private constructor(
    layerstack: Layerstack,
    root: string,
    bytes: Uint8Array,
    fetch: Fetcher,
    options: EditorOptions,
  ) {
    this.layerstack = layerstack;
    this.root = root;
    this.#fetch = fetch;
    this.#local = options.local ?? new Map();
    this.#progress = options.progress;
    this.stage = layerstack.open({ identifier: root, bytes });
    this.stage.onChange((changes) => this.changeLog.push({ cause: this.#cause, changes }));
  }

  /**
   * Opens `root` and resolves everything it depends on: the stage composes
   * the root alone, then `Stage.resolve` fetches what it wants, round by
   * round.
   */
  static async open(layerstack: Layerstack, root: string, fetch: Fetcher, options: EditorOptions = {}): Promise<Editor> {
    const local = options.local?.get(root);
    const bytes = local ?? (await fetch(root));
    if (bytes === null) throw new Error(`the root layer ${root} does not exist`);
    const editor = new Editor(layerstack, root, bytes, fetch, options);
    if (local === undefined) {
      editor.#fetched += 1;
      editor.#fetchedBytes += bytes.byteLength;
    }
    editor.#log("supply", root, `${bytes.byteLength} B ${local === undefined ? "fetched" : "from the saved layers"} (the root)`);
    try {
      await editor.resolve("open");
    } catch (error) {
      editor.free();
      throw error;
    }
    return editor;
  }

  /** The assets fetched so far, and their bytes. */
  get fetched(): { readonly assets: number; readonly bytes: number } {
    return { assets: this.#fetched, bytes: this.#fetchedBytes };
  }

  /**
   * Fetches what the stage wants with `Stage.resolve`, logging each fetch:
   * the one-call form of the loop.
   */
  async resolve(cause: string): Promise<ResolveResult> {
    this.#cause = cause;
    for (const asset of this.stage.unavailable()) {
      this.#log("want", asset.identifier, `${asset.usedBy} of ${asset.prims.join(", ")} (authored @${asset.assetPath}@ in ${asset.anchor.identifier})`);
    }
    const result = await this.stage.resolve({ fetch: (identifier) => this.#source(identifier) });
    for (const layer of result.loaded) this.#log("load", layer.identifier ?? "(anonymous)", "read");
    for (const failure of result.failures) this.#log("fail", failure.assetPath, `${failure.code}: ${failure.message}`);
    return result;
  }

  /**
   * Fetches what the stage wants step by step, the loop `Stage.resolve`
   * runs, so each phase is visible: `unavailable()`, fetch, `supply()`,
   * `loadSupplied()`, until nothing is unavailable. Returns the rounds run.
   */
  async supplyUnavailable(cause: string): Promise<number> {
    this.#cause = cause;
    for (let round = 0; round < MAX_ROUNDS; round += 1) {
      const wanted = this.stage.unavailable();
      if (wanted.length === 0) return round;
      for (const asset of wanted) {
        this.#log("want", asset.identifier, `${asset.usedBy} of ${asset.prims.join(", ")} (authored @${asset.assetPath}@ in ${asset.anchor.identifier})`);
      }
      const identifiers = [...new Set(wanted.map((asset) => asset.identifier))];
      const arrived = await Promise.all(identifiers.map((identifier) => this.#source(identifier)));
      identifiers.forEach((identifier, index) => {
        const bytes = arrived[index] ?? null;
        this.stage.supply(identifier, bytes);
        this.#log("supply", identifier, bytes === null ? "confirmed absent" : `${bytes.byteLength} B`);
      });
      const result = this.stage.loadSupplied();
      for (const layer of result.loaded) this.#log("load", layer.identifier ?? "(anonymous)", "read");
      for (const failure of result.failures) this.#log("fail", failure.assetPath, `${failure.code}: ${failure.message}`);
    }
    throw new Error(`assets still unavailable after ${MAX_ROUNDS} rounds`);
  }

  /**
   * Applies `edits` to the edit layer as one undoable action. The redo
   * stack is dropped: a new action forks history.
   */
  apply(label: string, edits: readonly Edit[]): EditReceipt {
    return this.#record(label, () => this.stage.apply(edits));
  }

  /**
   * Authors `value` for the attribute at `path` in the edit layer: its
   * default, or its sample at `time`.
   */
  setValue(path: string, value: ValueInput, time: number | null = null): EditReceipt {
    return time === null
      ? this.apply(`set ${path}`, [{ type: "setValue", path, value }])
      : this.apply(`set ${path} at ${time}`, [{ type: "setValue", path, value, time }]);
  }

  /**
   * Moves (renames or reparents) the prim or property at `path` to `to`,
   * as one undoable action: `Stage.move` moves every opinion of the edit
   * layer's stack and repairs the paths that name it.
   */
  move(path: string, to: string): EditReceipt {
    return this.#record(`move ${path} to ${to}`, () => this.stage.move(path, to));
  }

  /** Deletes the prim or property at `path` from the edit layer's stack, as one undoable action. */
  delete(path: string): EditReceipt {
    return this.#record(`delete ${path}`, () => this.stage.delete(path));
  }

  /**
   * Selects `variant` of `set` on `prim`, then supplies what the variant
   * needs that the stage has not read, step by step.
   */
  async selectVariant(prim: string, set: string, variant: string): Promise<EditReceipt> {
    const receipt = this.apply(`select ${set}=${variant} on ${prim}`, [
      { type: "setVariantSelection", path: prim, set, variant },
    ]);
    await this.supplyUnavailable(`${set}=${variant}`);
    return receipt;
  }

  /** Undoes the newest action; what it brings back may want assets, which are supplied. */
  async undo(): Promise<void> {
    const entry = this.undoStack.pop();
    if (entry === undefined) return;
    this.#cause = `undo ${entry.label}`;
    try {
      this.redoStack.push({ label: entry.label, receipt: entry.receipt.undo() });
    } catch (error) {
      this.undoStack.push(entry);
      throw error;
    }
    await this.supplyUnavailable(this.#cause);
  }

  /** Redoes the most recently undone action. */
  async redo(): Promise<void> {
    const entry = this.redoStack.pop();
    if (entry === undefined) return;
    this.#cause = `redo ${entry.label}`;
    try {
      this.undoStack.push({ label: entry.label, receipt: entry.receipt.undo() });
    } catch (error) {
      this.redoStack.push(entry);
      throw error;
    }
    await this.supplyUnavailable(this.#cause);
  }

  /** The layers with unsaved changes. */
  dirtyLayers(): Layer[] {
    return this.stage.layers().filter((layer) => this.stage.isDirty(layer));
  }

  /** Saves the dirty layers: their bytes are the host's to persist ({@link Editor.saved}). */
  save(): SaveResult {
    this.#cause = "save";
    const result = this.stage.save();
    this.saved = result.written.map(({ identifier, bytes }) => ({ identifier, bytes }));
    return result;
  }

  /**
   * Opens the root again in a new editor that reads `files` (the layers
   * as saved and downloaded) before fetching the rest.
   */
  reopen(files: readonly LayerFile[]): Promise<Editor> {
    const local = new Map(files.map((file) => [file.identifier, file.bytes]));
    return Editor.open(this.layerstack, this.root, this.#fetch, { local, progress: this.#progress });
  }

  /** Releases the receipts and the stage. */
  free(): void {
    for (const entry of [...this.undoStack, ...this.redoStack]) entry.receipt.release();
    this.undoStack.length = 0;
    this.redoStack.length = 0;
    this.stage.free();
  }

  [Symbol.dispose](): void {
    this.free();
  }

  /** Runs an action that returns a receipt, and keeps the receipt for undo. */
  #record(label: string, action: () => EditReceipt): EditReceipt {
    this.#cause = label;
    const receipt = action();
    this.undoStack.push({ label, receipt });
    for (const entry of this.redoStack.splice(0)) entry.receipt.release();
    return receipt;
  }

  async #source(identifier: string): Promise<Uint8Array | null> {
    const local = this.#local.get(identifier);
    if (local !== undefined) {
      this.#log("fetch", identifier, `${local.byteLength} B from the saved layers`);
      return local;
    }
    const bytes = await this.#fetch(identifier);
    if (bytes === null) {
      this.#log("fetch", identifier, "not found");
    } else {
      this.#fetched += 1;
      this.#fetchedBytes += bytes.byteLength;
      this.#log("fetch", identifier, `${bytes.byteLength} B`);
    }
    return bytes;
  }

  #log(phase: LoopEntry["phase"], identifier: string, detail: string): void {
    this.loopLog.push({ cause: this.#cause, phase, identifier, detail });
    this.#progress?.();
  }
}
