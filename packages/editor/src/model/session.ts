// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's model, with no DOM: a stage, the host's fetch, the
// stage's undo history, where edits go, and logs of what crossed the
// boundary. The React app (`../app`) drives it through `../store.ts`.
import { UndoHistory } from "@forest-rs/layerstack";
import type {
  Changes,
  Edit,
  EditReceipt,
  EditTarget,
  HistoryStep,
  Layer,
  Layerstack,
  LoadResult,
  OpenOptions,
  SaveResult,
  Stage,
  ValueInput,
} from "@forest-rs/layerstack";
import { LoopRecorder } from "./timeline.ts";

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

/** An action on the undo or redo stack. */
export interface UndoEntry {
  readonly label: string;
  /** The step of the undo history: its label and the layers it writes. */
  readonly step: HistoryStep;
  /**
   * The receipt of the action that made the step, for its report: what it
   * asks for, the paths a move leaves. The history holds what undoes it.
   */
  readonly receipt: EditReceipt;
}

/** Options of {@link Session.open}. */
export interface SessionOptions {
  /** How the stage opens (`Layerstack.open`'s options). */
  readonly open?: OpenOptions;
  /** Bytes the host already has (layers it saved), read before fetching. */
  readonly local?: ReadonlyMap<string, Uint8Array>;
  /** Called with the session after each step of the asset loop, so a page can show it while fetches are pending. */
  readonly progress?: ((session: Session) => void) | undefined;
  /**
   * Called as the open enters each phase, and awaited, so a page can show
   * the phase before work that holds the thread (reading the root layer,
   * reading a round's supplied layers, composing them) begins.
   */
  readonly phase?: ((phase: OpenPhase) => void | Promise<void>) | undefined;
}

/** What an open, or a later round of the asset loop, is doing. */
export type OpenPhase =
  /** Fetching the root layer. */
  | { readonly kind: "fetchingRoot"; readonly identifier: string }
  /** Reading the root layer's bytes, before any asset is asked for. */
  | { readonly kind: "readingRoot"; readonly identifier: string; readonly bytes: number }
  /** Fetching what round `round` of the asset loop asks for. */
  | { readonly kind: "fetching"; readonly round: number; readonly assets: number }
  /** Reading the round's supplied layers, without composing (`readSupplied`). */
  | { readonly kind: "reading"; readonly round: number; readonly layers: number; readonly bytes: number }
  /** Composing, once, the `layers` the rounds read (`loadSupplied`). */
  | { readonly kind: "composing"; readonly layers: number };

/**
 * The most rounds of the asset loop before the session gives up, as
 * `Stage.resolve`'s default: each round reads what the last one's layers
 * name, so a deeply nested asset needs one per level.
 */
const MAX_ROUNDS = 100;

/**
 * A stage being edited. Edits go to {@link Session.target} (the stage's
 * edit layer until another is chosen); each action's receipt goes to the
 * session's `UndoHistory` as one step.
 */
export class Session implements Disposable {
  /** The instance the stage runs in. */
  readonly layerstack: Layerstack;
  /** The stage. */
  readonly stage: Stage;
  /** The root layer's identifier. */
  readonly root: string;
  /** The undo and redo steps of the actions. */
  readonly history: UndoHistory;
  /** Every recomposition, oldest first. */
  readonly changeLog: ChangeEntry[] = [];
  /** Every step of the asset loop, oldest first. */
  readonly loopLog: LoopEntry[] = [];
  /** When each step of the asset loop happened, run by run, for the waterfall. */
  readonly loops: LoopRecorder;
  /** The layers last saved: what a reopen reads before fetching. */
  saved: readonly LayerFile[] = [];
  /**
   * The identifiers of the layers the stage mutes, kept from the changes
   * it reports (`Changes.mutedIdentifiers` and `unmutedIdentifiers`), not
   * read back from the stage.
   */
  readonly muted = new Set<string>();
  /**
   * Where actions write: a layer of the stage's layer stack, or an edit
   * target through an arc (an opinion's, a variant's); `undefined` for
   * the stage's edit layer.
   */
  target: Layer | EditTarget | undefined = undefined;
  readonly #fetch: Fetcher;
  readonly #local: ReadonlyMap<string, Uint8Array>;
  readonly #progress: ((session: Session) => void) | undefined;
  readonly #phase: SessionOptions["phase"];
  readonly #open: OpenOptions;
  /** The receipt that made each step, for its report. */
  readonly #reports = new WeakMap<HistoryStep, EditReceipt>();
  #cause = "open";
  #fetched = 0;
  #fetchedBytes = 0;

  private constructor(
    layerstack: Layerstack,
    root: string,
    bytes: Uint8Array,
    fetch: Fetcher,
    options: SessionOptions,
    loops: LoopRecorder,
  ) {
    this.layerstack = layerstack;
    this.loops = loops;
    this.root = root;
    this.#fetch = fetch;
    this.#local = options.local ?? new Map();
    this.#progress = options.progress;
    this.#phase = options.phase;
    this.#open = options.open ?? {};
    this.stage = layerstack.open({ identifier: root, bytes }, options.open ?? {});
    this.history = new UndoHistory(this.stage);
    this.stage.onChange((changes) => {
      this.changeLog.push({ cause: this.#cause, changes });
      for (const identifier of changes.mutedIdentifiers) this.muted.add(identifier);
      for (const identifier of changes.unmutedIdentifiers) this.muted.delete(identifier);
    });
  }

  /**
   * Opens `root` and resolves everything it depends on: the stage composes
   * the root alone, then {@link Session.supplyUnavailable} fetches what it
   * wants, round by round, timing each step ({@link Session.loops}).
   */
  static async open(layerstack: Layerstack, root: string, fetch: Fetcher, options: SessionOptions = {}): Promise<Session> {
    const loops = new LoopRecorder();
    const start = performance.now();
    loops.begin("open", start);
    const local = options.local?.get(root);
    if (local === undefined) await options.phase?.({ kind: "fetchingRoot", identifier: root });
    const bytes = local ?? (await fetch(root));
    if (bytes === null) throw new Error(`the root layer ${root} does not exist`);
    await options.phase?.({ kind: "readingRoot", identifier: root, bytes: bytes.byteLength });
    const readStart = performance.now();
    loops.root(root, start, readStart, bytes.byteLength, local === undefined ? "fetched" : "saved");
    const session = new Session(layerstack, root, bytes, fetch, options, loops);
    loops.rootRead(readStart, performance.now());
    if (local === undefined) {
      session.#fetched += 1;
      session.#fetchedBytes += bytes.byteLength;
    }
    session.#log("supply", root, `${bytes.byteLength} B ${local === undefined ? "fetched" : "from the saved layers"} (the root)`);
    try {
      await session.supplyUnavailable("open");
    } catch (error) {
      session.free();
      throw error;
    }
    return session;
  }

  /** The assets fetched so far, and their bytes. */
  get fetched(): { readonly assets: number; readonly bytes: number } {
    return { assets: this.#fetched, bytes: this.#fetchedBytes };
  }

  /**
   * Fetches what the stage wants step by step, the loop `Stage.resolve`
   * runs: `unavailable()`, fetch every identifier at once, `supply()` what
   * arrived, `readSupplied()`, until nothing is unavailable; then
   * `loadSupplied()` composes what the rounds read, once, and the loop
   * goes on if that composition wants more. Returns the rounds run.
   *
   * The session runs the loop itself rather than calling `Stage.resolve`
   * so that each phase is logged and timed ({@link Session.loops}):
   * `resolve` hands its fetch only an identifier, so what later rounds
   * want (their uses and prims), when they are supplied and how long the
   * stage reads are hidden in it. As `resolve` does, a fetch that throws
   * fails the loop after what the round's other fetches returned is read
   * and composed, and an asset supplied but wanted again fails it.
   */
  async supplyUnavailable(cause: string): Promise<number> {
    this.#cause = cause;
    const run = this.loops.current ?? this.loops.begin(cause, performance.now());
    try {
      const rounds = await this.#rounds();
      if (run.assets.length === 0) this.loops.discard();
      else this.loops.end(performance.now());
      return rounds;
    } catch (error) {
      this.loops.end(performance.now(), error instanceof Error ? error.message : String(error));
      throw error;
    }
  }

  /** Undoable actions, oldest first. */
  get undoStack(): UndoEntry[] {
    return this.history.undoSteps.map((step) => this.#entry(step));
  }

  /** Undone actions, the most recently undone last. */
  get redoStack(): UndoEntry[] {
    return this.history.redoSteps.map((step) => this.#entry(step));
  }

  /**
   * Applies `edits` to the edit layer as one undoable action. The redo
   * stack is dropped: a new action forks history.
   */
  apply(label: string, edits: readonly Edit[]): EditReceipt {
    return this.#record(label, () => this.stage.apply(edits, this.target));
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
   * Authors `value` for the attribute at `path` through `target`, such as
   * the edit target of an opinion in a referenced asset: the path and time
   * are the stage's, and the target writes them where it maps them (the
   * asset's `/Robot.height` for `/World/Robot.height`).
   */
  setValueThrough(target: EditTarget, path: string, value: ValueInput, time: number | null = null): EditReceipt {
    const where = `${target.layer.identifier ?? "(anonymous)"}${target.site === null ? "" : ` at ${target.site}`}`;
    const edit: Edit = time === null ? { type: "setValue", path, value } : { type: "setValue", path, value, time };
    return this.#record(`set ${path}${time === null ? "" : ` at ${time}`} in ${where}`, () => this.stage.apply([edit], target));
  }

  /**
   * Moves (renames or reparents) the prim or property at `path` to `to`,
   * as one undoable action: `Stage.move` moves every opinion of the edit
   * layer's stack and repairs the paths that name it.
   */
  move(path: string, to: string): EditReceipt {
    return this.#record(`move ${path} to ${to}`, () => this.stage.move(path, to, this.target));
  }

  /** Deletes the prim or property at `path` from the edit layer's stack, as one undoable action. */
  delete(path: string): EditReceipt {
    return this.#record(`delete ${path}`, () => this.stage.delete(path, this.target));
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

  /**
   * Undoes the newest action; what it brings back may want assets, which
   * are supplied. A step an edit outside the history made stale throws
   * `history.stale` and stays.
   */
  async undo(): Promise<void> {
    const step = this.history.nextUndo;
    if (step === null) return;
    this.#cause = `undo ${step.label}`;
    this.history.undo();
    await this.supplyUnavailable(this.#cause);
  }

  /** Redoes the most recently undone action. */
  async redo(): Promise<void> {
    const step = this.history.nextRedo;
    if (step === null) return;
    this.#cause = `redo ${step.label}`;
    this.history.redo();
    await this.supplyUnavailable(this.#cause);
  }

  /**
   * Mutes or unmutes `layer`: the stage recomposes without or with its
   * opinions, heard as one change. Muting is the stage's state, not an
   * edit, so it is not on the undo stack. What an unmuted layer asks for
   * that the stage has not read is supplied. Returns whether the muting
   * changed.
   */
  async setMuted(layer: Layer, muted: boolean): Promise<boolean> {
    const name = layer.identifier ?? "(anonymous)";
    this.#cause = `${muted ? "mute" : "unmute"} ${name}`;
    const changed = muted ? this.stage.muteLayer(layer) : this.stage.unmuteLayer(layer);
    if (!muted) await this.supplyUnavailable(this.#cause);
    return changed;
  }

  /** The layer the stage holds with `identifier`; throws when it holds none. */
  layerNamed(identifier: string): Layer {
    const layer = this.stage.layers().find((candidate) => candidate.identifier === identifier);
    if (layer === undefined) throw new Error(`the stage holds no layer ${identifier}`);
    return layer;
  }

  /** The layers with unsaved changes. */
  dirtyLayers(): Layer[] {
    return this.stage.layers().filter((layer) => this.stage.isDirty(layer));
  }

  /** Saves the dirty layers: their bytes are the host's to persist ({@link Session.saved}). */
  save(): SaveResult {
    this.#cause = "save";
    const result = this.stage.save();
    this.saved = result.written.map(({ identifier, bytes }) => ({ identifier, bytes }));
    return result;
  }

  /**
   * Opens the root again in a new session that reads `files` (the layers
   * as saved and downloaded) before fetching the rest.
   */
  reopen(files: readonly LayerFile[]): Promise<Session> {
    const local = new Map(files.map((file) => [file.identifier, file.bytes]));
    return Session.open(this.layerstack, this.root, this.#fetch, { open: this.#open, local, progress: this.#progress, phase: this.#phase });
  }

  /** Releases the history's receipts and the stage. */
  free(): void {
    this.history.release();
    this.stage.free();
  }

  [Symbol.dispose](): void {
    this.free();
  }

  /** Runs an action that returns a receipt, and records it in the history as a step. */
  #record(label: string, action: () => EditReceipt): EditReceipt {
    this.#cause = label;
    const receipt = action();
    if (this.history.record(label, receipt).kind === "step") this.#reports.set(this.history.nextUndo!, receipt);
    return receipt;
  }

  #entry(step: HistoryStep): UndoEntry {
    const receipt = this.#reports.get(step);
    if (receipt === undefined) throw new Error(`no receipt for the step ${step.label}`);
    return { label: step.label, step, receipt };
  }

  /** The rounds of {@link Session.supplyUnavailable}, recorded into the current run. */
  async #rounds(): Promise<number> {
    const supplied = new Set<string>();
    // The layers read since the stage last composed; `null` when none was.
    let read: number | null = null;
    const compose = async () => {
      const layers = read ?? 0;
      read = null;
      await this.#phase?.({ kind: "composing", layers });
      const start = performance.now();
      const result = this.stage.loadSupplied();
      this.loops.composed(result, start, performance.now());
      this.#logLoad(result);
    };
    for (let round = 0; round < MAX_ROUNDS; ) {
      const wantStart = performance.now();
      const wanted = this.stage.unavailable();
      const wantEnd = performance.now();
      if (wanted.length === 0 && read !== null) {
        await compose();
        continue;
      }
      this.loops.want(wanted, wantStart, wantEnd);
      if (wanted.length === 0) return round;
      round += 1;
      for (const asset of wanted) {
        const by = asset.prims.length > 0 ? ` of ${asset.prims.join(", ")}` : "";
        this.#log("want", asset.identifier, `${asset.usedBy}${by} (authored @${asset.assetPath}@ in ${asset.anchor.identifier})`);
      }
      const identifiers = [...new Set(wanted.map((asset) => asset.identifier))];
      const stuck = identifiers.find((identifier) => supplied.has(identifier));
      if (stuck !== undefined) throw new Error(`${stuck} was supplied but the stage still wants it`);
      await this.#phase?.({ kind: "fetching", round, assets: identifiers.length });
      const settled = await Promise.allSettled(identifiers.map((identifier) => this.#source(identifier)));
      this.loops.fetched(performance.now());
      let failed: Error | null = null;
      settled.forEach((outcome, index) => {
        const identifier = identifiers[index] as string;
        if (outcome.status === "rejected") {
          failed ??= new Error(`fetching ${identifier} failed`, { cause: outcome.reason });
          return;
        }
        const bytes = outcome.value;
        this.stage.supply(identifier, bytes);
        supplied.add(identifier);
        this.loops.supplied(identifier, performance.now());
        this.#log("supply", identifier, bytes === null ? "confirmed absent" : `${bytes.byteLength} B`);
      });
      this.loops.suppliesDone(performance.now());
      const suppliedBytes = settled.reduce((sum, outcome) => sum + (outcome.status === "fulfilled" ? (outcome.value?.byteLength ?? 0) : 0), 0);
      await this.#phase?.({ kind: "reading", round, layers: identifiers.length, bytes: suppliedBytes });
      const readStart = performance.now();
      const result = this.stage.readSupplied();
      this.loops.read(result, readStart, performance.now());
      read = (read ?? 0) + result.loaded.length;
      this.#logLoad(result);
      if (failed !== null) {
        // The stage composes what the rounds read before the loop fails.
        await compose();
        throw failed;
      }
    }
    throw new Error(`assets still unavailable after ${MAX_ROUNDS} rounds`);
  }

  /** Logs the layers a read loaded and the assets that failed. */
  #logLoad(result: LoadResult): void {
    for (const layer of result.loaded) this.#log("load", layer.identifier ?? "(anonymous)", "read");
    for (const failure of result.failures) this.#log("fail", failure.assetPath, `${failure.code}: ${failure.message}`);
  }

  /** The host's bytes for `identifier`: the saved layers first, then the fetch; logged and timed. */
  async #source(identifier: string): Promise<Uint8Array | null> {
    this.loops.fetchStarted(identifier, performance.now());
    const local = this.#local.get(identifier);
    if (local !== undefined) {
      this.loops.fetchSettled(identifier, performance.now(), local.byteLength, "saved");
      this.#log("fetch", identifier, `${local.byteLength} B from the saved layers`);
      return local;
    }
    let bytes: Uint8Array | null;
    try {
      bytes = await this.#fetch(identifier);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.loops.fetchSettled(identifier, performance.now(), null, "error", message);
      this.#log("fail", identifier, `fetch failed: ${message}`);
      throw error;
    }
    if (bytes === null) {
      this.loops.fetchSettled(identifier, performance.now(), null, "absent");
      this.#log("fetch", identifier, "not found");
    } else {
      this.#fetched += 1;
      this.#fetchedBytes += bytes.byteLength;
      this.loops.fetchSettled(identifier, performance.now(), bytes.byteLength, "fetched");
      this.#log("fetch", identifier, `${bytes.byteLength} B`);
    }
    return bytes;
  }

  #log(phase: LoopEntry["phase"], identifier: string, detail: string): void {
    this.loopLog.push({ cause: this.#cause, phase, identifier, detail });
    this.#progress?.(this);
  }
}
