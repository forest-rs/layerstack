// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { ArrayCopy, ArrayHandle } from "./arrays.js";
import type { NativeArrayHandle } from "./arrays.js";
import { lowerEdits } from "./edits.js";
import type { NativeEditBatch } from "./edits.js";
import { LayerstackError } from "./errors.js";
import type { ErrorCode, HostErrorCode } from "./errors.js";
import type {
  ArcKind,
  ArrayData,
  EditTargetKind,
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
  PropertyChange,
  PackageResult,
  ResolveOptions,
  ResolveResult,
  SaveResult,
  Scalar,
  SkippedLayer,
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
export const ABI = 4;

/** The native stage the generated glue exports (`layerstack_wasm::Stage`). Borrowed only by same-module extensions. */
export interface NativeStage extends Disposable {
  free(): void;
  unavailable(): unknown;
  supply(identifier: string, bytes: Uint8Array): void;
  suppliedBytes(): bigint;
  confirmAbsent(identifier: string): void;
  loadSupplied(): unknown;
  layers(): unknown;
  layerDirty(id: bigint): boolean;
  setLayerMuted(id: bigint, muted: boolean): boolean;
  mutedLayers(): unknown;
  status(): unknown;
  prim(path: string): unknown;
  children(path: string): string[];
  attributeNames(path: string): string[];
  relationshipNames(path: string): string[];
  variantSets(path: string): unknown;
  attribute(path: string): unknown;
  get(path: string, time?: number | null): unknown;
  timeSamples(path: string): Float64Array;
  explain(path: string, time?: number | null): unknown;
  metadata(path: string, key: string): unknown;
  relationship(path: string): unknown;
  apply(target: bigint, batch: NativeEditBatch): NativeReceipt;
  applyTarget(...args: [...NativeTarget, batch: NativeEditBatch]): NativeReceipt;
  undo(receipt: NativeReceipt): NativeReceipt;
  moveObject(target: bigint, from: string, to: string): NativeReceipt;
  deleteObject(target: bigint, path: string): NativeReceipt;
  editTargetForLayer(layer: bigint): unknown;
  hasLocalLayer(layer: bigint): boolean;
  variantEditTarget(path: string, set: string, variant: string, layer: bigint): unknown;
  compositionArcs(path: string): unknown;
  mapEditTarget(...args: [...NativeTarget, path: string]): string | undefined;
  editTargetComposed(...args: NativeTarget): boolean;
  takeChanges(render: boolean): unknown;
  save(layer?: bigint | null): unknown;
  exportLayer(layer: bigint, format: string): Uint8Array;
  exportFlattened(format: string): unknown;
  exportPackage(): unknown;
}

/**
 * An edit target as the native module takes it back: the values of its
 * record, `[kind, layer, prim, site, arc, mapsOutside, offset, scale]`.
 */
export type NativeTarget = [
  kind: string,
  layer: bigint,
  prim: string | null,
  site: string | null,
  arc: string | null,
  mapsOutside: boolean,
  offset: number,
  scale: number,
];

/** The native receipt the generated glue exports (`layerstack_wasm::EditReceipt`). Not for app use. */
export interface NativeReceipt extends Disposable {
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
      provenance: boolean,
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
    if (options.provenance !== undefined && typeof options.provenance !== "boolean") {
      throw new LayerstackError("argument.invalid", "open", "provenance must be a boolean");
    }
    const provenance = options.provenance ?? false;
    const recover = options.importPolicy === "recover";
    const session = editLayer === "session";
    const limits = options.limits;
    const nodes = limit(limits?.primIndexNodes, "primIndexNodes");
    const perIndex = limit(limits?.nodesPerPrimIndex, "nodesPerPrimIndex");
    const depth = limit(limits?.composedDepth, "composedDepth");
    const native = this.call("open", () =>
      this.#module.Stage.open(root.identifier, root.bytes, recover, session, nodes, perIndex, depth, provenance),
    );
    this.#bytesIn += root.bytes.byteLength;
    return new Stage(this, native, provenance);
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
        ErrorCode | HostErrorCode,
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
 * Where edits are written, and how their stage paths and times map there:
 * OpenUSD's `UsdEditTarget`. Minted by the stage, never constructed:
 * {@link Opinion.editTarget} (where an opinion lives),
 * {@link CompositionArc.editTarget} (any arc of a prim, opinion or not),
 * {@link Prim.variantEditTarget} (inside a variant) and
 * {@link Stage.editTargetForLayer} (a layer of the stage's own stack).
 * Pass one as the `target` of {@link Stage.apply}.
 *
 * Edits through a target still take stage paths and stage times: through
 * `/World/Robot`'s reference to `robot.usda`'s `/Robot`, a value set on
 * `/World/Robot.height` is written to `/Robot.height` in `robot.usda`, and
 * every prim that references the asset changes. A path the target does not
 * map is rejected (`edit.rejected.unmappable`).
 *
 * A target is a value, identified by its mapping as `UsdEditTarget` is:
 * targets with the same layer, prim, site, arc and offset are the same
 * object (`===`), whichever way they were minted. It survives edits and
 * unrelated recomposition: each use finds the arc again, and one no arc
 * maps any more (the reference was removed, the prim renamed) fails with
 * `target.stale`.
 */
export class EditTarget {
  /**
   * `"layer"` for a layer of the stage's layer stack, `"variant"` inside a
   * variant on its prim with no layer offset (OpenUSD's
   * `ForLocalDirectVariant`), `"arc"` through any other arc, a variant's
   * node in a sublayer with an offset included.
   */
  readonly kind: EditTargetKind;
  /** The layer written. A package member is read-only ({@link Layer.readOnly}): edits through it fail with `layer.read_only`. */
  readonly layer: Layer;
  /** The stage prim whose namespace is mapped (`/World/Robot`); `null` for a `"layer"` target, which maps every path to itself. */
  readonly prim: string | null;
  /** The spec path `prim` maps to in `layer` (`/Robot`, `/World/Robot{finish=painted}`); `null` for a `"layer"` target. */
  readonly site: string | null;
  /** The arc it goes through; `null` for a `"layer"` target. */
  readonly arc: ArcKind | null;
  /**
   * Whether paths outside `prim` map to themselves: true through variants,
   * inherits and specializes; false across a reference or payload, which
   * maps only the referenced prim's namespace.
   */
  readonly mapsOutsidePrim: boolean;
  /** The offset from `layer`'s times to stage times: a stage time `t` is written at layer time `(t - offset) / scale`. */
  readonly layerOffset: { readonly offset: number; readonly scale: number };
  readonly #native: NativeTarget;

  /** @internal Minted by the stage. */
  constructor(stage: Stage, layer: Layer, native: NativeTarget) {
    const [kind, , prim, site, arc, mapsOutside, offset, scale] = native;
    this.kind = kind as EditTargetKind;
    this.layer = layer;
    this.prim = prim;
    this.site = site;
    this.arc = arc as ArcKind | null;
    this.mapsOutsidePrim = mapsOutside;
    this.layerOffset = Object.freeze({ offset, scale });
    this.#native = native;
    owners.set(this, stage);
    Object.freeze(this);
  }

  /** The stage that minted it. */
  get stage(): Stage {
    return owners.get(this) as Stage;
  }

  /**
   * The spec path the stage prim or property path `path` is written at
   * in {@link EditTarget.layer} (`/Robot.height` for `/World/Robot.height`
   * through the robot's reference), or `null` when the target does not map
   * it. OpenUSD's `UsdEditTarget::MapToSpecPath`.
   *
   * Throws `path.invalid`, `layer.muted`, and `target.stale` when no arc
   * maps the target any more.
   */
  mapPath(path: string): string | null {
    if (typeof path !== "string") {
      throw new LayerstackError("argument.invalid", "mapPath", "expected a path string");
    }
    return this.stage.native("mapPath", (native) => native.mapEditTarget(...this.#native, path)) ?? null;
  }

  /** The time in {@link EditTarget.layer} a stage time code is written at: `(time - offset) / scale`. */
  mapTime(time: number): number {
    return (time - this.layerOffset.offset) / this.layerOffset.scale;
  }

  /**
   * Whether the stage composes what this target writes now: false for a
   * variant that is not selected, an arc that was removed, or a muted
   * layer. Edits through a target that is not composed are kept, and
   * change no composed value until it is.
   */
  composed(): boolean {
    return this.stage.native("composed", (native) => native.editTargetComposed(...this.#native));
  }

  /** @internal The values the native module takes it back as. */
  get nativeTarget(): NativeTarget {
    return this.#native;
  }
}

/**
 * One arc of a prim's composition, strongest first, as
 * {@link Prim.compositionArcs} lists them: OpenUSD's
 * `UsdPrimCompositionQueryArc`. Read when it was returned; take its
 * {@link CompositionArc.editTarget} to author through it.
 */
export class CompositionArc {
  /** The arc kind: `local` for the prim's own opinions, else the arc that reaches the node. */
  readonly arc: ArcKind;
  /** The spec path the arc reads in its layer stack (`/Robot`, `/World/Robot{finish=painted}`). */
  readonly site: string;
  /** The root layer of the layer stack the arc reads. */
  readonly layerStack: Layer;
  /** The layers of that stack, strongest first: the layers {@link CompositionArc.editTarget} can write. */
  readonly layers: readonly Layer[];
  /** The offset from the stack's root layer's times to stage times. */
  readonly layerOffset: { readonly offset: number; readonly scale: number };
  /** Whether the arc is implied: a class arc authored inside a reference, mapped to the referencing site. */
  readonly implied: boolean;
  /** The index, in the list it came in, of the arc it is authored beneath; `null` for the `local` arc. */
  readonly parent: number | null;
  readonly #prim: string;
  readonly #editable: boolean;
  readonly #mapsOutside: boolean;
  /** Per layer: the offset and the kind of a target through the arc in it. */
  readonly #targets: readonly (readonly [number, number, EditTargetKind])[];

  /** @internal Use {@link Prim.compositionArcs}. */
  constructor(stage: Stage, prim: string, row: Row) {
    const [arc, site, layerStack, layers, implied, parent, kind, mapsOutside] = row;
    const entries = layers as [bigint, number, number, EditTargetKind | null][];
    this.arc = arc as ArcKind;
    this.site = site as string;
    this.layerStack = stage.layer(layerStack as bigint);
    this.layers = Object.freeze(entries.map(([id]) => stage.layer(id)));
    const [, offset, scale] = entries.find(([id]) => id === layerStack) ?? [0n, 0, 1];
    this.layerOffset = Object.freeze({ offset, scale });
    this.implied = implied as boolean;
    this.parent = parent as number | null;
    this.#prim = prim;
    this.#editable = kind !== null;
    this.#mapsOutside = mapsOutside as boolean;
    this.#targets = entries.map(([, offset, scale, layerKind]) => [offset, scale, layerKind ?? "arc"] as const);
    owners.set(this, stage);
    Object.freeze(this);
  }

  /**
   * The target that writes through this arc in `layer`, one of
   * {@link CompositionArc.layers} (the stack's root layer by default), as
   * OpenUSD's `UsdEditTarget(layer, node)`: whether or not the prim has an
   * opinion there yet. Times map through the offset `layer` is read at, a
   * sublayer's included; for the `local` arc this is
   * {@link Stage.editTargetForLayer}. A selected variant's arc in a layer
   * read with no offset gives the same target as
   * {@link Prim.variantEditTarget}; in a sublayer with an offset it gives
   * an `"arc"` target that maps times through it, where
   * `variantEditTarget` does not.
   *
   * Throws `target.not_in_arc` for a layer outside the arc's stack, and
   * `target.relocate_source` for a `relocates` arc, whose specs never
   * contribute: edit a relocated prim through its other arcs.
   */
  editTarget(layer: Layer = this.layerStack): EditTarget {
    const stage = owners.get(this) as Stage;
    if (!this.#editable) {
      throw new LayerstackError(
        "target.relocate_source",
        "editTarget",
        "a relocates arc reads the relocation source, whose specs never contribute: edit the prim through its other arcs",
        { path: this.#prim },
      );
    }
    const index = this.layers.indexOf(layer);
    const through = this.#targets[index];
    if (through === undefined) {
      throw new LayerstackError(
        "target.not_in_arc",
        "editTarget",
        `the layer is not in the arc's layer stack: pick one of arc.layers`,
        { path: this.#prim, identifier: layer instanceof Layer ? (layer.identifier ?? undefined) : undefined },
      );
    }
    const [offset, scale, kind] = through;
    const local = kind === "layer";
    return stage.mintTarget([
      kind,
      layer.id,
      local ? null : this.#prim,
      local ? null : this.site,
      local ? null : this.arc,
      this.#mapsOutside,
      offset,
      scale,
    ]);
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
  /** The targets minted, by their values, so equal targets are one object. */
  readonly #targets = new Map<string, WeakRef<EditTarget>>();
  readonly #forget = new FinalizationRegistry<string>((key) => {
    if (this.#targets.get(key)?.deref() === undefined) this.#targets.delete(key);
  });
  #root: Layer | null = null;
  #editLayer: Layer | null | undefined = undefined;
  readonly #provenance: boolean;
  readonly #listeners: ChangeListener[] = [];
  #dispatching = false;

  /** @internal Use {@link Layerstack.open}. */
  constructor(layerstack: Layerstack, native: NativeStage, provenance: boolean) {
    this.#ls = layerstack;
    this.#native = native;
    this.#provenance = provenance;
  }

  /** The instance that opened this stage. */
  get layerstack(): Layerstack {
    return this.#ls;
  }

  /** Whether the stage was opened with {@link OpenOptions.provenance}: whether its native resolved values name their sources. */
  get provenance(): boolean {
    return this.#provenance;
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

  /**
   * The layer with `id`, the same `Layer` every time: for an id that native
   * code returns, such as the source layer of a resolved value on a stage
   * opened with {@link OpenOptions.provenance}. Any layer the stage holds
   * has one, a muted layer and a layer reached only through an arc
   * included ({@link Stage.layers}).
   *
   * Throws `argument.invalid` when `id` is not a `bigint`, and
   * `layer.unknown` when the stage holds no layer with it (an asset it
   * has not read yet has none).
   */
  layer(id: bigint): Layer {
    if (typeof id !== "bigint") {
      throw new LayerstackError("argument.invalid", "layer", "expected a bigint layer id");
    }
    this.#call("layer", () => undefined);
    return this.#layer(id, "layer");
  }

  /** Whether `layer` changed since it was read or last saved. Throws `layer.affinity` for another stage's layer. */
  isDirty(layer: Layer): boolean {
    this.#own("isDirty", layer);
    return this.#call("isDirty", (native) => native.layerDirty(layer.id));
  }

  /**
   * The edit target of `layer`, a layer of the stage's own layer stack
   * (the session layer, the root and their sublayers), as OpenUSD's
   * `UsdStage::GetEditTargetForLocalLayer` makes it: every path maps to
   * itself, and stage times map through the offset the stack reads the
   * layer at, so a sample set at stage time 15 in a sublayer authored with
   * `(offset = 10)` lands at its time 5. A bare `Layer` passed as a target
   * means this target.
   *
   * Throws `layer.affinity`, `layer.muted`, and `layer.outside_stack` for
   * a layer the stage reaches only through an arc: edit that through an
   * {@link Opinion.editTarget} or a {@link CompositionArc.editTarget}.
   */
  editTargetForLayer(layer: Layer): EditTarget {
    this.#own("editTargetForLayer", layer);
    const record = this.#call("editTargetForLayer", (native) => native.editTargetForLayer(layer.id)) as NativeTarget;
    return this.mintTarget(record);
  }

  /**
   * Whether `layer` is in the stage's own layer stack, unmuted, as
   * OpenUSD's `UsdStage::HasLocalLayer` says: whether it can be passed as
   * a bare `target`. Throws `layer.affinity` for another stage's layer.
   */
  hasLocalLayer(layer: Layer): boolean {
    this.#own("hasLocalLayer", layer);
    return this.#call("hasLocalLayer", (native) => native.hasLocalLayer(layer.id));
  }

  /**
   * Mutes `layer`, as OpenUSD's `UsdStage::MuteLayer` does: the stage
   * recomposes without its opinions and its sublayers, wherever it reaches
   * it (a sublayer, the session layer, a reference's or payload's layer),
   * and change listeners hear it before this returns
   * ({@link Changes.mutedLayers}). Returns whether its muting changed:
   * `false` for a layer muted already.
   *
   * Muting is the stage's state, not an edit: it makes no receipt, and
   * undo neither undoes it nor is blocked by it. The layer keeps its
   * content and its unsaved edits, and composes again when unmuted.
   * While it is muted, edits that target it fail with `layer.muted` and
   * {@link Stage.save} without a layer skips it (`save(layer)` writes it).
   * While any layer is muted, {@link Stage.move} and {@link Stage.delete}
   * fail with `namespace.unsupported`: they could not repair the paths a
   * muted layer names. Muting a layer the stage holds but does not use is
   * not an error, as in OpenUSD; it changes no prim.
   *
   * Throws `layer.affinity` for another stage's layer and
   * `mute.root_layer` for {@link Stage.root}, which cannot be muted.
   */
  muteLayer(layer: Layer): boolean {
    return this.#setMuted("muteLayer", layer, true);
  }

  /**
   * Unmutes `layer`, as OpenUSD's `UsdStage::UnmuteLayer` does: the stage
   * recomposes with it again, and listeners hear it
   * ({@link Changes.unmutedLayers}). Returns whether its muting changed:
   * `false` for a layer not muted. Assets its content asks for that the
   * stage has not read are unavailable afterwards:
   * `await stage.resolve(source)`.
   *
   * Throws `layer.affinity` for another stage's layer.
   */
  unmuteLayer(layer: Layer): boolean {
    return this.#setMuted("unmuteLayer", layer, false);
  }

  /** The layers the stage mutes, by id, as OpenUSD's `UsdStage::GetMutedLayers` lists them. Never the root. */
  mutedLayers(): Layer[] {
    const ids = this.#call("mutedLayers", (native) => native.mutedLayers()) as bigint[];
    return ids.map((id) => this.#layer(id));
  }

  /** Whether the stage mutes `layer` (OpenUSD's `UsdStage::IsLayerMuted`). Throws `layer.affinity` for another stage's layer. */
  isMuted(layer: Layer): boolean {
    this.#own("isMuted", layer);
    return this.mutedLayers().includes(layer);
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
   * Applies `edits` through `target`, the {@link Stage.editLayer} by
   * default, atomically: every edit applies, or none does and this throws. Assets the edits ask for that the stage
   * has not read (a new reference, a variant's payload) are unavailable
   * afterwards: the receipt lists them, and {@link Stage.resolve} fetches
   * them. Change listeners run before this returns.
   *
   * `target` is a layer of the stage's own layer stack (its
   * {@link Stage.editTargetForLayer}) or an {@link EditTarget}: an
   * opinion's, to edit where it lives, such as inside a referenced asset,
   * or a variant's. The edits always name stage paths and stage times,
   * which the target maps into its layer.
   *
   * Throws `layer.affinity` (another stage's layer), `target.affinity`
   * (another stage's target), `layer.read_only` (a USDZ member, or no
   * `target` and no edit layer: open with `editLayer: "session"`),
   * `layer.muted`, `layer.outside_stack` (a layer reached only through an
   * arc: pass an opinion's {@link Opinion.editTarget} instead),
   * `target.stale` (no arc maps the target any more),
   * `argument.invalid` or `value.*` for a malformed edit, and `edit.*`
   * when the edits are rejected (`edit.rejected.type_mismatch`,
   * `edit.rejected.unmappable` for a path the target does not map, ...);
   * the error's `edit` is the index of the edit at fault.
   */
  apply(edits: readonly Edit[], target?: Layer | EditTarget): EditReceipt {
    const through = this.#target("apply", target);
    const batch = this.#ls.batch();
    try {
      lowerEdits(batch, edits, "apply", this.#ls);
      const native = this.#call("apply", (stage) => stage.applyTarget(...through, batch));
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
   * Throws as {@link Stage.apply}; `layer.read_only` when there is no
   * `target` and no edit layer.
   */
  setVariantSelection(prim: Prim | string, set: string, variant: string | null, target?: Layer | EditTarget): EditReceipt {
    const path = typeof prim === "string" ? prim : prim.path;
    return this.apply([{ type: "setVariantSelection", path, set, variant }], target);
  }

  /**
   * Moves the prim or property at `path` to `to`, as OpenUSD's
   * `UsdNamespaceEditor` moves it: a rename when only the last name
   * changes (`/World/Rock` to `/World/Boulder`), a reparent otherwise.
   * Every opinion of the layer stack that `target` (the
   * {@link Stage.editLayer} by default) is in moves together, the prim's
   * variant sets and the specs inside its variants with it, and the
   * paths that name the moved object (relationship targets, connections,
   * internal arcs, path expressions) are repaired; so one move may edit
   * several layers of the stack. Atomic, with change listeners run before
   * it returns; the receipt undoes it.
   *
   * `target` must be a layer of the stage's layer stack (or its
   * `"layer"` {@link EditTarget}): as OpenUSD's `UsdNamespaceEditor`, a
   * move does not go through an arc or into a variant, and such a target
   * fails with `namespace.unmappable`.
   *
   * Throws `path.invalid`, the target errors of {@link Stage.apply}, and
   * `namespace.*` when the move cannot be made, nothing changed:
   * `namespace.collision` (the destination is occupied),
   * `namespace.missing_parent`, `namespace.not_found`,
   * `namespace.instance_proxy` (inside an instance),
   * `namespace.unmappable`, ...
   */
  move(path: string, to: string, target?: Layer | EditTarget): EditReceipt {
    if (typeof path !== "string" || typeof to !== "string") {
      throw new LayerstackError("argument.invalid", "move", "expected (path: string, to: string)");
    }
    const layer = this.#namespaceLayer("move", path, target);
    const native = this.#call("move", (stage) => stage.moveObject(layer, path, to));
    const receipt = this.#receipt(native);
    this.#notify();
    return receipt;
  }

  /**
   * Deletes the prim or property at `path` from the composed stage, as
   * OpenUSD's `UsdNamespaceEditor` deletes it: every opinion of the layer
   * stack `target` is in (the {@link Stage.editLayer} by default), and the
   * paths and arcs that name it where a composed prim depends on them.
   * A `removePrim` edit, by contrast, removes one spec from one layer.
   * Atomic; the receipt undoes it.
   *
   * Throws as {@link Stage.move}; `target` must be a layer of the stage's
   * layer stack, as for a move.
   */
  delete(path: string, target?: Layer | EditTarget): EditReceipt {
    if (typeof path !== "string") {
      throw new LayerstackError("argument.invalid", "delete", "expected (path: string)");
    }
    const layer = this.#namespaceLayer("delete", path, target);
    const native = this.#call("delete", (stage) => stage.deleteObject(layer, path));
    const receipt = this.#receipt(native);
    this.#notify();
    return receipt;
  }

  /**
   * Calls `listener` with each composed change, synchronously and in
   * order, after every call that changes the stage: {@link Stage.apply},
   * {@link Stage.undo}, {@link Stage.muteLayer}, {@link Stage.unmuteLayer},
   * {@link Stage.loadSupplied} (and so
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
   * fails with `io.read_only` and stays dirty. A layer edited through an
   * arc is saved like any other while the stage uses it; a dirty layer the
   * stage mutes or no longer composes is left dirty and listed in
   * `skipped`.
   *
   * Throws `layer.affinity` or `layer.unknown` for a `layer` not of this stage.
   */
  save(layer?: Layer): SaveResult {
    if (layer !== undefined) this.#own("save", layer);
    const [written, failures, anonymous, skipped] = this.#call("save", (native) => native.save(layer?.id ?? null)) as [
      Row[],
      Row[],
      bigint[],
      Row[],
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
      skipped: skipped.map(([id, reason]) => ({ layer: this.#layer(id as bigint), reason: reason as SkippedLayer["reason"] })),
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

  /**
   * Borrows this stage for a synchronous extension linked into the same native
   * module, such as a Rust export taking `&layerstack_wasm::Stage`. `operation`
   * names that extension in disposed-stage and native-error diagnostics: a
   * `layerstack_wasm::Error` it returns is thrown as a {@link LayerstackError}
   * with that operation and the error's own code ({@link HostErrorCode}). A
   * `LayerId` it returns is {@link Stage.layer}'s `id`.
   *
   * The callback must not retain, free or consume the borrowed native stage.
   * Return an extension result, never a promise or thenable. A returned
   * thenable is rejected with `argument.invalid`; this cannot cancel an async
   * continuation that the callback already started.
   *
   * Native changes use the ordinary synchronous {@link onChange} ordering;
   * extensions called by a listener queue their changes after the current
   * batch. Change dispatch can call the module again before returning: return
   * owned data or a handle, not a borrowed view into Wasm memory.
   *
   * Changes are drained even after a callback failure. The callback is not a
   * transaction: edits it committed before failing remain committed. A
   * callback failure remains the primary error if draining changes also fails.
   * Native traps poison the module as they do for ordinary stage calls.
   *
   * Native edits are heard once the live stage recomposes them: at once for
   * a transaction (`StageDocument::apply`) or a load, and for a queued control
   * (layer muting) or a direct store edit only after `StageDocument::synchronize`,
   * which the extension calls before returning; muting is heard with the
   * layers it muted and unmuted ({@link Changes.mutedLayers}). Edited
   * layers become dirty ({@link Stage.isDirty}, {@link Stage.save}). Native
   * edits make no {@link EditReceipt}: undo covers only what {@link Stage.apply},
   * {@link Stage.move} and {@link Stage.delete} did. A receipt whose inverse
   * covers a spec a native edit changed since (a value both set, or a prim
   * the receipt defined that native code then gave a child) fails with
   * `edit.stale` and changes nothing; it undoes once native code puts the
   * spec back as the receipt left it (removes that child, restores that
   * value). Content derived natively therefore belongs in a layer the user's
   * edits do not target, such as the session layer while they target the
   * root (`layerstack_wasm::Stage::document_mut`).
   */
  withNative<T>(operation: string, f: (native: NativeStage) => T): T {
    if (typeof operation !== "string" || operation.length === 0 || typeof f !== "function") {
      throw new LayerstackError("argument.invalid", "withNative", "expected an operation name and a synchronous callback");
    }
    let result: T;
    try {
      result = this.#call(operation, (native) => {
        const value = f(native);
        if (
          value !== null &&
          (typeof value === "object" || typeof value === "function") &&
          "then" in value &&
          typeof value.then === "function"
        ) {
          throw new LayerstackError("argument.invalid", operation, "native extensions must return synchronously, not a promise or thenable");
        }
        return value;
      });
    } catch (error) {
      try {
        this.#notify();
      } catch {
        // A callback failure is primary. In particular, a trap can prevent
        // draining, and its original operation/cause must remain intact.
      }
      throw error;
    }
    this.#notify();
    return result;
  }

  /** @internal */
  native<T>(operation: string, f: (native: NativeStage) => T): T {
    return this.#call(operation, f);
  }

  /** @internal The one {@link EditTarget} of a target record. */
  mintTarget(record: NativeTarget): EditTarget {
    const key = record.join("\u0000");
    const known = this.#targets.get(key)?.deref();
    if (known !== undefined) return known;
    const target = new EditTarget(this, this.#layer(record[1]), [...record]);
    this.#targets.set(key, new WeakRef(target));
    this.#forget.register(target, key);
    return target;
  }

  #setMuted(operation: string, layer: Layer, muted: boolean): boolean {
    this.#own(operation, layer);
    const changed = this.#call(operation, (native) => native.setLayerMuted(layer.id, muted));
    this.#notify();
    return changed;
  }

  /** The target edits go through, as the native module takes it: `target`, else the edit layer. */
  #target(operation: string, target: Layer | EditTarget | undefined): NativeTarget {
    if (target instanceof EditTarget) {
      if (owners.get(target) !== this) {
        throw new LayerstackError("target.affinity", operation, "the edit target belongs to another stage");
      }
      return target.nativeTarget;
    }
    const layer = target ?? this.editLayer;
    if (layer === null) {
      throw new LayerstackError(
        "layer.read_only",
        operation,
        'the stage has no edit layer: its root cannot be saved; open it with editLayer: "session"',
      );
    }
    this.#own(operation, layer);
    return ["layer", layer.id, null, null, null, true, 0, 1];
  }

  /** The layer a namespace edit goes to: a layer target, refusing one through an arc or a variant. */
  #namespaceLayer(operation: string, path: string, target: Layer | EditTarget | undefined): bigint {
    const [kind, layer] = this.#target(operation, target);
    if (kind !== "layer") {
      throw new LayerstackError(
        "namespace.unmappable",
        operation,
        "a move or delete edits the opinions of the stage's layer stack, so its target must be a layer of the stack, not an arc or a variant: pass a layer, or stage.editTargetForLayer(layer)",
        { path },
      );
    }
    return layer;
  }

  #receipt(native: NativeReceipt): EditReceipt {
    const [target, changes, loaded, failures, unavailable, layers] = native.record() as [
      NativeTarget,
      Row,
      bigint[],
      Row[],
      Row[],
      bigint[],
    ];
    const receipt = new EditReceipt(
      this,
      this.mintTarget(target),
      layers.map((id) => this.#layer(id)),
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
        const [record, muted, unmuted] = this.#call("onChange", (native) =>
          native.takeChanges(this.#listeners.length > 0),
        ) as [Row[] | null, bigint[], bigint[]];
        const batches = record === null ? [decodeChanges([[], [], ["/"], [], []])] : record.map(decodeChanges);
        if (muted.length > 0 || unmuted.length > 0) {
          const last = batches.pop() ?? decodeChanges([[], [], [], [], []]);
          batches.push({
            ...last,
            mutedLayers: muted.map((id) => this.#layer(id)),
            unmutedLayers: unmuted.map((id) => this.#layer(id)),
          });
        }
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

  #layer(id: bigint, operation = "layers"): Layer {
    const known = this.#layers.get(id);
    if (known !== undefined) return known;
    this.layers();
    const found = this.#layers.get(id);
    if (found === undefined) {
      throw new LayerstackError("layer.unknown", operation, `the stage has no layer ${id}`);
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

  /** The names of its relationships, schema-defined and authored, in property order. */
  relationshipNames(): string[] {
    return this.stage.native("relationshipNames", (native) => native.relationshipNames(this.path));
  }

  /** Its relationship `name` (`lookAt`, `material:binding`), or `null`. */
  relationship(name: string): Relationship | null {
    return this.stage.relationship(`${this.path}.${name}`);
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

  /**
   * The arcs of its composition, strongest first: its own (`local`)
   * opinions, then each variant, inherit, reference, payload and
   * specialize that reaches it, as OpenUSD's `UsdPrimCompositionQuery`
   * lists them. Author through one with {@link CompositionArc.editTarget},
   * whether or not the prim has an opinion there yet.
   *
   * Throws `prim.not_found` when the prim is gone.
   */
  compositionArcs(): CompositionArc[] {
    const rows = this.stage.native("compositionArcs", (native) => native.compositionArcs(this.path)) as Row[];
    return rows.map((row) => new CompositionArc(this.stage, this.path, row));
  }

  /**
   * The target inside the variant `variant` of its variant set `set`, in
   * `layer` (the {@link Stage.editLayer} by default), a layer of the
   * stage's layer stack: edits of this prim and its descendants are
   * written inside the branch (`/World/Robot{finish=painted}.color`), and
   * of any other path outside it, as OpenUSD's
   * `UsdEditTarget::ForLocalDirectVariant` writes them. The variant does
   * not have to be selected, or to exist yet: edits through the target
   * create it, and {@link EditTarget.composed} says whether it is
   * selected.
   *
   * As `ForLocalDirectVariant`, the target has no layer offset: times are
   * written as given, not mapped through the offset the stack reads
   * `layer` at. In the root layer that changes nothing; in a sublayer
   * authored with `(offset = 10)`, a sample set at stage time 15 is written
   * at its time 15 and read at stage time 25. To edit the selected
   * variant in stage time, use the variant's arc instead
   * (`prim.compositionArcs()`, `arc.editTarget(layer)`, or the
   * `editTarget` of an opinion inside it), which maps times through the
   * sublayer's offset; an unselected variant has no arc.
   *
   * Throws `path.invalid` for names that are not a variant set and a
   * variant, and the layer errors of {@link Stage.editTargetForLayer}.
   */
  variantEditTarget(set: string, variant: string, layer: Layer | null = this.stage.editLayer): EditTarget {
    if (typeof set !== "string" || typeof variant !== "string") {
      throw new LayerstackError("argument.invalid", "variantEditTarget", "expected (set: string, variant: string)");
    }
    if (layer === null) {
      throw new LayerstackError(
        "layer.read_only",
        "variantEditTarget",
        'the stage has no edit layer: its root cannot be saved; open it with editLayer: "session"',
      );
    }
    if (!(layer instanceof Layer) || owners.get(layer) !== this.stage) {
      throw new LayerstackError("layer.affinity", "variantEditTarget", "the layer belongs to another stage");
    }
    const record = this.stage.native("variantEditTarget", (native) =>
      native.variantEditTarget(this.path, set, variant, layer.id),
    ) as NativeTarget;
    return this.stage.mintTarget(record);
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
        ([layer, specPath, arc, role, detail, offset, scale, target]): Opinion => ({
          editTarget: target === null ? null : this.stage.mintTarget(target as NativeTarget),
          layer: this.stage.layer(layer as bigint),
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
  /** The edit target the edits went through; its {@link EditTarget.layer} is the layer they were written to. */
  readonly target: EditTarget;
  /** Every layer the edits wrote: the target's, and, for a move, each layer of the stack whose opinions moved. */
  readonly layers: readonly Layer[];
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
    target: EditTarget,
    layers: readonly Layer[],
    changes: Changes,
    loaded: readonly Layer[],
    failures: readonly AssetFailure[],
    unavailable: readonly UnavailableAsset[],
  ) {
    this.stage = stage;
    this.target = target;
    this.layers = layers;
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
  const [created, removed, resynced, changedInfoOnly, properties] = row as [
    string[],
    string[],
    string[],
    string[],
    [string, [string, PropertyChange["field"], string | null][]][],
  ];
  return {
    created,
    removed,
    resynced,
    changedInfoOnly,
    properties: properties.map(([prim, fields]) => ({
      prim,
      fields: fields.map(([property, field, metadata]) => ({ property, field, metadata })),
    })),
    mutedLayers: [],
    unmutedLayers: [],
  };
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
