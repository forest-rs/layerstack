// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's model, with no DOM: a stage, the host's fetch, the
// stage's undo history, where edits go, and logs of what crossed the
// boundary. The React app (`../app`) drives it through `../store.ts`.
import { EditTarget, LayerstackError, UndoHistory } from "@forest-rs/layerstack";
import type {
  Changes,
  Edit,
  EditReceipt,
  HistoryMark,
  HistoryStep,
  Layer,
  Layerstack,
  LoadOutcome,
  LoadPolicy,
  LoadResult,
  OpenOptions,
  PackageResult,
  ReleasedLayers,
  ReloadResult,
  SaveResult,
  Stage,
  ValueInput,
} from "@forest-rs/layerstack";
import { addArcEdit, type NewArc, newArcLabel, removeArcEdit, removeArcLabel } from "./composition.ts";
import { packageWants, sessionLayerReason, UNSAVABLE_ROOT } from "./files.ts";
import type { AuthoredArc } from "./layerSpecs.ts";
import { LoopRecorder } from "./timeline.ts";

/**
 * Where the editor gets an asset's bytes: HTTP in a browser, the disk in
 * Node. `null` means the asset does not exist; a throw means the fetch
 * failed. `signal`, when given, aborts once the bytes are no longer
 * wanted (the open was cancelled): a fetcher that can stop should, and
 * one that cannot is not waited for.
 */
export type Fetcher = (identifier: string, signal?: AbortSignal) => Promise<Uint8Array | null>;

/**
 * `work`, or a rejection with `signal`'s reason as soon as it aborts:
 * what is awaited on the host (a fetch, a frame) is not waited for once
 * nobody wants it. The work itself goes on unless it heeds the signal;
 * its result is dropped.
 */
function unlessAborted<T>(work: Promise<T>, signal: AbortSignal | undefined): Promise<T> {
  if (signal === undefined) return work;
  if (signal.aborted) {
    // Its rejection, if any, has no one to hear it.
    work.catch(() => {});
    return Promise.reject(signal.reason);
  }
  return new Promise<T>((resolve, reject) => {
    const abort = () => reject(signal.reason);
    signal.addEventListener("abort", abort, { once: true });
    work.then(
      (value) => {
        signal.removeEventListener("abort", abort);
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener("abort", abort);
        reject(error);
      },
    );
  });
}

/** Where the history stood at a moment (a layer's save): a mark of it, and its undo steps then. */
interface HistoryPoint {
  readonly mark: HistoryMark;
  readonly steps: readonly HistoryStep[];
}

/**
 * The steps between two states of a history, given each one's undo steps:
 * those undone since `then` and those recorded or redone since, past the
 * steps both share. A step undone and redone is the same step, so it is
 * shared again.
 */
function stepsBetween(then: readonly HistoryStep[], now: readonly HistoryStep[]): HistoryStep[] {
  let shared = 0;
  while (shared < then.length && shared < now.length && then[shared] === now[shared]) shared += 1;
  return [...then.slice(shared), ...now.slice(shared)];
}

/** A prim or property that moved: where it was, and where it is. */
export interface PathMove {
  readonly from: string;
  readonly to: string;
}

/**
 * How much an asset loop may read: before each round is fetched, the
 * bytes its assets hold (`size`, unknown ones counting nothing) are added
 * to what the run fetched, and over `bytes` the run stops with
 * {@link OverBudget}, the round not fetched.
 */
export interface LoadBudget {
  readonly bytes: number;
  readonly size: (identifier: string) => Promise<number | null>;
}

/** An asset loop stopped before a round that would take it over its {@link LoadBudget}. */
export class OverBudget extends Error {
  /** The bytes fetched, and the round's sizes. */
  readonly bytes: number;
  constructor(bytes: number, budget: number) {
    super(`reading ${bytes} bytes of layers would pass the ${budget}-byte budget`);
    this.bytes = bytes;
  }
}

/** A layer's bytes under its identifier, as `Stage.save` writes them. */
export interface LayerFile {
  readonly identifier: string;
  readonly bytes: Uint8Array;
}

/** One step of the asset loop: what the stage wanted, what the host fetched and supplied, what the stage read. */
export interface LoopEntry {
  /** The action that ran the loop (`open`, `finish=chrome`). */
  readonly cause: string;
  readonly phase: "want" | "fetch" | "supply" | "load" | "fail" | "release";
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
  /**
   * Aborts the open: nothing more is fetched or read, and the open rejects
   * with the signal's reason, its stage freed.
   */
  readonly signal?: AbortSignal | undefined;
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

/** The most times {@link Session.exportPackage} supplies what the package still wants before it gives up. */
const MAX_PACKAGE_ROUNDS = 8;

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
  /**
   * Why the stage edits in a session layer though the open did not ask
   * for one (`OpenOptions.editLayer`): its root cannot take edits, as a
   * USDZ package cannot; `null` when it was asked for, or is not used.
   */
  readonly sessionNote: string | null;
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
  /** The move each step of a move made, for what follows the moved path when it is undone and redone. */
  readonly #moves = new WeakMap<HistoryStep, PathMove>();
  /** The steps a move or a delete made: what {@link Session.lastNamespaceEdit} looks for. */
  readonly #namespaceSteps = new WeakSet<HistoryStep>();
  /** Where the history stood when the scene opened: the saved point of every layer not saved since. */
  readonly #opened: HistoryPoint;
  /** Where the history stood when each layer (by id) was last saved, for {@link Session.isEdited}. */
  readonly #savedAt = new Map<Layer["id"], HistoryPoint>();
  #cause = "open";
  #fetched = 0;
  #fetchedBytes = 0;
  #releasedLayers = 0;
  #releasedBytes = 0;

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
    // A root that cannot take edits (a package) is edited in a session layer in front of it.
    const asked = options.open ?? {};
    const own = asked.editLayer === "session" || asked.sessionLayer !== undefined;
    let note = own ? null : sessionLayerReason(root, bytes);
    let open: OpenOptions = note === null ? asked : { ...asked, editLayer: "session" };
    let stage = layerstack.open({ identifier: root, bytes }, open);
    if (stage.editLayer === null && open.editLayer !== "session") {
      stage.free();
      note = UNSAVABLE_ROOT;
      open = { ...asked, editLayer: "session" };
      stage = layerstack.open({ identifier: root, bytes }, open);
    }
    this.#open = open;
    this.sessionNote = note;
    this.stage = stage;
    this.history = new UndoHistory(this.stage);
    this.#opened = this.#historyPoint();
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
    const signal = options.signal;
    loops.begin("open", start);
    const local = options.local?.get(root);
    signal?.throwIfAborted();
    if (local === undefined) await options.phase?.({ kind: "fetchingRoot", identifier: root });
    signal?.throwIfAborted();
    const bytes = local ?? (await unlessAborted(fetch(root, signal), signal));
    if (bytes === null) throw new Error(`the root layer ${root} does not exist`);
    await options.phase?.({ kind: "readingRoot", identifier: root, bytes: bytes.byteLength });
    signal?.throwIfAborted();
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
      await session.supplyUnavailable("open", undefined, signal);
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

  /** The layers {@link Session.releaseUnusedLayers} released so far, and the bytes they were read from. */
  get released(): { readonly layers: number; readonly bytes: number } {
    return { layers: this.#releasedLayers, bytes: this.#releasedBytes };
  }

  /**
   * Releases the layers the stage no longer uses (`Stage.releaseUnusedLayers`),
   * keeping the edit target's layer, so the memory of an unloaded payload
   * or a muted layer comes back; the stage composes nothing differently.
   * A layer with unsaved edits is kept. What the stage asks for again
   * later (the payload loaded, the layer unmuted) is fetched again.
   */
  releaseUnusedLayers(): ReleasedLayers {
    const target = this.target instanceof EditTarget ? this.target.layer : this.target;
    const released = this.stage.releaseUnusedLayers(target === undefined ? [] : [target]);
    this.#releasedLayers += released.released.length;
    this.#releasedBytes += released.sourceBytes;
    for (const layer of released.released) {
      this.#log("release", layer.identifier, layer.sourceBytes === null ? "released" : `${layer.sourceBytes} B released`);
    }
    return released;
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
   *
   * When `signal` aborts, the loop stops where it waits (a fetch, a
   * phase's paint) and rejects with the signal's reason: what was
   * supplied is read, the round in flight is not supplied, and the stage
   * still wants it, so the caller either lets the stage go or asks for
   * something else. One loop runs at a time: the caller (the store's
   * action queue) does not start another until this one settles.
   */
  async supplyUnavailable(cause: string, budget?: LoadBudget, signal?: AbortSignal): Promise<number> {
    this.#cause = cause;
    const run = this.loops.current ?? this.loops.begin(cause, performance.now());
    try {
      const rounds = await this.#rounds(budget, signal);
      if (run.assets.length === 0) this.loops.discard();
      else this.loops.end(performance.now());
      return rounds;
    } catch (error) {
      this.loops.end(performance.now(), signal?.aborted === true ? "cancelled" : error instanceof Error ? error.message : String(error));
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
    const receipt = this.#record(`move ${path} to ${to}`, () => this.stage.move(path, to, this.target));
    const step = this.history.nextUndo;
    if (step !== null && this.#reports.get(step) === receipt) {
      this.#moves.set(step, { from: path, to });
      this.#namespaceSteps.add(step);
    }
    return receipt;
  }

  /** Deletes the prim or property at `path` from the edit layer's stack, as one undoable action. */
  delete(path: string): EditReceipt {
    const receipt = this.#record(`delete ${path}`, () => this.stage.delete(path, this.target));
    const step = this.history.nextUndo;
    if (step !== null && this.#reports.get(step) === receipt) this.#namespaceSteps.add(step);
    return receipt;
  }

  /**
   * The newest move or delete on the undo stack, with its receipt: its
   * `unrepaired` lists the paths it left naming what moved. `null` when
   * none is (none was made, or it was undone).
   */
  lastNamespaceEdit(): UndoEntry | null {
    const steps = this.history.undoSteps;
    for (let at = steps.length - 1; at >= 0; at--) {
      const step = steps[at]!;
      if (this.#namespaceSteps.has(step)) return this.#entry(step);
    }
    return null;
  }

  /**
   * Applies `edits` to `layer`, a layer of the stage's own stack, whatever
   * {@link Session.target} is, as one undoable action: for what belongs to
   * one layer (its sublayers, the stage's metadata in the root).
   */
  applyIn(label: string, layer: Layer, edits: readonly Edit[]): EditReceipt {
    return this.#record(label, () => this.stage.apply(edits, layer));
  }

  /**
   * Applies `edits` to `layer`'s sublayers (`insertSublayer` and its
   * siblings) as one undoable action, then supplies what the stack now
   * names that the stage has not read: a sublayer added is unavailable
   * until then, and one missing from the source is then unresolved.
   */
  async editSublayers(label: string, layer: Layer, edits: readonly Edit[], signal?: AbortSignal): Promise<EditReceipt> {
    const receipt = this.applyIn(label, layer, edits);
    await this.supplyUnavailable(label, undefined, signal);
    return receipt;
  }

  /**
   * Clears the metadata field `key` of `layer` (`clearLayerMetadata`), as
   * one undoable action; throws when the layer does not author it, and
   * records nothing.
   */
  clearLayerMetadata(label: string, layer: Layer, key: string): EditReceipt {
    const receipt = this.applyIn(label, layer, [{ type: "clearLayerMetadata", key }]);
    if (receipt.layers.length === 0) throw new Error(`${layer.identifier ?? "the session layer"} has no ${key} to clear`);
    return receipt;
  }

  /**
   * Reads `layers` again from the source (`Stage.reloadLayers`): each
   * one's bytes are fetched and supplied again, and the stage reads again
   * those whose bytes changed, publishing only what changed; what the new
   * content names that the stage has not read is then supplied. A layer
   * gone from the source keeps its content. Throws `io.dirty_reload` when
   * one has unsaved edits, with nothing read: the caller leaves those out.
   */
  async reload(layers: readonly Layer[], signal?: AbortSignal): Promise<ReloadResult> {
    const cause = layers.length === 1 ? `reload ${layers[0]!.identifier ?? "the session layer"}` : `reload ${layers.length} layers`;
    this.#cause = cause;
    // A package member is read with its package: the package's bytes are what the source holds.
    const identifiers = [
      ...new Set(layers.flatMap((layer) => (layer.identifier === null ? [] : [layer.identifier.includes("[") ? layer.identifier.slice(0, layer.identifier.indexOf("[")) : layer.identifier]))),
    ];
    const fetched = await unlessAborted(Promise.all(identifiers.map((identifier) => this.#fetch(identifier, signal))), signal);
    identifiers.forEach((identifier, index) => {
      const bytes = fetched[index] ?? null;
      // Gone from the source: the layer keeps what it read.
      if (bytes === null) {
        this.#log("fetch", identifier, "not found, kept as read");
        return;
      }
      this.stage.supply(identifier, bytes);
      this.#log("supply", identifier, `${bytes.byteLength} B, to reload`);
    });
    const result = this.stage.reloadLayers(layers);
    this.#logLoad(result);
    await this.supplyUnavailable(cause, undefined, signal);
    return result;
  }

  /**
   * Clears the attribute at `path` where edits go: its default (a value or
   * a block) and its samples at `samples` (stage times), as
   * `UsdAttribute::Clear` and `ClearAtTime` do; its value falls back to
   * weaker opinions, or the schema's fallback. Throws when the target had
   * none of them, and records nothing.
   */
  clearValue(path: string, samples: readonly number[] = []): EditReceipt {
    const edits: Edit[] = [{ type: "clearValue", path }, ...samples.map((time): Edit => ({ type: "clearValue", path, time }))];
    return this.#changing(`clear ${path}`, edits, `${this.#targetName} has no value of ${path} to clear`);
  }

  /** Clears the attribute at `path`'s sample at stage time `time` where edits go (`UsdAttribute::ClearAtTime`); throws when there was none. */
  clearSample(path: string, time: number): EditReceipt {
    return this.#changing(`delete key ${path} at ${time}`, [{ type: "clearValue", path, time }], `${this.#targetName} has no key of ${path} at ${time}`);
  }

  /** Blocks the attribute at `path` where edits go (`UsdAttribute::Block`): its samples there are cleared and weaker opinions hidden. */
  blockValue(path: string): EditReceipt {
    return this.#changing(`block ${path}`, [{ type: "blockValue", path }], `${path} is blocked in ${this.#targetName} already`);
  }

  /** Removes the property at `path`'s spec where edits go (`UsdPrim::RemoveProperty`); throws when the target has none. */
  removeProperty(path: string): EditReceipt {
    return this.#changing(`remove ${path}`, [{ type: "removeProperty", path }], `${this.#targetName} has no opinion of ${path} to remove`);
  }

  /**
   * Creates the attribute at `path` where edits go (`UsdPrim::CreateAttribute`):
   * `typeName` and `variability` for an attribute nothing declares yet
   * (`custom`), and `value` as its default when given.
   */
  createAttribute(path: string, typeName: string, variability: "varying" | "uniform", value?: ValueInput): EditReceipt {
    const edit: Edit = value === undefined ? { type: "createAttribute", path, typeName, variability } : { type: "createAttribute", path, typeName, variability, value };
    return this.#changing(`create ${path}`, [edit], `${path} exists in ${this.#targetName} already`);
  }

  /** Authors the relationship at `path`'s explicit targets where edits go (`UsdRelationship::SetTargets`), creating a `custom` one when nothing declares it. */
  setTargets(path: string, targets: readonly string[]): EditReceipt {
    return this.apply(`set targets of ${path}`, [{ type: "setTargets", path, targets }]);
  }

  /**
   * Adds `arc` to the prim at `prim` where edits go, as one undoable
   * action (`UsdReferences::AddReference` and its siblings). An asset the
   * stage has not read is then unavailable until it is supplied
   * ({@link Session.supplyUnavailable}).
   */
  addArc(prim: string, arc: NewArc): EditReceipt {
    return this.apply(newArcLabel(prim, arc), [addArcEdit(prim, arc)]);
  }

  /**
   * Removes `arc`, which the edit target's layer authors on the prim at
   * `prim`, as one undoable action: it leaves the layer's list and is
   * deleted from weaker layers' (`UsdReferences::RemoveReference` and its
   * siblings).
   */
  removeArc(prim: string, arc: AuthoredArc): EditReceipt {
    const through = this.target instanceof EditTarget ? this.target : null;
    return this.apply(removeArcLabel(prim, arc), [removeArcEdit(prim, arc, through)]);
  }

  /**
   * Adds the variant `variant` to the set `set` of the prim at `prim`
   * where edits go (the set too, when the layer holds none,
   * `UsdVariantSet::AddVariant`), and selects it, as one undoable action.
   * `newSet` names the action as adding the set.
   */
  addVariant(prim: string, set: string, variant: string, newSet = false): EditReceipt {
    const label = newSet ? `add variant set ${set} (${variant}) to ${prim}` : `add variant ${set}=${variant} to ${prim}`;
    return this.apply(label, [
      { type: "addVariant", path: prim, set, variant },
      { type: "setVariantSelection", path: prim, set, variant },
    ]);
  }

  /**
   * Removes the edit target's layer's spec of the variant set `set` of the
   * prim at `prim`, with its variants and what they author, and the
   * layer's selection in it, as one undoable action
   * (`SdfPrimSpec::RemoveVariantSet`). As in OpenUSD, the prim's
   * `variantSetNames` keeps the name. Throws when the layer holds no such
   * set.
   */
  removeVariantSet(prim: string, set: string): EditReceipt {
    return this.#changing(
      `remove variant set ${set} from ${prim}`,
      [
        { type: "removeVariantSet", path: prim, set },
        { type: "setVariantSelection", path: prim, set, variant: null },
      ],
      `${this.#targetName} holds no variant set ${set} of ${prim}`,
    );
  }

  /**
   * Applies `edits` where edits go as one undoable action, or, when they
   * wrote no layer (clearing an opinion the target does not hold), throws
   * `nothing(layer)`, the layer named, and records nothing.
   */
  applyChanging(label: string, edits: readonly Edit[], nothing: (layer: string) => string): EditReceipt {
    return this.#changing(label, edits, nothing(this.#targetName));
  }

  /** Where edits go, in words: the layer written (`scene.usda`). */
  get targetName(): string {
    return this.#targetName;
  }

  get #targetName(): string {
    const layer = this.target === undefined ? this.stage.editLayer : "kind" in this.target ? this.target.layer : this.target;
    return layer?.identifier ?? "the anonymous edit layer";
  }

  /** Applies `edits` as one undoable action, or throws `nothing` when they wrote no layer (and nothing is recorded). */
  #changing(label: string, edits: readonly Edit[], nothing: string): EditReceipt {
    const receipt = this.apply(label, edits);
    if (receipt.layers.length === 0) throw new Error(nothing);
    return receipt;
  }

  /**
   * Selects `variant` of `set` on `prim`, then supplies what the variant
   * needs that the stage has not read, step by step.
   */
  async selectVariant(prim: string, set: string, variant: string, signal?: AbortSignal): Promise<EditReceipt> {
    const receipt = this.apply(`select ${set}=${variant} on ${prim}`, [
      { type: "setVariantSelection", path: prim, set, variant },
    ]);
    await this.supplyUnavailable(`${set}=${variant}`, undefined, signal);
    return receipt;
  }

  /**
   * Undoes the newest action; what it brings back may want assets, which
   * are supplied. A step an edit outside the history made stale throws
   * `history.stale` and stays. Returns the move it made, when the step
   * was one: back from where the step moved its prim to.
   */
  async undo(signal?: AbortSignal): Promise<PathMove | null> {
    const step = this.history.nextUndo;
    if (step === null) return null;
    const cause = `undo ${step.label}`;
    this.#cause = cause;
    const moved = this.#moves.get(step);
    this.history.undo();
    await this.supplyUnavailable(cause, undefined, signal);
    return moved === undefined ? null : { from: moved.to, to: moved.from };
  }

  /** Redoes the most recently undone action. Returns the move it made, when the step was one. */
  async redo(signal?: AbortSignal): Promise<PathMove | null> {
    const step = this.history.nextRedo;
    if (step === null) return null;
    const cause = `redo ${step.label}`;
    this.#cause = cause;
    this.history.redo();
    await this.supplyUnavailable(cause, undefined, signal);
    return this.#moves.get(step) ?? null;
  }

  /**
   * Mutes or unmutes `layer`: the stage recomposes without or with its
   * opinions, heard as one change. Muting is the stage's state, not an
   * edit, so it is not on the undo stack. What an unmuted layer asks for
   * that the stage has not read is supplied. Muting releases the layers
   * the stage no longer uses ({@link Session.releaseUnusedLayers}), the
   * muted one among them unless it has unsaved edits; it is then unmuted
   * by identifier, and fetched again. Returns whether the muting changed.
   */
  async setMuted(layer: Layer | string, muted: boolean, signal?: AbortSignal): Promise<boolean> {
    const name = typeof layer === "string" ? layer : (layer.identifier ?? "(anonymous)");
    const cause = `${muted ? "mute" : "unmute"} ${name}`;
    this.#cause = cause;
    const changed = muted ? this.stage.muteLayer(layer) : this.stage.unmuteLayer(layer);
    if (muted) this.releaseUnusedLayers();
    else await this.supplyUnavailable(cause, undefined, signal);
    return changed;
  }

  /** The layer the stage holds with `identifier`; throws when it holds none. */
  layerNamed(identifier: string): Layer {
    const layer = this.stage.layers().find((candidate) => candidate.identifier === identifier);
    if (layer === undefined) throw new Error(`the stage holds no layer ${identifier}`);
    return layer;
  }

  /**
   * The session layer edits go to by default, when the stage was opened
   * with one (`editLayer: "session"`, or a root that cannot take edits,
   * {@link Session.sessionNote}); `null` when edits go to the root.
   */
  get sessionLayer(): Layer | null {
    return this.#open.editLayer === "session" || this.#open.sessionLayer !== undefined ? this.stage.editLayer : null;
  }

  /**
   * Whether `layer` has edits that are not saved. `Stage.isDirty` says the
   * layer changed since it was read or saved, and stays so once those
   * changes are undone, as OpenUSD's `SdfLayer::IsDirty` does; so a dirty
   * layer counts as edited only when a step between the history as it
   * stood then and as it stands now writes it: undo back to the save, and
   * it is not. While a group is open (its receipts are no step yet), a
   * dirty layer counts.
   */
  isEdited(layer: Layer): boolean {
    try {
      if (!this.stage.isDirty(layer)) return false;
    } catch (error) {
      // A layer the stage released (muted, or unloaded with its payload) has no edits to save; what still holds it may ask.
      if (error instanceof LayerstackError && error.code === "layer.unknown") return false;
      throw error;
    }
    if (this.history.groupDepth > 0) return true;
    const saved = this.#savedAt.get(layer.id) ?? this.#opened;
    if (this.history.isAt(saved.mark)) return false;
    return stepsBetween(saved.steps, this.history.undoSteps).some((step) => step.layers.some((written) => written.id === layer.id));
  }

  /** The layers with unsaved edits ({@link Session.isEdited}). */
  dirtyLayers(): Layer[] {
    return this.stage.layers().filter((layer) => this.isEdited(layer));
  }

  /**
   * Saves the dirty layers: their bytes are the host's to persist
   * ({@link Session.saved}). A save of the stage leaves the session layer
   * (export it, `Stage.exportLayer`) and muted layers dirty.
   */
  save(): SaveResult {
    this.#cause = "save";
    const result = this.stage.save();
    this.saved = result.written.map(({ identifier, bytes }) => ({ identifier, bytes }));
    const point = this.#historyPoint();
    for (const { layer } of result.written) this.#savedAt.set(layer.id, point);
    return result;
  }

  /**
   * The root and everything it depends on as a USDZ package
   * (`Stage.exportPackage`), the unsaved edits with it. The stage packages
   * what the host supplied; the assets it was never asked for (textures,
   * the layers of a variant not selected) or whose bytes it let go once
   * read are fetched here and supplied, every tile of a UDIM set at once
   * (those `exists` says are there), and the package made again, until
   * nothing more can be supplied. What is still unresolved then is in the
   * result's `unresolved`, and its `bytes` are `null`. Returns, with the
   * result, how many assets were fetched for it.
   */
  async exportPackage(
    exists?: (identifier: string) => Promise<boolean>,
    signal?: AbortSignal,
  ): Promise<{ readonly result: PackageResult; readonly fetched: number }> {
    this.#cause = "package";
    // What was fetched for the package, supplied again before each try: a layer the packager reads is let go.
    const supplied = new Map<string, Uint8Array | null>();
    let result = this.stage.exportPackage();
    for (let round = 0; round < MAX_PACKAGE_ROUNDS && result.bytes === null; round++) {
      const wants = packageWants(result.unresolved);
      const fresh = wants.fetch.filter((identifier) => !supplied.has(identifier));
      const tiles = wants.tiles.filter((identifier) => !supplied.has(identifier));
      // Nothing new to fetch, and what was fetched supplied again already: nothing more helps.
      if (fresh.length === 0 && tiles.length === 0 && wants.fetch.length === 0) break;
      const found = await unlessAborted(Promise.all(tiles.map(async (tile) => (exists === undefined || (await exists(tile)) ? tile : null))), signal);
      const identifiers = [...fresh, ...found.filter((tile) => tile !== null)];
      const bytes = await unlessAborted(Promise.all(identifiers.map((identifier) => this.#fetch(identifier, signal))), signal);
      identifiers.forEach((identifier, index) => {
        const data = bytes[index] ?? null;
        supplied.set(identifier, data);
        this.#log("supply", identifier, data === null ? "confirmed absent, for the package" : `${data.byteLength} B, for the package`);
      });
      // Tiles that are not there are not supplied: the packager asks only for those it is told exist.
      for (const tile of tiles) if (!supplied.has(tile)) supplied.set(tile, null);
      for (const [identifier, data] of supplied) this.stage.supply(identifier, data);
      result = this.stage.exportPackage();
    }
    return { result, fetched: [...supplied.values()].filter((data) => data !== null).length };
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

  /** Where the history stands: a mark of it, and its undo steps. */
  #historyPoint(): HistoryPoint {
    return { mark: this.history.mark(), steps: this.history.undoSteps };
  }

  #entry(step: HistoryStep): UndoEntry {
    const receipt = this.#reports.get(step);
    if (receipt === undefined) throw new Error(`no receipt for the step ${step.label}`);
    return { label: step.label, step, receipt };
  }

  /**
   * Loads the payloads at `load` (and beneath, with `"withDescendants"`)
   * after unloading those at `unload`, as one request
   * (`Stage.loadAndUnload`), then fetches what they want within `budget`.
   * Returns what the request did (`LoadOutcome`: whether the load rules
   * changed, and the paths the stage refused). Over the budget it throws
   * {@link OverBudget}, the rules as the request left them and the
   * layers fetched so far read; the caller decides what to unload. So it
   * does when `signal` aborts the fetching: the rules are as the request
   * left them, and the caller puts back those it had.
   */
  async loadPayloads(
    load: readonly string[],
    unload: readonly string[],
    policy: LoadPolicy = "withDescendants",
    budget?: LoadBudget,
    signal?: AbortSignal,
  ): Promise<LoadOutcome> {
    const cause = load.length > 0 ? `load ${load.join(", ")}` : `unload ${unload.join(", ")}`;
    this.#cause = cause;
    const outcome = this.stage.loadAndUnload(load, unload, policy);
    await this.supplyUnavailable(cause, budget, signal);
    return outcome;
  }

  /** The rounds of {@link Session.supplyUnavailable}, recorded into the current run. */
  async #rounds(budget?: LoadBudget, signal?: AbortSignal): Promise<number> {
    const supplied = new Set<string>();
    // What the run fetched, against the budget.
    let fetchedBytes = 0;
    // The layers read since the stage last composed; `null` when none was.
    let read: number | null = null;
    const compose = async () => {
      const layers = read ?? 0;
      read = null;
      await this.#phase?.({ kind: "composing", layers });
      signal?.throwIfAborted();
      const start = performance.now();
      const result = this.stage.loadSupplied();
      this.loops.composed(result, start, performance.now());
      this.#logLoad(result);
    };
    for (let round = 0; round < MAX_ROUNDS; ) {
      signal?.throwIfAborted();
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
      if (budget !== undefined) {
        const sizes = await unlessAborted(Promise.all(identifiers.map((identifier) => budget.size(identifier))), signal);
        const planned = fetchedBytes + sizes.reduce<number>((sum, size) => sum + (size ?? 0), 0);
        if (planned > budget.bytes) {
          // Nothing more is fetched, and what the rounds read is left for
          // the stage to compose under the rules the caller sets next
          // (unloading it, it composes none of it).
          throw new OverBudget(planned, budget.bytes);
        }
      }
      await this.#phase?.({ kind: "fetching", round, assets: identifiers.length });
      signal?.throwIfAborted();
      // Aborted, the round is not supplied: the fetches in flight are told, and not waited for.
      const settled = await unlessAborted(Promise.allSettled(identifiers.map((identifier) => this.#source(identifier, signal))), signal);
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
      fetchedBytes += suppliedBytes;
      await this.#phase?.({ kind: "reading", round, layers: identifiers.length, bytes: suppliedBytes });
      // Not checked for an abort here: what was supplied is read, so the stage holds nothing half taken.
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
  async #source(identifier: string, signal?: AbortSignal): Promise<Uint8Array | null> {
    this.loops.fetchStarted(identifier, performance.now());
    const local = this.#local.get(identifier);
    if (local !== undefined) {
      this.loops.fetchSettled(identifier, performance.now(), local.byteLength, "saved");
      this.#log("fetch", identifier, `${local.byteLength} B from the saved layers`);
      return local;
    }
    let bytes: Uint8Array | null;
    try {
      bytes = await this.#fetch(identifier, signal);
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
