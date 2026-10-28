// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { ArrayCopy, ArrayHandle } from "./arrays.js";
import type { NativeArrayHandle } from "./arrays.js";
import { lowerEdits } from "./edits.js";
import type { NativeEditBatch } from "./edits.js";
import { LayerstackError } from "./errors.js";
import type { ErrorCode } from "./errors.js";
import type {
  ArcKind,
  ArrayData,
  AssetFailure,
  AssetSource,
  AssetUse,
  ChangeListener,
  Changes,
  Edit,
  Explanation,
  FlattenResult,
  Issue,
  LayerFormat,
  LoadResult,
  OpenOptions,
  Opinion,
  PackageResult,
  ResolveOptions,
  ResolveResult,
  SaveResult,
  Scalar,
  StageStatus,
  Transfer,
  UnavailableAsset,
  UnresolvedAsset,
  Value,
  ValueSource,
  VariantSet,
  Version,
} from "./types.js";

/** This package's version. */
export const PACKAGE_VERSION = "0.1.0";

/**
 * The ABI this package speaks: the version of the records it exchanges
 * with the native module (`layerstack_wasm::ABI`).
 */
export const ABI = 6;

/** The native stage the generated glue exports (`layerstack_wasm::Stage`). Not for app use. */
export interface NativeStage {
  free(): void;
  unavailable(): unknown;
  supply(identifier: string, bytes: Uint8Array): void;
  confirmAbsent(identifier: string): void;
  loadSupplied(): unknown;
  layers(): unknown;
  layerDirty(id: bigint): boolean;
  status(): unknown;
  prim(path: string): unknown;
  children(path: string): string[];
  attributeNames(path: string): string[];
  variantSets(path: string): unknown;
  attribute(path: string): unknown;
  get(path: string, time?: number | null): unknown;
  timeSamples(path: string): Float64Array;
  explain(path: string, time?: number | null): unknown;
  metadata(path: string, key: string): unknown;
  relationship(path: string): unknown;
  apply(target: bigint, batch: NativeEditBatch): NativeReceipt;
  undo(receipt: NativeReceipt): NativeReceipt;
  takeChanges(render: boolean): unknown;
  save(layer?: bigint | null): unknown;
  exportLayer(layer: bigint, format: string): Uint8Array;
  exportFlattened(format: string): unknown;
  exportPackage(): unknown;
}

/** The native receipt the generated glue exports (`layerstack_wasm::EditReceipt`). Not for app use. */
export interface NativeReceipt {
  free(): void;
  record(): unknown;
  readonly undoable: boolean;
}

/**
 * An initialized native module: the generated glue of `layerstack_wasm`,
 * or of a module that links it (such as a combined module of another
 * crate). Pass it to {@link loadLayerstack} to use that module.
 */
export interface NativeModule {
  readonly Stage: {
    open(
      identifier: string,
      bytes: Uint8Array,
      recover: boolean,
      sessionLayer: boolean,
      primIndexNodes: number | undefined,
      nodesPerPrimIndex: number | undefined,
      composedDepth: number | undefined,
    ): NativeStage;
  };
  /** The batch of edits `Stage.apply` takes. */
  readonly EditBatch: new () => NativeEditBatch;
  nativeVersion(): unknown;
  /** The module's memory, for {@link Layerstack.transfer}; optional. */
  readonly memory?: WebAssembly.Memory;
}

type Row = readonly unknown[];

const owners = new WeakMap<object, Stage>();
const receiptNatives = new WeakMap<EditReceipt, NativeReceipt>();

/**
 * One instance of the native module: opens stages and counts the bytes
 * that cross the boundary. Get one with {@link loadLayerstack}.
 */
export class Layerstack {
  /** This package's and the module's versions. */
  readonly version: Version;
  readonly #module: NativeModule;
  #trapped: unknown = undefined;
  #bytesIn = 0;
  #bytesOut = 0;
  #views = 0;
  #bytesViewed = 0;

  /** Wraps an initialized module, refusing one of another ABI (`module.abi`). */
  constructor(module: NativeModule) {
    this.#module = module;
    const [crate, revision, abi] = module.nativeVersion() as [string, string, number];
    if (abi !== ABI) {
      throw new LayerstackError(
        "module.abi",
        "loadLayerstack",
        `the module speaks ABI ${abi}, this package ABI ${ABI}; use a module built from a matching layerstack_wasm`,
      );
    }
    this.version = Object.freeze({ package: PACKAGE_VERSION, crate, revision, abi });
  }

  /**
   * Opens a stage from the bytes of its root layer, copied once into the
   * module. Nothing is fetched: the stage composes what it has and wants
   * the rest ({@link Stage.unavailable}, {@link Stage.resolve}).
   *
   * Throws `io.rejected` for a root that does not parse.
   *
   * To edit a root that cannot be saved (a USDZ package), pass
   * `editLayer: "session"`: edits then go to an anonymous session layer in
   * front of it ({@link Stage.editLayer}).
   */
  open(root: { readonly identifier: string; readonly bytes: Uint8Array }, options: OpenOptions = {}): Stage {
    if (typeof root?.identifier !== "string" || !(root.bytes instanceof Uint8Array)) {
      throw new LayerstackError("argument.invalid", "open", "expected { identifier: string, bytes: Uint8Array }");
    }
    const editLayer = options.editLayer ?? "root";
    if (editLayer !== "root" && editLayer !== "session") {
      throw new LayerstackError("argument.invalid", "open", 'editLayer must be "root" or "session"');
    }
    const recover = options.importPolicy === "recover";
    const session = editLayer === "session";
    const limits = options.limits;
    const nodes = limit(limits?.primIndexNodes, "primIndexNodes");
    const perIndex = limit(limits?.nodesPerPrimIndex, "nodesPerPrimIndex");
    const depth = limit(limits?.composedDepth, "composedDepth");
    const native = this.call("open", () =>
      this.#module.Stage.open(root.identifier, root.bytes, recover, session, nodes, perIndex, depth),
    );
    this.#bytesIn += root.bytes.byteLength;
    return new Stage(this, native);
  }

  /** @internal A new native batch of edits. */
  batch(): NativeEditBatch {
    return new this.#module.EditBatch();
  }

  /** The bytes that crossed the boundary through this instance so far. */
  transfer(): Transfer {
    return {
      bytesIn: this.#bytesIn,
      bytesOut: this.#bytesOut,
      views: this.#views,
      bytesViewed: this.#bytesViewed,
      memoryBytes: this.#module.memory?.buffer.byteLength ?? 0,
    };
  }

  /** @internal The module's memory, if it exposes it. */
  memory(): WebAssembly.Memory | undefined {
    return this.#module.memory;
  }

  /** @internal Counts bytes copied in or out, or a view built over the module's memory. */
  count(direction: "in" | "out" | "view", bytes: number): void {
    if (direction === "in") this.#bytesIn += bytes;
    else if (direction === "out") this.#bytesOut += bytes;
    else {
      this.#views += 1;
      this.#bytesViewed += bytes;
    }
  }

  /**
   * @internal Runs a native call, turning its thrown record into a
   * {@link LayerstackError} and a trap into `module.trapped`.
   */
  call<T>(operation: string, f: () => T): T {
    if (this.#trapped !== undefined) {
      throw new LayerstackError("module.trapped", operation, "the module trapped earlier; load a new instance", {
        cause: this.#trapped,
      });
    }
    try {
      return f();
    } catch (error) {
      throw this.#translate(operation, error);
    }
  }

  #translate(operation: string, error: unknown): unknown {
    if (Array.isArray(error) && typeof error[0] === "string" && typeof error[1] === "string") {
      const [code, message, identifier, path, edit] = error as [
        ErrorCode,
        string,
        string | null,
        string | null,
        number | null | undefined,
      ];
      return new LayerstackError(code, operation, message, {
        identifier: identifier ?? undefined,
        path: path ?? undefined,
        edit: edit ?? undefined,
      });
    }
    if (error instanceof WebAssembly.RuntimeError) {
      this.#trapped = error;
      return new LayerstackError("module.trapped", operation, `the module trapped: ${error.message}`, {
        cause: error,
      });
    }
    return error;
  }
}

/** A layer of a stage: its identity. Ask its stage about its state ({@link Stage.isDirty}). */
export class Layer {
  /**
   * The layer's id in its stage. Ids may exceed 2^53; they are never
   * needed for normal work.
   */
  readonly id: bigint;
  /** The identifier it was read from (`set/scene.usda`, `scene.usdz[root.usdc]` for a package member); `null` for an anonymous layer. */
  readonly identifier: string | null;
  /** The identifier's extension (a package member's own): `usda`, `usdc`, `usdz`, or `""`. */
  readonly format: string;
  /** Whether it is read-only, as a package member is. */
  readonly readOnly: boolean;

  /** @internal */
  constructor(stage: Stage, id: bigint, identifier: string | null, format: string, readOnly: boolean) {
    this.id = id;
    this.identifier = identifier;
    this.format = format;
    this.readOnly = readOnly;
    owners.set(this, stage);
    Object.freeze(this);
  }

  /** Whether it has no identifier. */
  get anonymous(): boolean {
    return this.identifier === null;
  }
}

/**
 * An open stage. Reads are synchronous; only {@link Stage.resolve}, which
 * waits on the host's fetches, is asynchronous.
 *
 * Edits are plain data ({@link Edit}) applied atomically to one layer
 * ({@link Stage.apply}); each apply returns an {@link EditReceipt} that
 * undoes it. Listeners ({@link Stage.onChange}) hear every composed change
 * synchronously, after each call that changes the stage.
 *
 * Paths are text: prim paths (`/World/Rock`) and property paths
 * (`/World/Rock.size`). A stage owns native memory: dispose it with
 * {@link Stage.free} or `using`; every call after that fails with
 * `stage.disposed`.
 */
export class Stage implements Disposable {
  readonly #ls: Layerstack;
  #native: NativeStage | null;
  readonly #layers = new Map<bigint, Layer>();
  #root: Layer | null = null;
  #editLayer: Layer | null | undefined = undefined;
  readonly #listeners: ChangeListener[] = [];
  #dispatching = false;

  /** @internal Use {@link Layerstack.open}. */
  constructor(layerstack: Layerstack, native: NativeStage) {
    this.#ls = layerstack;
    this.#native = native;
  }

  /** The instance that opened this stage. */
  get layerstack(): Layerstack {
    return this.#ls;
  }

  /** Whether the stage was disposed. */
  get disposed(): boolean {
    return this.#native === null;
  }

  /** The root layer. */
  get root(): Layer {
    if (this.#root === null) this.layers();
    return this.#root as Layer;
  }

  /**
   * The layer edits go to by default: the session layer when opened with
   * `editLayer: "session"`, else the root when it can be saved, else
   * `null` (a USDZ root opened without a session layer).
   */
  get editLayer(): Layer | null {
    if (this.#editLayer === undefined) this.layers();
    return this.#editLayer ?? null;
  }

  /** The assets the stage asks for that the host has not supplied yet: they may still arrive. */
  unavailable(): UnavailableAsset[] {
    const rows = this.#call("unavailable", (native) => native.unavailable()) as Row[];
    return rows.map((row) => this.#unavailable(row));
  }

  /**
   * Supplies the bytes of `identifier` (copied once into the module), or
   * `null` to confirm there is no such asset. An unavailable asset is read at the
   * next {@link Stage.loadSupplied}; a layer already read stays as it was
   * read.
   */
  supply(identifier: string, bytes: Uint8Array | null): void {
    if (typeof identifier !== "string" || !(bytes === null || bytes instanceof Uint8Array)) {
      throw new LayerstackError("argument.invalid", "supply", "expected (identifier: string, bytes: Uint8Array | null)");
    }
    if (bytes === null) {
      this.#call("supply", (native) => native.confirmAbsent(identifier));
    } else {
      this.#call("supply", (native) => native.supply(identifier, bytes));
      this.#ls.count("in", bytes.byteLength);
    }
  }

  /**
   * Reads what was supplied for the unavailable assets. It reads no
   * layer already read, so it works whatever layers are dirty. A supplied
   * layer may ask for more: call {@link Stage.unavailable} again.
   *
   * Throws `io.rejected` when a supplied layer does not parse under the
   * strict import policy; the layers read before it stay.
   */
  loadSupplied(): LoadResult {
    const [loaded, failures] = this.#call("loadSupplied", (native) => native.loadSupplied()) as [bigint[], Row[]];
    this.#notify();
    return {
      loaded: loaded.map((id) => this.#layer(id)),
      failures: failures.map(([asset, anchor, code, message]) => this.#failure(asset, anchor, code, message)),
    };
  }

  /**
   * Runs the loop until no asset is unavailable: {@link Stage.unavailable},
   * fetch every unavailable identifier from `source` at once, {@link Stage.supply}
   * what arrived, {@link Stage.loadSupplied}.
   *
   * A fetch that throws fails the loop with `resolve.fetch` (naming the
   * identifier) after what the other fetches of its round returned is
   * loaded. `signal` aborts it with `resolve.aborted`.
   */
  async resolve(source: AssetSource, options: ResolveOptions = {}): Promise<ResolveResult> {
    const { signal, maxRounds = 100 } = options;
    const loaded: Layer[] = [];
    const failures: AssetFailure[] = [];
    const supplied = new Set<string>();
    let rounds = 0;
    let bytes = 0;
    for (;;) {
      const unavailable = [...new Set(this.unavailable().map((asset) => asset.identifier))];
      if (unavailable.length === 0) break;
      const stuck = unavailable.find((identifier) => supplied.has(identifier));
      if (stuck !== undefined) {
        throw new LayerstackError("resolve.stuck", "resolve", "an asset was supplied but is still unavailable", {
          identifier: stuck,
        });
      }
      if (rounds === maxRounds) {
        throw new LayerstackError("resolve.limit", "resolve", `assets still unavailable after ${maxRounds} rounds`);
      }
      rounds += 1;
      if (signal?.aborted) {
        throw new LayerstackError("resolve.aborted", "resolve", "the signal aborted", { cause: signal.reason });
      }
      const settled = await Promise.allSettled(unavailable.map((identifier) => source.fetch(identifier, signal)));
      if (signal?.aborted) {
        throw new LayerstackError("resolve.aborted", "resolve", "the signal aborted", { cause: signal.reason });
      }
      let failed: LayerstackError | null = null;
      settled.forEach((outcome, index) => {
        const identifier = unavailable[index] as string;
        if (outcome.status === "rejected") {
          failed ??= new LayerstackError("resolve.fetch", "resolve", "the asset source failed to fetch", {
            identifier,
            cause: outcome.reason,
          });
          return;
        }
        this.supply(identifier, outcome.value);
        supplied.add(identifier);
        bytes += outcome.value?.byteLength ?? 0;
      });
      const result = this.loadSupplied();
      loaded.push(...result.loaded);
      failures.push(...result.failures);
      if (failed !== null) throw failed;
    }
    return { loaded, failures, rounds, fetched: supplied.size, bytes };
  }

  /** Every layer the stage holds, read or anonymous. */
  layers(): Layer[] {
    const [root, , rows, editLayer] = this.#call("layers", (native) => native.layers()) as [
      bigint,
      bigint | null,
      Row[],
      bigint | null,
    ];
    const layers = rows.map(([id, identifier, format, readOnly]) =>
      this.#remember(id as bigint, identifier as string | null, format as string, readOnly as boolean),
    );
    this.#root = this.#layers.get(root) ?? null;
    this.#editLayer = editLayer === null ? null : (this.#layers.get(editLayer) ?? null);
    return layers;
  }

  /** Whether `layer` changed since it was read or last saved. Throws `layer.affinity` for another stage's layer. */
  isDirty(layer: Layer): boolean {
    this.#own("isDirty", layer);
    return this.#call("isDirty", (native) => native.layerDirty(layer.id));
  }

  /** The stage's state now: prims, composition errors, unavailable and unresolved assets. */
  status(): StageStatus {
    const [prims, errors, rows] = this.#call("status", (native) => native.status()) as [number, Row[], Row[]];
    const unavailable = this.unavailable();
    const unresolved: UnresolvedAsset[] = rows.map(([anchor, assetPath, usedBy, identifier, reason]) => ({
      assetPath: assetPath as string,
      anchor: this.#layer(anchor as bigint),
      usedBy: usedBy as AssetUse,
      identifier: identifier as string | null,
      reason: reason as string | null,
    }));
    return {
      prims,
      compositionErrors: errors.map(
        ([code, message, prim]): Issue => ({
          code: code as string,
          message: message as string,
          prim: prim as string | null,
        }),
      ),
      unavailable,
      unresolved,
      complete: unavailable.length === 0 && unresolved.length === 0,
    };
  }

  /** The composed prim at `path`, or `null`. Throws `path.invalid` for a path that is not an absolute prim path. */
  prim(path: string): Prim | null {
    const row = this.#call("prim", (native) => native.prim(path)) as Row | null;
    return row === null ? null : new Prim(this, path, row);
  }

  /** The paths of every composed child of the prim at `path`, in namespace order, inactive and abstract ones included. Throws `prim.not_found`. */
  children(path: string): string[] {
    return this.#call("children", (native) => native.children(path));
  }

  /** The attribute at the property path `path` (`/World/Rock.size`), or `null`. */
  attribute(path: string): Attribute | null {
    const row = this.#call("attribute", (native) => native.attribute(path)) as Row | null;
    return row === null ? null : new Attribute(this, path, row);
  }

  /** The relationship at the property path `path` (`/World.lookAt`), or `null`. */
  relationship(path: string): Relationship | null {
    const row = this.#call("relationship", (native) => native.relationship(path)) as Row | null;
    return row === null ? null : new Relationship(this, path, row);
  }

  /**
   * Applies `edits` to the layer `target` atomically: every edit applies,
   * or none does and this throws. Assets the edits ask for that the stage
   * has not read (a new reference, a variant's payload) are unavailable
   * afterwards: the receipt lists them, and {@link Stage.resolve} fetches
   * them. Change listeners run before this returns.
   *
   * Throws `layer.affinity` (another stage's layer), `layer.read_only` (a
   * USDZ member: open with `editLayer: "session"`), `layer.outside_stack`,
   * `argument.invalid` or `value.*` for a malformed edit, and `edit.*`
   * when the edits are rejected (`edit.rejected.type_mismatch`, ...); the
   * error's `edit` is the index of the edit at fault.
   */
  apply(target: Layer, edits: readonly Edit[]): EditReceipt {
    this.#own("apply", target);
    const batch = this.#ls.batch();
    try {
      lowerEdits(batch, edits, "apply", this.#ls);
      const native = this.#call("apply", (stage) => stage.apply(target.id, batch));
      const receipt = this.#receipt(native);
      this.#notify();
      return receipt;
    } finally {
      batch.free();
    }
  }

  /**
   * Undoes `receipt`: applies its inverse, and returns the receipt of the
   * undo, whose own `undo()` redoes. `receipt` is spent (released).
   *
   * Throws `receipt.affinity` for another stage's receipt (checked before
   * anything runs), `receipt.released` for a spent one, and `edit.stale`
   * when a slot the edits wrote changed since: nothing changes, and
   * `receipt` stays undoable.
   */
  undo(receipt: EditReceipt): EditReceipt {
    if (!(receipt instanceof EditReceipt) || receipt.stage !== this) {
      throw new LayerstackError("receipt.affinity", "undo", "the receipt belongs to another stage");
    }
    const old = receiptNatives.get(receipt);
    if (old === undefined) {
      throw new LayerstackError("receipt.released", "undo", "the receipt was undone or released");
    }
    const native = this.#call("undo", (stage) => stage.undo(old));
    receipt.release();
    const redo = this.#receipt(native);
    this.#notify();
    return redo;
  }

  /**
   * Selects `variant` of the variant set `set` on `prim` (`null` clears
   * the selection), in `target` or the {@link Stage.editLayer}: a
   * `setVariantSelection` edit. A variant whose arcs need assets the stage
   * has not read leaves them unavailable: `await stage.resolve(source)` after.
   *
   * Throws `layer.read_only` when there is no `target` and no edit layer.
   */
  setVariantSelection(prim: Prim | string, set: string, variant: string | null, target?: Layer): EditReceipt {
    const path = typeof prim === "string" ? prim : prim.path;
    const layer = target ?? this.editLayer;
    if (layer === null) {
      throw new LayerstackError(
        "layer.read_only",
        "setVariantSelection",
        'the stage has no edit layer: its root cannot be saved; open it with editLayer: "session"',
      );
    }
    return this.apply(layer, [{ type: "setVariantSelection", path, set, variant }]);
  }

  /**
   * Calls `listener` with each composed change, synchronously and in
   * order, after every call that changes the stage: {@link Stage.apply},
   * {@link Stage.undo}, {@link Stage.loadSupplied} (and so
   * {@link Stage.resolve}, once per round). A listener that throws does not
   * stop the others; its error is reported (`reportError`, else
   * `console.error`) as `listener.failed`. A listener may edit the stage;
   * those changes are heard after the current ones. Returns the function
   * that removes the listener.
   */
  onChange(listener: ChangeListener): () => void {
    if (typeof listener !== "function") {
      throw new LayerstackError("argument.invalid", "onChange", "expected a function");
    }
    this.#call("onChange", () => undefined);
    this.#listeners.push(listener);
    return () => {
      const index = this.#listeners.indexOf(listener);
      if (index >= 0) this.#listeners.splice(index, 1);
    };
  }

  /**
   * Saves the dirty layers the stage uses (OpenUSD's `UsdStage::Save`), or
   * only `layer`: each in its own format, to the identifier it was read
   * from. The module writes nothing anywhere: persist `written` yourself.
   * The saved bytes also become what the stage reads those identifiers
   * from, and the layers are clean afterwards.
   *
   * The session layer is anonymous, so it is never saved: it is listed in
   * `anonymous`; export it with {@link Stage.exportLayer}. A package member
   * fails with `io.read_only` and stays dirty.
   *
   * Throws `layer.affinity` or `layer.unknown` for a `layer` not of this stage.
   */
  save(layer?: Layer): SaveResult {
    if (layer !== undefined) this.#own("save", layer);
    const [written, failures, anonymous] = this.#call("save", (native) => native.save(layer?.id ?? null)) as [
      Row[],
      Row[],
      bigint[],
    ];
    let bytes = 0;
    const saved = written.map(([id, identifier, data]) => {
      bytes += (data as Uint8Array).byteLength;
      return { layer: this.#layer(id as bigint), identifier: identifier as string, bytes: data as Uint8Array };
    });
    this.#ls.count("out", bytes);
    return {
      written: saved,
      failures: failures.map(([id, code, message]) => ({
        layer: this.#layer(id as bigint),
        code: code as string,
        message: message as string,
      })),
      anonymous: anonymous.map((id) => this.#layer(id)),
      bytes,
    };
  }

  /**
   * The bytes of `layer` as `format`: a copy, as OpenUSD's `SdfLayer::Export`
   * writes one. The layer keeps its identity and dirty state. Any layer
   * exports, the session layer and package members included.
   *
   * Throws `argument.invalid` for another format, `io.unsupported` for
   * content the format cannot hold, and `io.rejected` for a layer that
   * cannot be written as it stands (an attribute with no type, ...).
   */
  exportLayer(layer: Layer, format: LayerFormat): Uint8Array {
    this.#own("exportLayer", layer);
    const bytes = this.#call("exportLayer", (native) => native.exportLayer(layer.id, format));
    this.#ls.count("out", bytes.byteLength);
    return bytes;
  }

  /**
   * The composed stage as one layer with no composition arcs, in `format`
   * (`usda` by default), as OpenUSD's `UsdStage::Flatten` writes it:
   * instances keep sharing prototypes, asset paths are written as authored.
   * Opening the bytes on their own composes the same stage. Content a
   * layer cannot hold is left out and listed in `lost`, never silently.
   */
  exportFlattened(format: LayerFormat = "usda"): FlattenResult {
    const [bytes, lossless, lost] = this.#call("exportFlattened", (native) => native.exportFlattened(format)) as [
      Uint8Array,
      boolean,
      Row[],
    ];
    this.#ls.count("out", bytes.byteLength);
    return {
      bytes,
      lossless,
      lost: lost.map(([path, message]) => ({ path: path as string, message: message as string })),
    };
  }

  /**
   * The root layer and everything it depends on as a USDZ package, as
   * OpenUSD's `UsdUtilsCreateNewUsdzPackage` makes it: layers with their
   * unsaved edits, and the other assets the host supplied. When a
   * dependency is not available (supply it, or confirm it absent) `bytes`
   * is `null` and `unresolved` lists why.
   *
   * The session layer is not part of the root's dependencies, so it is not
   * packaged (`omitted`): export it with {@link Stage.exportLayer}, or the
   * composed stage with {@link Stage.exportFlattened}.
   */
  exportPackage(): PackageResult {
    const [bytes, layers, assets, unresolved, omitted] = this.#call("exportPackage", (native) =>
      native.exportPackage(),
    ) as [Uint8Array | null, number, number, Row[], bigint[]];
    this.#ls.count("out", bytes?.byteLength ?? 0);
    return {
      bytes,
      layers,
      assets,
      unresolved: unresolved.map(([layer, assetPath, reason]) => ({
        layer: this.#layer(layer as bigint),
        assetPath: assetPath as string,
        reason: reason as string,
      })),
      omitted: omitted.map((id) => this.#layer(id)),
    };
  }

  /** Disposes the native stage. Idempotent. */
  free(): void {
    const native = this.#native;
    this.#native = null;
    native?.free();
  }

  /** Disposes the native stage, for `using`. */
  [Symbol.dispose](): void {
    this.free();
  }

  /** @internal */
  native<T>(operation: string, f: (native: NativeStage) => T): T {
    return this.#call(operation, f);
  }

  /** @internal The layer with `id`, from the cache or a fresh inventory. */
  layerOf(id: bigint): Layer {
    return this.#layer(id);
  }

  #receipt(native: NativeReceipt): EditReceipt {
    const [target, changes, loaded, failures, unavailable] = native.record() as [bigint, Row, bigint[], Row[], Row[]];
    const receipt = new EditReceipt(
      this,
      this.#layer(target),
      decodeChanges(changes),
      loaded.map((id) => this.#layer(id)),
      failures.map(([asset, anchor, code, message]) => this.#failure(asset, anchor, code, message)),
      unavailable.map((row) => this.#unavailable(row)),
    );
    receiptNatives.set(receipt, native);
    return receipt;
  }

  /** Drains the native change cursor and calls the listeners, in order. */
  #notify(): void {
    if (this.#dispatching || this.#native === null) return;
    this.#dispatching = true;
    try {
      for (;;) {
        const record = this.#call("onChange", (native) => native.takeChanges(this.#listeners.length > 0)) as
          | Row[]
          | null;
        const batches =
          record === null ? [{ created: [], removed: [], resynced: ["/"], changedInfoOnly: [] }] : record.map(decodeChanges);
        if (batches.length === 0 || this.#listeners.length === 0) return;
        for (const changes of batches) {
          for (const listener of [...this.#listeners]) {
            try {
              listener(changes);
            } catch (error) {
              report(new LayerstackError("listener.failed", "onChange", `a change listener threw: ${String(error)}`, { cause: error }));
            }
          }
        }
        if (this.#native === null) return;
      }
    } finally {
      this.#dispatching = false;
    }
  }

  #call<T>(operation: string, f: (native: NativeStage) => T): T {
    const native = this.#native;
    if (native === null) {
      throw new LayerstackError("stage.disposed", operation, "the stage was disposed");
    }
    return this.#ls.call(operation, () => f(native));
  }

  #own(operation: string, layer: Layer): void {
    if (!(layer instanceof Layer) || owners.get(layer) !== this) {
      throw new LayerstackError("layer.affinity", operation, "the layer belongs to another stage");
    }
  }

  #layer(id: bigint): Layer {
    const known = this.#layers.get(id);
    if (known !== undefined) return known;
    this.layers();
    const found = this.#layers.get(id);
    if (found === undefined) {
      throw new LayerstackError("layer.unknown", "layers", `the stage has no layer ${id}`);
    }
    return found;
  }

  #remember(id: bigint, identifier: string | null, format: string, readOnly: boolean): Layer {
    let layer = this.#layers.get(id);
    if (layer === undefined) {
      layer = new Layer(this, id, identifier, format, readOnly);
      this.#layers.set(id, layer);
    }
    return layer;
  }

  #unavailable([identifier, assetPath, anchor, usedBy, prims]: Row): UnavailableAsset {
    return {
      identifier: identifier as string,
      assetPath: assetPath as string,
      anchor: this.#layer(anchor as bigint),
      usedBy: usedBy as AssetUse,
      prims: prims as string[],
    };
  }

  #failure(assetPath: unknown, anchor: unknown, code: unknown, message: unknown): AssetFailure {
    return {
      assetPath: assetPath as string,
      anchor: anchor === null ? null : this.#layer(anchor as bigint),
      code: code as string,
      message: message as string,
    };
  }
}

/**
 * A composed prim, as read when it was returned: its fields do not
 * follow later changes, so read it again after one. Its methods query the
 * stage now.
 */
export class Prim {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its path. */
  readonly path: string;
  /** Its name, the last element of its path (`""` for the pseudo-root). */
  readonly name: string;
  /** Its type name (`Sphere`), `null` when typeless. */
  readonly typeName: string | null;
  /** Its composed specifier. */
  readonly specifier: "def" | "over" | "class";
  /** Whether it is active. */
  readonly active: boolean;
  /** Whether it and its ancestors are defined (`def` or `class`). */
  readonly defined: boolean;
  /** Whether it or an ancestor is a `class`. */
  readonly abstract: boolean;
  /** Whether it is an instance. */
  readonly instance: boolean;
  /** Whether it is a proxy for a prim inside an instance's prototype. */
  readonly instanceProxy: boolean;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    const [typeName, specifier, active, defined, abstract, instance, instanceProxy] = row;
    this.stage = stage;
    this.path = path;
    this.name = path === "/" ? "" : path.slice(path.lastIndexOf("/") + 1);
    this.typeName = typeName as string | null;
    this.specifier = specifier as Prim["specifier"];
    this.active = active as boolean;
    this.defined = defined as boolean;
    this.abstract = abstract as boolean;
    this.instance = instance as boolean;
    this.instanceProxy = instanceProxy as boolean;
    Object.freeze(this);
  }

  /** The paths of its children ({@link Stage.children}). */
  children(): string[] {
    return this.stage.children(this.path);
  }

  /** The names of its attributes, schema-defined and authored, in property order. */
  attributeNames(): string[] {
    return this.stage.native("attributeNames", (native) => native.attributeNames(this.path));
  }

  /** Its attribute `name` (`size`, `inputs:diffuseColor`), or `null`. */
  attribute(name: string): Attribute | null {
    return this.stage.attribute(`${this.path}.${name}`);
  }

  /** Its variant sets, their selections and variant names. Select one with {@link Stage.setVariantSelection}. */
  variantSets(): VariantSet[] {
    const rows = this.stage.native("variantSets", (native) => native.variantSets(this.path)) as Row[];
    return rows.map(([name, selection, variants]) => ({
      name: name as string,
      selection: selection as string | null,
      variants: variants as string[],
    }));
  }

  /** Its composed metadata field `key` (`kind`, `documentation`, `apiSchemas`), or `null` when nothing authors it. */
  metadata(key: string): Value | null {
    return readMetadata(this.stage, this.path, key);
  }
}

/**
 * A composed attribute, as read when it was returned; {@link Attribute.get}
 * and {@link Attribute.explain} query the stage now.
 *
 * `time` is a time code, interpolated linearly between samples; without
 * one, queries read the default time.
 */
export class Attribute {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its property path (`/World/Rock.size`). */
  readonly path: string;
  /** Its name (`size`). */
  readonly name: string;
  /** Its value type name (`double`, `point3f[]`); `null` when nothing declares one. */
  readonly typeName: string | null;
  /** Whether its value may vary over time. */
  readonly variability: "varying" | "uniform";
  /** Whether no schema defines it. */
  readonly custom: boolean;
  /** Whether any layer authors an opinion for it. */
  readonly authored: boolean;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    const [typeName, variability, custom, authored] = row;
    this.stage = stage;
    this.path = path;
    this.name = path.slice(path.lastIndexOf(".") + 1);
    this.typeName = typeName as string | null;
    this.variability = variability as Attribute["variability"];
    this.custom = custom as boolean;
    this.authored = authored as boolean;
    Object.freeze(this);
  }

  /**
   * Its composed value at `time`, or `null` when it resolves none. A
   * numeric array is an {@link ArrayHandle} on the stage's buffer, not a
   * copy; release it when done.
   */
  get(time?: number): Value | null {
    const record = this.stage.native("get", (native) => native.get(this.path, time));
    return decodeValue(this.stage.layerstack, record);
  }

  /** Its value as array data (an {@link ArrayHandle} for numbers), or `null` when it is not an array. */
  array(time?: number): ArrayData | null {
    const value = this.get(time);
    return value?.kind === "array" ? value.array : null;
  }

  /** Why it has its value at `time`, or `null` when it does not exist on the prim. */
  explain(time?: number): Explanation | null {
    const record = this.stage.native("explain", (native) => native.explain(this.path, time)) as Row | null;
    if (record === null) return null;
    const [value, source, lower, upper, opinions] = record;
    return {
      value: decodeValue(this.stage.layerstack, value),
      source: source as ValueSource,
      samples: lower === null ? null : { lower: lower as number, upper: upper as number },
      opinions: (opinions as Row[]).map(
        ([layer, specPath, arc, role, detail, offset, scale]): Opinion => ({
          layer: this.stage.layerOf(layer as bigint),
          specPath: specPath as string,
          arc: arc as ArcKind | null,
          role: role as Opinion["role"],
          contribution: role === "contributed" ? (detail as Opinion["contribution"]) : null,
          cause: role === "ignored" ? (detail as Opinion["cause"]) : null,
          layerOffset: { offset: offset as number, scale: scale as number },
        }),
      ),
    };
  }

  /** Its composed time sample times, ascending. */
  timeSamples(): number[] {
    return Array.from(this.stage.native("timeSamples", (native) => native.timeSamples(this.path)));
  }

  /** Its composed metadata field `key` (`hidden`, `documentation`), or `null` when nothing authors it. */
  metadata(key: string): Value | null {
    return readMetadata(this.stage, this.path, key);
  }
}

/** A composed relationship, as read when it was returned; {@link Relationship.targets} queries the stage now. */
export class Relationship {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its property path (`/World.lookAt`). */
  readonly path: string;
  /** Its name (`lookAt`). */
  readonly name: string;
  /** Whether an opinion declares it `custom`: one no schema of its prim is expected to define. */
  readonly custom: boolean;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    this.stage = stage;
    this.path = path;
    this.name = path.slice(path.lastIndexOf(".") + 1);
    this.custom = row[0] as boolean;
    Object.freeze(this);
  }

  /** Its composed targets, in order: prim and property paths. */
  targets(): string[] {
    const row = this.stage.native("relationship", (native) => native.relationship(this.path)) as Row | null;
    return row === null ? [] : [...(row[1] as string[])];
  }

  /** Its composed metadata field `key`, or `null`. */
  metadata(key: string): Value | null {
    return readMetadata(this.stage, this.path, key);
  }
}

/**
 * What {@link Stage.apply} (or {@link Stage.undo}) did, holding the
 * native inverse that undoes it. The app keeps the receipts it wants to
 * undo; the stage keeps no history.
 *
 * Owns native memory: {@link EditReceipt.release} it (or declare it with
 * `using`) when it will not be undone. Undoing releases it.
 */
export class EditReceipt implements Disposable {
  /** The stage it belongs to. */
  readonly stage: Stage;
  /** The layer the edits went to. */
  readonly target: Layer;
  /** The composed changes of the edits themselves (loading the assets they ask for is heard separately). */
  readonly changes: Changes;
  /** The layers read for the edits: assets the host had supplied already. */
  readonly loaded: readonly Layer[];
  /** The assets that failed to read for the edits. */
  readonly failures: readonly AssetFailure[];
  /** The assets the edits ask for that the host has not supplied: run {@link Stage.resolve}. */
  readonly unavailable: readonly UnavailableAsset[];

  /** @internal Use {@link Stage.apply}. */
  constructor(
    stage: Stage,
    target: Layer,
    changes: Changes,
    loaded: readonly Layer[],
    failures: readonly AssetFailure[],
    unavailable: readonly UnavailableAsset[],
  ) {
    this.stage = stage;
    this.target = target;
    this.changes = changes;
    this.loaded = loaded;
    this.failures = failures;
    this.unavailable = unavailable;
  }

  /** Whether it was undone or released: it can no longer undo. */
  get released(): boolean {
    return !receiptNatives.has(this);
  }

  /** Undoes the edits ({@link Stage.undo}) and returns the receipt that redoes them. */
  undo(): EditReceipt {
    return this.stage.undo(this);
  }

  /** Releases the native inverse; it can no longer undo. Idempotent. */
  release(): void {
    const native = receiptNatives.get(this);
    receiptNatives.delete(this);
    native?.free();
  }

  /** Releases it, for `using`. */
  [Symbol.dispose](): void {
    this.release();
  }
}

function readMetadata(stage: Stage, path: string, key: string): Value | null {
  const record = stage.native("metadata", (native) => native.metadata(path, key));
  return decodeValue(stage.layerstack, record);
}

function decodeChanges(row: unknown): Changes {
  const [created, removed, resynced, changedInfoOnly] = row as [string[], string[], string[], string[]];
  return { created, removed, resynced, changedInfoOnly };
}

/** Reports an error that must not interrupt the caller, as the host reports uncaught errors. */
/** A limit of `OpenOptions.limits` as the module takes it: `undefined` for its default, `Infinity` for none. */
function limit(value: number | null | undefined, name: string): number | undefined {
  if (value === undefined) return undefined;
  if (value === null) return Infinity;
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new LayerstackError("argument.invalid", "open", `limits.${name} must be a non-negative integer or null`);
  }
  return value;
}

function report(error: LayerstackError): void {
  const host = globalThis as { reportError?: (error: unknown) => void };
  if (typeof host.reportError === "function") host.reportError(error);
  else console.error(error);
}

function decodeValue(layerstack: Layerstack, record: unknown): Value | null {
  if (record === null || record === undefined) return null;
  const [type, tag, ...rest] = record as [string, string, ...unknown[]];
  switch (tag) {
    case "s":
      return { kind: "scalar", type, value: rest[0] as boolean | number | bigint | string };
    case "t":
      return { kind: "tuple", type, value: rest[0] as number[] };
    case "a": {
      const [scalar, components, length, elements] = rest as [Scalar, number, number, string[] | NativeArrayHandle];
      if (Array.isArray(elements)) {
        const array = new ArrayCopy(type, scalar, components, length, elements);
        layerstack.count("out", array.byteLength);
        return { kind: "array", type, array };
      }
      return { kind: "array", type, array: new ArrayHandle(layerstack, elements) };
    }
    case "d":
      return {
        kind: "dictionary",
        type,
        value: new Map((rest[0] as [string, unknown][]).map(([key, value]) => [key, decodeValue(layerstack, value)])),
      };
    case "l":
      return { kind: "list", type, value: (rest[0] as unknown[]).map((item) => decodeValue(layerstack, item)) };
    default:
      return { kind: "opaque", type };
  }
}
