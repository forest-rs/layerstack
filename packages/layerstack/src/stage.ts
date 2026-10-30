// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { ArrayCopy, ArrayHandle } from "./arrays.js";
import type { NativeArrayHandle } from "./arrays.js";
import { lowerEdits } from "./edits.js";
import type { NativeEditBatch } from "./edits.js";
import { LayerstackError } from "./errors.js";
import type { ErrorCode, HostErrorCode } from "./errors.js";
import {
  Material,
  Shader,
  ShaderPort,
  decodeBinding,
  decodeCoordSys,
  decodeMaterialSubsets,
  decodeProblems,
  decodeSubsets,
  readConnectionSourceInfo,
  readPort,
  readPorts,
} from "./schemas.js";
import { SkinningQuery } from "./skinning.js";
import {
  Primvar,
  Xformable,
  decodeBounds,
  decodeInstances,
  decodeLocalTransform,
  decodePurpose,
} from "./geometry.js";
import type {
  ArcKind,
  ArrayData,
  BoundingBoxes,
  BoundsCacheStats,
  BoundsOptions,
  InstanceOptions,
  LocalTransform,
  PointInstances,
  Prototype,
  PurposeInfo,
  TransformCacheStats,
  Visibility,
  EditTargetKind,
  AssetFailure,
  AssetSource,
  AssetUse,
  ChangeListener,
  Changes,
  ConnectionSourceInfo,
  CoordSysBinding,
  Edit,
  Explanation,
  FlattenResult,
  GeomSubset,
  Issue,
  LayerFormat,
  LoadResult,
  MaterialBinding,
  MaterialSubsets,
  MeshTopology,
  MeshTopologyProblemCode,
  OpenOptions,
  Opinion,
  PropertyChange,
  PackageResult,
  ResolveOptions,
  ResolveResult,
  SaveResult,
  SkinInput,
  SkinningCacheStats,
  Scalar,
  SkippedLayer,
  StageStatus,
  SubsetElementType,
  SubsetProblem,
  Transfer,
  UnavailableAsset,
  UnrepairedField,
  UnrepairedPath,
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
export const ABI = 9;

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
  muteAndUnmuteLayers(
    muteIds: BigUint64Array,
    muteIdentifiers: string[],
    unmuteIds: BigUint64Array,
    unmuteIdentifiers: string[],
  ): boolean;
  isLayerMuted(identifier: string): boolean;
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
  valueSourcesMightVary(paths: string[]): Uint8Array;
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
  boundMaterials(paths: string[], purpose: string): unknown;
  materialSubsets(path: string, purpose: string, time?: number | null): unknown;
  material(path: string): unknown;
  materialTerminal(path: string, terminal: string, contexts: string[]): unknown;
  shader(path: string): unknown;
  ports(path: string, kind: string): unknown;
  port(path: string): unknown;
  valueSources(path: string): unknown;
  connectionSourceInfo(path: string): unknown;
  geomSubsets(path: string, element?: string | null, family?: string | null, time?: number | null): unknown;
  subsetFamilyType(path: string, family: string): string;
  meshTopology(path: string, time?: number | null): unknown;
  validateSubsetFamily(path: string, element: string, family: string, time?: number | null): unknown;
  coordSysBindings(path: string, inherited: boolean): unknown;
  skinning(path: string): unknown;
  skinningInfluences(path: string, time: number | null | undefined, interleaved: boolean): unknown;
  skinningTransforms(path: string, time?: number | null): unknown;
  deformedPoints(path: string, time?: number | null): unknown;
  deformedNormals(path: string, time?: number | null): unknown;
  skinningCache(): unknown;
  transformCache(): unknown;
  boundsCache(): unknown;
  localTransform(path: string, time?: number | null): unknown;
  worldTransforms(paths: string[], time: number | null | undefined, parent: boolean): Float64Array;
  worldTransformsMightVary(paths: string[], time: number | null | undefined): Uint8Array;
  xformable(path: string): unknown;
  visibilities(paths: string[], time?: number | null, purpose?: string | null): unknown;
  purposes(paths: string[]): unknown;
  primvars(path: string, inherited: boolean): unknown;
  primvar(path: string, name: string, inherited: boolean): unknown;
  primvarValue(path: string, name: string, time: number | null | undefined, flattened: boolean): unknown;
  primvarIndices(path: string, name: string, time?: number | null): unknown;
  bounds(
    paths: string[],
    time: number | null | undefined,
    purposes: string[],
    hints: boolean,
    ignoreVisibility: boolean,
    space: string,
  ): unknown;
  extentsHint(
    path: string,
    time: number | null | undefined,
    purposes: string[],
    hints: boolean,
    ignoreVisibility: boolean,
  ): Float64Array;
  instanceTransforms(
    path: string,
    time: number | null | undefined,
    baseTime: number | null | undefined,
    includePrototypeTransform: boolean,
    applyMask: boolean,
  ): unknown;
  instanceMask(path: string, time?: number | null): Uint8Array;
  prototypes(): unknown;
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
      sessionIdentifier: string | undefined,
      sessionBytes: Uint8Array | undefined,
    ): NativeStage;
  };
  /** The batch of edits `Stage.apply` takes. */
  readonly EditBatch: new () => NativeEditBatch;
  nativeVersion(): unknown;
  /** The skinning functions over the caller's arrays ({@link Layerstack.skinPoints}). */
  skinPoints(
    method: string,
    geomBind: Float64Array,
    joints: Float64Array,
    indices: Int32Array | undefined,
    weights: Float32Array | undefined,
    interleaved: Float32Array | undefined,
    elementSize: number,
    constant: boolean,
    points: Float32Array,
  ): Float32Array;
  skinNormals(
    method: string,
    geomBind: Float64Array,
    joints: Float64Array,
    indices: Int32Array | undefined,
    weights: Float32Array | undefined,
    interleaved: Float32Array | undefined,
    elementSize: number,
    constant: boolean,
    normals: Float32Array,
  ): Float32Array;
  skinTransform(
    method: string,
    geomBind: Float64Array,
    joints: Float64Array,
    indices: Int32Array | undefined,
    weights: Float32Array | undefined,
    interleaved: Float32Array | undefined,
    elementSize: number,
  ): Float64Array;
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
   * front of it ({@link Stage.editLayer}). `sessionLayer` opens a session
   * layer from bytes instead.
   */
  open(root: { readonly identifier: string; readonly bytes: Uint8Array }, options: OpenOptions = {}): Stage {
    if (typeof root?.identifier !== "string" || !(root.bytes instanceof Uint8Array)) {
      throw new LayerstackError("argument.invalid", "open", "expected { identifier: string, bytes: Uint8Array }");
    }
    const sessionLayer = options.sessionLayer;
    if (
      sessionLayer !== undefined &&
      (typeof sessionLayer?.identifier !== "string" || !(sessionLayer.bytes instanceof Uint8Array))
    ) {
      throw new LayerstackError("argument.invalid", "open", "sessionLayer: expected { identifier: string, bytes: Uint8Array }");
    }
    const editLayer = options.editLayer ?? (sessionLayer ? "session" : "root");
    if (editLayer !== "root" && editLayer !== "session") {
      throw new LayerstackError("argument.invalid", "open", 'editLayer must be "root" or "session"');
    }
    if (sessionLayer && editLayer !== "session") {
      throw new LayerstackError("argument.invalid", "open", 'a sessionLayer takes the edits: editLayer must be "session"');
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
      this.#module.Stage.open(
        root.identifier,
        root.bytes,
        recover,
        session,
        nodes,
        perIndex,
        depth,
        provenance,
        sessionLayer?.identifier,
        sessionLayer?.bytes,
      ),
    );
    this.#bytesIn += root.bytes.byteLength + (sessionLayer?.bytes.byteLength ?? 0);
    return new Stage(this, native, provenance);
  }

  /**
   * Skins `points` (3 floats each, in geometry space) with the caller's
   * own arrays, as OpenUSD's `UsdSkelSkinPoints` does with separate or
   * interleaved influences: the bind transform, then each influence's
   * joint transform, weighted. Returns the points in skeleton space, a new
   * `Float32Array`; weights are used as given, not normalized.
   *
   * Throws `skel.deformation` when the influences do not fit (not one
   * block per point, not as many weights as indices, a joint index past the
   * palette, a weight that is not finite), `argument.invalid` for arrays
   * of the wrong shape.
   */
  skinPoints(input: SkinInput & { readonly points: Float32Array }): Float32Array {
    const [method, bind, joints, indices, weights, pairs, size, constant] = skinArguments(input, "skinPoints");
    const points = input.points;
    if (!(points instanceof Float32Array)) {
      throw new LayerstackError("argument.invalid", "skinPoints", "`points` must be a Float32Array");
    }
    this.#bytesIn += bind.byteLength + joints.byteLength + points.byteLength;
    const out = this.call("skinPoints", () =>
      this.#module.skinPoints(method, bind, joints, indices, weights, pairs, size, constant, points),
    );
    this.#bytesOut += out.byteLength;
    return out;
  }

  /** Skins vertex or varying `normals` as {@link Layerstack.skinPoints} skins points, normalized (`UsdSkelSkinNormals`). Throws as that does, and `skel.singular` for a transform whose linear part has no inverse. */
  skinNormals(input: SkinInput & { readonly normals: Float32Array }): Float32Array {
    const [method, bind, joints, indices, weights, pairs, size, constant] = skinArguments(input, "skinNormals");
    const normals = input.normals;
    if (!(normals instanceof Float32Array)) {
      throw new LayerstackError("argument.invalid", "skinNormals", "`normals` must be a Float32Array");
    }
    this.#bytesIn += bind.byteLength + joints.byteLength + normals.byteLength;
    const out = this.call("skinNormals", () =>
      this.#module.skinNormals(method, bind, joints, indices, weights, pairs, size, constant, normals),
    );
    this.#bytesOut += out.byteLength;
    return out;
  }

  /** The one transform constant influences move a rigid prim by, 16 numbers row by row, in skeleton space (`UsdSkelSkinTransform`). Throws as {@link Layerstack.skinPoints}, and `skel.deformation` for influences that are not one block. */
  skinTransform(input: SkinInput): Float64Array {
    const [method, bind, joints, indices, weights, pairs, size] = skinArguments(input, "skinTransform");
    this.#bytesIn += bind.byteLength + joints.byteLength;
    return this.call("skinTransform", () =>
      this.#module.skinTransform(method, bind, joints, indices, weights, pairs, size),
    );
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
   * A layer muted by identifier before the stage read it has an id but
   * no `Layer`: as OpenUSD never opens a muted layer (`SdfLayer::Find`
   * gives none, and `UsdStage::GetLayerStack` leaves it out), the stage
   * does not read it, and {@link Stage.mutedIdentifiers} lists it. Once
   * unmuted and read ({@link Stage.resolve}), its id names the `Layer`
   * read from it.
   *
   * Throws `argument.invalid` when `id` is not a `bigint`, and
   * `layer.unknown` when the stage holds no layer with it: an asset it
   * has not read yet has none, a layer muted before it was read
   * included.
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
   * `layer` is a {@link Layer} of the stage, or an identifier, anchored to
   * the root layer as OpenUSD anchors a muted identifier: `./lamp.usda`,
   * `lamp.usda` and `parts/../lamp.usda` beside a root `set/scene.usda`
   * all name `set/lamp.usda`, the layer an arc authored on the root to
   * that path reads. An identifier may name a layer the stage has not
   * read: it is not read while muted, and an arc or sublayer to it, now or
   * added later, composes muted (`composition.muted_asset` for an arc)
   * instead of asking for it ({@link Stage.unavailable}). The empty
   * identifier names nothing.
   *
   * Muting is the stage's state, not an edit: it makes no receipt, and
   * undo neither undoes it nor is blocked by it. The layer keeps its
   * content and its unsaved edits, and composes again when unmuted.
   * While it is muted, edits that target it fail with `layer.muted` and
   * {@link Stage.save} without a layer skips it (`save(layer)` writes it).
   * {@link Stage.move} and {@link Stage.delete} neither read nor edit a
   * muted layer, as OpenUSD's `UsdNamespaceEditor` does: its specs stay at
   * the old path, and the paths it authors are not repaired. Muting a
   * layer the stage does not use is not an error, as in OpenUSD; it
   * changes no prim.
   *
   * Throws `layer.affinity` for another stage's layer, `mute.root_layer`
   * for the root ({@link Stage.root}, by any spelling), which cannot be
   * muted, `argument.invalid` for neither a layer nor a string, and the
   * `io.*` error of an identifier that cannot be anchored.
   */
  muteLayer(layer: Layer | string): boolean {
    return this.#mute("muteLayer", [layer], []);
  }

  /**
   * Unmutes `layer`, a {@link Layer} or an identifier as
   * {@link Stage.muteLayer} takes it, as OpenUSD's `UsdStage::UnmuteLayer`
   * does: the stage recomposes with it again, and listeners hear it
   * ({@link Changes.unmutedLayers}). Returns whether its muting changed:
   * `false` for a layer not muted, the root included. A layer muted before
   * the stage read it, and assets its content asks for that the stage has
   * not read, are unavailable afterwards: `await stage.resolve(source)`.
   *
   * Throws as {@link Stage.muteLayer} does.
   */
  unmuteLayer(layer: Layer | string): boolean {
    return this.#mute("unmuteLayer", [], [layer]);
  }

  /**
   * Mutes the layers of `mute`, then unmutes those of `unmute`, as one
   * request: one recomposition, heard as one change, as OpenUSD's
   * `UsdStage::MuteAndUnmuteLayers`. Each is a {@link Layer} or an
   * identifier, as {@link Stage.muteLayer} takes it. Returns whether the
   * muted layers changed.
   *
   * As in OpenUSD, an unmute spelled as a mute is dropped, so the layer
   * stays muted; a {@link Layer} is spelled by its identifier. A layer
   * named under two spellings (`b.usda` to mute, `./b.usda` to unmute) is
   * muted, then unmuted, and listeners hear no change of it. A request
   * naming the root among the layers to mute fails as a whole
   * (`mute.root_layer`) and changes nothing, where OpenUSD reports an
   * error and mutes the others.
   *
   * Throws as {@link Stage.muteLayer} does.
   */
  muteAndUnmuteLayers(mute: Iterable<Layer | string>, unmute: Iterable<Layer | string>): boolean {
    return this.#mute("muteAndUnmuteLayers", [...mute], [...unmute]);
  }

  /**
   * The layers the stage mutes that it holds, by id ({@link Stage.layers}).
   * Never the root. A layer muted by identifier before the stage read it
   * has no {@link Layer}: {@link Stage.mutedIdentifiers} lists it.
   */
  mutedLayers(): Layer[] {
    return this.#mutedRows("mutedLayers")
      .filter(([, , held]) => held)
      .map(([id]) => this.#layer(id));
  }

  /**
   * The identifiers of the layers the stage mutes, sorted, as OpenUSD's
   * `UsdStage::GetMutedLayers` lists them: the layers it holds and those
   * muted by identifier before it read them. An anonymous layer, which
   * has no identifier, is listed only by {@link Stage.mutedLayers}.
   */
  mutedIdentifiers(): string[] {
    return this.#mutedRows("mutedIdentifiers")
      .flatMap(([, identifier]) => (identifier === null ? [] : [identifier]))
      .sort();
  }

  /**
   * Whether the stage mutes `layer`, a {@link Layer} or an identifier as
   * {@link Stage.muteLayer} takes it (OpenUSD's `UsdStage::IsLayerMuted`).
   * Throws `layer.affinity` for another stage's layer.
   */
  isMuted(layer: Layer | string): boolean {
    if (typeof layer === "string") {
      return this.#call("isMuted", (native) => native.isLayerMuted(layer));
    }
    this.#own("isMuted", layer);
    return this.#mutedRows("isMuted").some(([id]) => id === layer.id);
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

  /**
   * The skinning binding of the geometry at `path`
   * ({@link SkinningQuery}), or `null` when no skeleton with joint
   * influences binds it (`UsdSkelSkinningQuery`).
   *
   * Throws `prim.not_found`, a `skel.*` code for a binding that is partial
   * or malformed (`skel.input`: joint indices without weights),
   * `skel.method` for an unknown skinning method.
   */
  skinning(path: string): SkinningQuery | null {
    const row = this.#call("skinning", (native) => native.skinning(path)) as Row | null;
    return row === null ? null : new SkinningQuery(this, path, row);
  }

  /**
   * What the stage's skinning cache has done and holds: the evaluator
   * behind {@link SkinningQuery.deformedPoints},
   * {@link SkinningQuery.deformedNormals} and
   * {@link SkinningQuery.skinningTransforms}, which hears the stage's edits
   * before each read and invalidates only what they touch. Compare two
   * reads to see what a frame cost.
   */
  skinningCache(): SkinningCacheStats {
    const [
      hits,
      misses,
      skeletonBuilds,
      bindingBuilds,
      poseEvaluations,
      influenceResolutions,
      pointVertices,
      normalVectors,
      invalidations,
      resets,
      skeletons,
      bindings,
      usedBytes,
      capacityBytes,
    ] = this.#call("skinningCache", (native) => native.skinningCache()) as number[];
    return {
      hits: hits as number,
      misses: misses as number,
      skeletonBuilds: skeletonBuilds as number,
      bindingBuilds: bindingBuilds as number,
      poseEvaluations: poseEvaluations as number,
      influenceResolutions: influenceResolutions as number,
      pointVertices: pointVertices as number,
      normalVectors: normalVectors as number,
      invalidations: invalidations as number,
      resets: resets as number,
      skeletons: skeletons as number,
      bindings: bindings as number,
      usedBytes: usedBytes as number,
      capacityBytes: capacityBytes as number,
    };
  }

  /**
   * What the stage's transform cache has done and holds: the cache behind
   * {@link Stage.worldTransforms}, {@link Stage.parentToWorldTransforms}
   * and {@link Stage.localTransform}, which hears the stage's edits before
   * each read and invalidates only what they touch. Compare two reads to
   * see what a frame cost.
   */
  transformCache(): TransformCacheStats {
    const [localComputed, worldComputed, hits, prims, changeBatches, resets] = this.#call("transformCache", (native) =>
      native.transformCache(),
    ) as number[];
    return {
      localComputed: localComputed as number,
      worldComputed: worldComputed as number,
      hits: hits as number,
      prims: prims as number,
      changeBatches: changeBatches as number,
      resets: resets as number,
    };
  }

  /** What the stage's bounds caches have done and hold: the caches behind {@link Stage.worldBounds} and its siblings, which hear the stage's edits as {@link Stage.transformCache} does. */
  boundsCache(): BoundsCacheStats {
    const [computed, hits, invalidated, prims, caches, resets] = this.#call("boundsCache", (native) =>
      native.boundsCache(),
    ) as number[];
    return {
      computed: computed as number,
      hits: hits as number,
      invalidated: invalidated as number,
      prims: prims as number,
      caches: caches as number,
      resets: resets as number,
    };
  }

  /**
   * The local-to-world transform of each prim of `paths` at `time`, in
   * order: 16 numbers per prim in one `Float64Array`, prim `i` at
   * `16 * i`. The pseudo-root's is the identity.
   *
   * Matrices are row by row in OpenUSD's row-vector convention
   * (`GfMatrix4d`): a point `p` transforms as `p * M`, the translation is
   * the last row (`M[12..14]`), and a prim's world transform is its local
   * transform times its parent's. The same 16 numbers, read as
   * column-major, are the column-vector matrix WebGL's `uniformMatrix4fv`
   * and most engines take: upload them as they are.
   *
   * The stage keeps a transform cache that hears its edits: each frame
   * recomputes only the transforms an edit touched or that vary with time
   * (`UsdGeomXformCache::GetLocalToWorldTransform`).
   *
   * Throws `prim.not_found` naming the first path no prim is at.
   */
  worldTransforms(paths: readonly string[], options: { readonly time?: number } = {}): Float64Array {
    return this.#call("worldTransforms", (native) => native.worldTransforms([...paths], options.time, false));
  }

  /**
   * Whether each prim's local-to-world transform ({@link Stage.worldTransforms})
   * might differ at another time, the default time included: an op of its
   * local transform, or of an ancestor's up to the nearest
   * `!resetXformStack!`, has time samples (even one), a spline or value
   * clips. `false` means every time reads the same matrix: a renderer can
   * skip the prim when the time changes. Read through the stage's
   * transform cache at `time`, as {@link Stage.worldTransforms} reads.
   *
   * Throws `prim.not_found` naming the first path no prim is at.
   */
  worldTransformsMightVary(paths: readonly string[], options: { readonly time?: number } = {}): boolean[] {
    const flags = this.#call("worldTransformsMightVary", (native) =>
      native.worldTransformsMightVary([...paths], options.time),
    );
    return Array.from(flags, (flag) => flag === 1);
  }

  /**
   * Whether each attribute's value source might vary with time, the
   * default time included: time samples (even one), a spline or value
   * clips supply it, directly or under sparse edits
   * (`UsdAttributeQuery::ValueSourceMightBeTimeVarying`). `false` for a
   * path no attribute is at; `false` means every time reads the same
   * value. One read for many attributes: {@link Attribute.valueSourceMightBeTimeVarying}
   * asks for one.
   *
   * Throws `path.invalid` for a path that is not a property path.
   */
  valueSourcesMightBeTimeVarying(paths: readonly string[]): boolean[] {
    const flags = this.#call("valueSourcesMightBeTimeVarying", (native) => native.valueSourcesMightVary([...paths]));
    return Array.from(flags, (flag) => flag === 1);
  }

  /** The local-to-world transform of each prim's parent of `paths` at `time`, as {@link Stage.worldTransforms} lays them out (`UsdGeomXformCache::GetParentToWorldTransform`). */
  parentToWorldTransforms(paths: readonly string[], options: { readonly time?: number } = {}): Float64Array {
    return this.#call("parentToWorldTransforms", (native) =>
      native.worldTransforms([...paths], options.time, true),
    );
  }

  /**
   * The local transform of the prim at `path` at `time`: the product of
   * its ops, whether it resets the transform stack, and the ops that did
   * not contribute as written (`UsdGeomXformCache::GetLocalTransformation`).
   * The identity for a prim that is not `Xformable`.
   *
   * Throws `prim.not_found`.
   */
  localTransform(path: string, options: { readonly time?: number } = {}): LocalTransform {
    return decodeLocalTransform(
      this.#call("localTransform", (native) => native.localTransform(path, options.time)) as Row,
    );
  }

  /** The transform ops of the prim at `path` ({@link Xformable}), or `null` when it is not `Xformable`. Throws `prim.not_found`. */
  xformable(path: string): Xformable | null {
    const row = this.#call("xformable", (native) => native.xformable(path)) as Row | null;
    return row === null ? null : new Xformable(this, path, row);
  }

  /**
   * The computed visibility of each prim of `paths` at `time`, in order:
   * `invisible` when it or an ancestor is, else `inherited`
   * (`UsdGeomImageable::ComputeVisibility`). With a `purpose`, its
   * visibility for that purpose, `visible` among the answers
   * (`ComputeEffectiveVisibility`). `null` for a prim that is not
   * `Imageable`.
   *
   * Throws `prim.not_found` naming the first path no prim is at.
   */
  visibilities(
    paths: readonly string[],
    options: { readonly time?: number; readonly purpose?: string } = {},
  ): (Visibility | null)[] {
    return this.#call("visibilities", (native) =>
      native.visibilities([...paths], options.time, options.purpose ?? null),
    ) as (Visibility | null)[];
  }

  /**
   * The computed purpose of each prim of `paths`, in order: its own
   * authored `purpose`, else the nearest `Imageable` ancestor's, else its
   * fallback (`UsdGeomImageable::ComputePurposeInfo`); `null` for a prim
   * that is not `Imageable`.
   *
   * Throws `prim.not_found` naming the first path no prim is at.
   */
  purposeInfos(paths: readonly string[]): (PurposeInfo | null)[] {
    const rows = this.#call("purposeInfos", (native) => native.purposes([...paths])) as unknown[];
    return rows.map(decodePurpose);
  }

  /**
   * The primvars of the prim at `path`, sorted by name: its declared ones,
   * schema-defined included (`UsdGeomPrimvarsAPI::GetPrimvars`), or with
   * `inherited` those that produce a value, its own and the constant ones
   * its ancestors pass down (`FindPrimvarsWithInheritance`), each
   * {@link Primvar.prim} the prim it is on.
   *
   * Throws `prim.not_found`.
   */
  primvars(path: string, options: { readonly inherited?: boolean } = {}): Primvar[] {
    const rows = this.#call("primvars", (native) => native.primvars(path, options.inherited ?? false)) as Row[];
    return rows.map((row) => new Primvar(this, row));
  }

  /**
   * The primvar `name` (`displayColor` or `primvars:displayColor`) of the
   * prim at `path`, or `null` (`UsdGeomPrimvarsAPI::GetPrimvar`); with
   * `inherited`, its own when it authors a value, else the nearest
   * ancestor's constant one, else its own declaration
   * (`FindPrimvarWithInheritance`).
   *
   * Throws `prim.not_found`.
   */
  primvar(path: string, name: string, options: { readonly inherited?: boolean } = {}): Primvar | null {
    const row = this.#call("primvar", (native) => native.primvar(path, name, options.inherited ?? false)) as Row | null;
    return row === null ? null : new Primvar(this, row);
  }

  /**
   * The bound of each prim of `paths` in world space at `time`, for the
   * included purposes: its geometry's and its descendants', each oriented
   * box as a range and a matrix ({@link BoundingBoxes})
   * (`UsdGeomBBoxCache::ComputeWorldBound`). `alignedRanges` frames a
   * camera.
   *
   * The stage keeps a bounds cache for each set of options that hears its
   * edits, as the transform cache does: a model's authored `extentsHint`
   * stands for its subtree with `useExtentsHint`, a boundable's authored
   * `extent` for its own geometry, and geometry without one is bounded from
   * its points or shape.
   *
   * Throws `prim.not_found` naming the first path no prim is at, a
   * `bounds.*` code for geometry whose bound cannot be computed
   * (`bounds.extent`, `bounds.instancer`).
   */
  worldBounds(paths: readonly string[], options: BoundsOptions = {}): BoundingBoxes {
    return this.#bounds("worldBounds", paths, options, "world");
  }

  /** The bound of each prim of `paths` in its parent's space: {@link Stage.worldBounds} with only the prim's own local transform applied (`UsdGeomBBoxCache::ComputeLocalBound`). */
  localBounds(paths: readonly string[], options: BoundsOptions = {}): BoundingBoxes {
    return this.#bounds("localBounds", paths, options, "local");
  }

  /**
   * The bound of each prim of `paths` in its own space: its extent and its
   * descendants' bounds in it, without its own transform
   * (`UsdGeomBBoxCache::ComputeUntransformedBound`). A boundable's is its
   * extent; the aligned range is the extent of the subtree.
   */
  untransformedBounds(paths: readonly string[], options: BoundsOptions = {}): BoundingBoxes {
    return this.#bounds("untransformedBounds", paths, options, "untransformed");
  }

  /**
   * The `extentsHint` OpenUSD computes for the model at `path`: one extent
   * per purpose in default, render, proxy, guide order, 6 numbers each
   * (`[minX, minY, minZ, maxX, maxY, maxZ]`), trailing empty ones dropped
   * but one kept (`UsdGeomModelAPI::ComputeExtentsHint`). A boundable
   * repeats its computed extent for every purpose.
   *
   * Throws as {@link Stage.worldBounds}.
   */
  extentsHint(path: string, options: BoundsOptions = {}): Float64Array {
    return this.#call("extentsHint", (native) =>
      native.extentsHint(
        path,
        options.time,
        [...(options.purposes ?? [])],
        options.useExtentsHint ?? false,
        options.ignoreVisibility ?? false,
      ),
    );
  }

  /**
   * The instances of the point instancer at `path` at `time`, in the
   * instancer's space: one matrix per instance in one `Float64Array`
   * (laid out as {@link Stage.worldTransforms}), with its prototype index,
   * id and authored position, and the `prototypes` the indices address
   * (`UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`). By default
   * each prototype's own transform applies first and the mask leaves
   * inactive and invisible ids out. Multiply by the instancer's world
   * transform to place them.
   *
   * Throws `instancer.not_instancer` for a prim that is not a
   * `PointInstancer`, an `instancer.*` code for invalid instance data
   * (`instancer.length`, `instancer.prototype_index`).
   */
  pointInstances(path: string, options: InstanceOptions = {}): PointInstances {
    const row = this.#call("pointInstances", (native) =>
      native.instanceTransforms(
        path,
        options.time,
        options.baseTime,
        options.includePrototypeTransform ?? true,
        options.applyMask ?? true,
      ),
    ) as Row;
    return decodeInstances(row);
  }

  /**
   * The native instancing prototypes: for each, the instance roots that
   * share it (`instanceable` prims whose composition is the same), in
   * traversal order; prototypes in the order of their first instance. A
   * prototype's position names it in this snapshot only: an edit may
   * renumber them. Which instances share a prototype is OpenUSD's
   * (`UsdPrim::GetPrototype`, `UsdPrim::GetInstances`); OpenUSD orders its
   * `/__Prototype_N` prims otherwise.
   */
  prototypes(): Prototype[] {
    const rows = this.#call("prototypes", (native) => native.prototypes()) as string[][];
    return rows.map((instances) => ({ instances: Object.freeze([...instances]) }));
  }

  /**
   * The mask of the point instancer at `path` at `time`, one byte per
   * authored instance: 1 drawn, 0 inactive (`inactiveIds`) or invisible
   * (`invisibleIds`); empty when every instance is drawn
   * (`UsdGeomPointInstancer::ComputeMaskAtTime`).
   *
   * Throws `instancer.not_instancer`.
   */
  instanceMask(path: string, options: { readonly time?: number } = {}): Uint8Array {
    return this.#call("instanceMask", (native) => native.instanceMask(path, options.time));
  }

  #bounds(operation: string, paths: readonly string[], options: BoundsOptions, space: string): BoundingBoxes {
    const row = this.#call(operation, (native) =>
      native.bounds(
        [...paths],
        options.time,
        [...(options.purposes ?? [])],
        options.useExtentsHint ?? false,
        options.ignoreVisibility ?? false,
        space,
      ),
    ) as Row;
    return decodeBounds(row);
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

  /** The `Material` at `path` (or of a type derived from it), or `null` when no material is there. Throws `path.invalid`. */
  material(path: string): Material | null {
    const row = this.#call("material", (native) => native.material(path)) as Row | null;
    return row === null ? null : new Material(this, path, row);
  }

  /** The `Shader` at `path` (or of a type derived from it), or `null` when no shader is there. Throws `path.invalid`. */
  shader(path: string): Shader | null {
    const row = this.#call("shader", (native) => native.shader(path)) as Row | null;
    return row === null ? null : new Shader(this, path, row);
  }

  /** The shading input or output at the property path `path` (`/Looks/Stone/Surface.inputs:roughness`), or `null`. */
  port(path: string): ShaderPort | null {
    return readPort(this, path);
  }

  /**
   * The connection source the path text `path` names
   * (`/Looks/Stone/Texture.outputs:rgb`), checked against the stage, as
   * OpenUSD's `UsdShadeConnectionSourceInfo(stage, sourcePath)` reads it.
   * Its `source` is what {@link Edit} `connectToSource` takes when it is
   * `valid`. Text that is not a property path gives an empty info rather
   * than throwing; see {@link ConnectionSourceInfo}.
   */
  connectionSourceInfo(path: string): ConnectionSourceInfo {
    return readConnectionSourceInfo(this, path);
  }

  /**
   * The bound material of each prim of `paths`, in order: what
   * {@link Prim.boundMaterial} reads, for many prims at once, sharing the
   * ancestors' bindings and the bound collections' membership as OpenUSD's
   * `UsdShadeMaterialBindingAPI::ComputeBoundMaterials` does. `purpose` is
   * a material purpose (`preview`, `full`); without one, all purposes.
   *
   * Throws `prim.not_found` naming the first path no prim is at.
   */
  boundMaterials(paths: readonly string[], options: { readonly purpose?: string } = {}): (MaterialBinding | null)[] {
    const records = this.#call("boundMaterials", (native) =>
      native.boundMaterials([...paths], options.purpose ?? ""),
    ) as unknown[];
    return records.map(decodeBinding);
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
   * As OpenUSD, it repairs only the paths of specs a composed prim uses:
   * those in a variant no prim selects keep naming the old path, and
   * {@link EditReceipt.unrepaired} lists them, so a variant switch that
   * would compose a dangling arc or target can be caught at the move.
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
   * `namespace.unmappable`, ... A move that would change a layer that
   * cannot be saved ({@link Layer.readOnly}), such as a package member
   * whose arc back into the stage it would repair, throws
   * `layer.read_only` naming that layer, nothing changed, where OpenUSD
   * edits the member in memory and its save fails.
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
   * paths and arcs that name it where a composed prim depends on them;
   * {@link EditReceipt.unrepaired} lists those it leaves naming it.
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
   * A save of the stage leaves the session layer and its sublayers alone,
   * as OpenUSD's does; `save(layer)` saves one. An anonymous session layer
   * has nowhere to go, so it is listed in `anonymous`; export it with
   * {@link Stage.exportLayer}. A session layer opened from bytes
   * (`sessionLayer`) is written to its identifier. A package member
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
   * instances keep sharing prototypes, and each asset path is anchored to
   * the layer that authors it: `./albedo.png` in `set/rock.usda` is written
   * `./set/albedo.png`, the identifier it names marked relative to the
   * flattened layer (`set/albedo.png` would be a search path, which names
   * another asset), and a search path such as `textures/moss.png` is
   * anchored only when its asset was supplied beside that layer; absolute
   * paths and URLs are kept. Identifiers name assets from the host's root,
   * so the flattened layer resolves them as the stage did when it sits
   * there (as `flat.usda`, not `out/flat.usda`). Opening the bytes on
   * their own composes the same stage.
   * Content a layer cannot hold, such as a path relative to the anonymous
   * session layer, is written as authored and listed in `lost`, never
   * silently.
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

  #mute(operation: string, mute: readonly unknown[], unmute: readonly unknown[]): boolean {
    const split = (layers: readonly unknown[]): [BigUint64Array, string[]] => {
      const ids: bigint[] = [];
      const identifiers: string[] = [];
      for (const layer of layers) {
        if (typeof layer === "string") {
          identifiers.push(layer);
        } else if (layer instanceof Layer) {
          this.#own(operation, layer);
          ids.push(layer.id);
        } else {
          throw new LayerstackError("argument.invalid", operation, "expected a Layer or an identifier string");
        }
      }
      return [BigUint64Array.from(ids), identifiers];
    };
    const [muteIds, muteIdentifiers] = split(mute);
    const [unmuteIds, unmuteIdentifiers] = split(unmute);
    try {
      return this.#call(operation, (native) =>
        native.muteAndUnmuteLayers(muteIds, muteIdentifiers, unmuteIds, unmuteIdentifiers),
      );
    } finally {
      this.#notify();
    }
  }

  /** The muted layers as the module lists them: `[id, identifier, held]`. */
  #mutedRows(operation: string): MutingRow[] {
    return this.#call(operation, (native) => native.mutedLayers()) as MutingRow[];
  }

  /** The changes a muting row names: its layer, if the stage holds it, and its identifier. */
  #muting(rows: readonly MutingRow[]): [Layer[], string[]] {
    return [
      rows.filter(([, , held]) => held).map(([id]) => this.#layer(id)),
      rows.flatMap(([, identifier]) => (identifier === null ? [] : [identifier])).sort(),
    ];
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
    const [target, changes, loaded, failures, unavailable, layers, unrepaired] = native.record() as [
      NativeTarget,
      Row,
      bigint[],
      Row[],
      Row[],
      bigint[],
      [bigint, string, UnrepairedField, string, string | null][],
    ];
    const receipt = new EditReceipt(
      this,
      this.mintTarget(target),
      layers.map((id) => this.#layer(id)),
      decodeChanges(changes),
      loaded.map((id) => this.#layer(id)),
      failures.map(([asset, anchor, code, message]) => this.#failure(asset, anchor, code, message)),
      unavailable.map((row) => this.#unavailable(row)),
      unrepaired.map(([layer, spec, field, path, metadata]) =>
        Object.freeze({ layer: this.#layer(layer), spec, field, metadata, path }),
      ),
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
        ) as [Row[] | null, MutingRow[], MutingRow[]];
        const batches = record === null ? [decodeChanges([[], [], ["/"], [], [], []])] : record.map(decodeChanges);
        if (muted.length > 0 || unmuted.length > 0) {
          const last = batches.pop() ?? decodeChanges([[], [], [], [], [], []]);
          const [mutedLayers, mutedIdentifiers] = this.#muting(muted);
          const [unmutedLayers, unmutedIdentifiers] = this.#muting(unmuted);
          batches.push({ ...last, mutedLayers, unmutedLayers, mutedIdentifiers, unmutedIdentifiers });
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
   * The material it is bound to for `purpose` (`preview`, `full`; all
   * purposes without one), with the binding that decides it: its own or
   * an ancestor's, direct or through a collection, as OpenUSD's
   * `UsdShadeMaterialBindingAPI::ComputeBoundMaterial` resolves it;
   * `null` when nothing binds it. A binding to a path where no material is
   * has a `null` {@link MaterialBinding.material}. For many prims, use
   * {@link Stage.boundMaterials}.
   *
   * Throws `prim.not_found` when the prim is gone.
   */
  boundMaterial(options: { readonly purpose?: string } = {}): MaterialBinding | null {
    return this.stage.boundMaterials([this.path], options)[0] ?? null;
  }

  /**
   * Its `materialBind` face subsets at `time` and the material each is
   * bound to, with the faces in no subset bound as the mesh is, and the
   * family's validation: what a renderer draws a mesh with
   * ({@link MaterialSubsets}). Release the subsets' `indices` when done.
   *
   * Throws `subset.geometry` when the prim is not a mesh or tetrahedral
   * mesh, `value.decode` for indices or topology that fail to decode.
   */
  materialSubsets(options: { readonly purpose?: string; readonly time?: number } = {}): MaterialSubsets {
    const record = this.stage.native("materialSubsets", (native) =>
      native.materialSubsets(this.path, options.purpose ?? "", options.time),
    );
    return decodeMaterialSubsets(this.stage.layerstack, record);
  }

  /**
   * Its geometry subsets: its active, defined `GeomSubset` children in
   * namespace order, of `elementType` and in `familyName` when given,
   * with their indices at `time` (OpenUSD's `UsdGeomSubset::GetGeomSubsets`).
   *
   * Throws `argument.invalid` for an unknown element type.
   */
  geomSubsets(
    options: { readonly elementType?: SubsetElementType; readonly familyName?: string; readonly time?: number } = {},
  ): GeomSubset[] {
    const rows = this.stage.native("geomSubsets", (native) =>
      native.geomSubsets(this.path, options.elementType ?? null, options.familyName ?? null, options.time),
    );
    return decodeSubsets(this.stage.layerstack, rows);
  }

  /**
   * Its topology at `time` ({@link MeshTopology}): face counts, face
   * indices and hole indices, orientation, subdivision scheme and point
   * count, and whether they are valid (`UsdGeomMesh::ValidateTopology`);
   * `null` when the prim is not a `Mesh`.
   *
   * Throws `prim.not_found` when the prim is gone.
   */
  meshTopology(options: { readonly time?: number } = {}): MeshTopology | null {
    const record = this.stage.native("meshTopology", (native) => native.meshTopology(this.path, options.time));
    if (record === null) return null;
    const [counts, indices, holes, orientation, scheme, points, problem] = record as Row;
    const [code, reason] = (problem ?? []) as [MeshTopologyProblemCode?, string?];
    return {
      faceVertexCounts: counts as Int32Array,
      faceVertexIndices: indices as Int32Array,
      holeIndices: holes as Int32Array,
      orientation: orientation as string,
      subdivisionScheme: scheme as string,
      pointCount: points as number,
      valid: problem === null,
      problem: code === undefined ? null : { code, reason: reason! },
    };
  }

  /** The type of its subset family `familyName`, `unrestricted` when unauthored (`UsdGeomSubset::GetFamilyType`). */
  subsetFamilyType(familyName: string): string {
    return this.stage.native("subsetFamilyType", (native) => native.subsetFamilyType(this.path, familyName));
  }

  /**
   * Validates its subset family `familyName` of `elementType` subsets
   * against its topology, at `time` or, without one, at the default time
   * and every sample of the members' indices: the problems, none for a
   * valid family (`UsdGeomSubset::ValidateFamily`).
   *
   * Throws `subset.geometry` when the prim is not geometry,
   * `argument.invalid` for an unknown element type.
   */
  validateSubsetFamily(
    familyName: string,
    elementType: SubsetElementType,
    options: { readonly time?: number } = {},
  ): SubsetProblem[] {
    const rows = this.stage.native("validateSubsetFamily", (native) =>
      native.validateSubsetFamily(this.path, elementType, familyName, options.time),
    );
    return decodeProblems(rows);
  }

  /**
   * Its coordinate-system bindings: with `inherited` (the default), every
   * system bound on it or an ancestor, the nearest binding of each, as
   * OpenUSD's `UsdShadeCoordSysAPI::FindBindingsWithInheritance` finds
   * them; else only those its applied `CoordSysAPI` instances author.
   */
  coordSysBindings(options: { readonly inherited?: boolean } = {}): CoordSysBinding[] {
    const rows = this.stage.native("coordSysBindings", (native) =>
      native.coordSysBindings(this.path, options.inherited ?? true),
    );
    return decodeCoordSys(rows);
  }

  /** Its local transform at `time` ({@link Stage.localTransform}). */
  localTransform(options: { readonly time?: number } = {}): LocalTransform {
    return this.stage.localTransform(this.path, options);
  }

  /** Its local-to-world transform at `time`, 16 numbers row by row ({@link Stage.worldTransforms}). */
  worldTransform(options: { readonly time?: number } = {}): Float64Array {
    return this.stage.worldTransforms([this.path], options);
  }

  /** Its transform ops ({@link Stage.xformable}), or `null` when it is not `Xformable`. */
  xformable(): Xformable | null {
    return this.stage.xformable(this.path);
  }

  /** Its computed visibility at `time`, for `purpose` when given ({@link Stage.visibilities}); `null` when it is not `Imageable`. */
  visibility(options: { readonly time?: number; readonly purpose?: string } = {}): Visibility | null {
    return this.stage.visibilities([this.path], options)[0] ?? null;
  }

  /** Its computed purpose ({@link Stage.purposeInfos}); `null` when it is not `Imageable`. */
  purposeInfo(): PurposeInfo | null {
    return this.stage.purposeInfos([this.path])[0] ?? null;
  }

  /** Its primvars ({@link Stage.primvars}). */
  primvars(options: { readonly inherited?: boolean } = {}): Primvar[] {
    return this.stage.primvars(this.path, options);
  }

  /** Its primvar `name` ({@link Stage.primvar}), or `null`. */
  primvar(name: string, options: { readonly inherited?: boolean } = {}): Primvar | null {
    return this.stage.primvar(this.path, name, options);
  }

  /** Its skinning binding ({@link Stage.skinning}), or `null` when nothing skins it. */
  skinning(): SkinningQuery | null {
    return this.stage.skinning(this.path);
  }

  /** Its shading inputs (`inputs:*`), schema-defined and authored, in property order: a shader's parameters, a material's or node graph's interface. */
  inputs(): ShaderPort[] {
    return readPorts(this.stage, this.path, "input");
  }

  /** Its shading outputs (`outputs:*`), in property order. */
  outputs(): ShaderPort[] {
    return readPorts(this.stage, this.path, "output");
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

  /** Whether its value source might vary with time, the default time included (`UsdAttributeQuery::ValueSourceMightBeTimeVarying`); see {@link Stage.valueSourcesMightBeTimeVarying}. */
  valueSourceMightBeTimeVarying(): boolean {
    return this.stage.valueSourcesMightBeTimeVarying([this.path])[0] === true;
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
  /**
   * For a {@link Stage.move} or {@link Stage.delete}, the authored paths
   * it leaves naming the old location, in specs no composed prim uses
   * (a variant no prim selects): what a later variant switch would find
   * dangling. Sorted by layer, spec, field and path; empty when every
   * path naming the object was repaired, and for other edits.
   */
  readonly unrepaired: readonly UnrepairedPath[];

  /** @internal Use {@link Stage.apply}. */
  constructor(
    stage: Stage,
    target: EditTarget,
    layers: readonly Layer[],
    changes: Changes,
    loaded: readonly Layer[],
    failures: readonly AssetFailure[],
    unavailable: readonly UnavailableAsset[],
    unrepaired: readonly UnrepairedPath[] = [],
  ) {
    this.stage = stage;
    this.target = target;
    this.layers = layers;
    this.changes = changes;
    this.loaded = loaded;
    this.failures = failures;
    this.unavailable = unavailable;
    this.unrepaired = unrepaired;
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
  const [created, removed, resynced, changedInfoOnly, properties, resyncedProperties] = row as [
    string[],
    string[],
    string[],
    string[],
    [string, [string, PropertyChange["field"], string | null][]][],
    string[],
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
    resyncedProperties,
    mutedLayers: [],
    unmutedLayers: [],
    mutedIdentifiers: [],
    unmutedIdentifiers: [],
  };
}

/** A muted or unmuted layer as the module lists it: `[id, identifier, held]`. */
type MutingRow = [bigint, string | null, boolean];

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

/** @internal A value record. */
export function decodeValue(layerstack: Layerstack, record: unknown): Value | null {
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

type SkinArguments = [
  method: string,
  geomBind: Float64Array,
  joints: Float64Array,
  indices: Int32Array | undefined,
  weights: Float32Array | undefined,
  interleaved: Float32Array | undefined,
  elementSize: number,
  constant: boolean,
];

/** The arguments of the native skinning functions, shapes checked. */
function skinArguments(input: SkinInput, operation: string): SkinArguments {
  const fail = (message: string): never => {
    throw new LayerstackError("argument.invalid", operation, message);
  };
  if (typeof input !== "object" || input === null) fail("expected { geomBindTransform, jointTransforms, influences, ... }");
  const method = input.method ?? "classicLinear";
  const numbers = (value: unknown, field: string): Float64Array => {
    if (value instanceof Float64Array) return value;
    if (Array.isArray(value) && value.every((item) => typeof item === "number")) return Float64Array.from(value);
    return fail(`\`${field}\` must be a Float64Array or an array of numbers`);
  };
  const influences = input.influences;
  if (typeof influences !== "object" || influences === null) fail("`influences` must be { indices, weights } or { interleaved }");
  const size = influences.elementSize;
  if (!Number.isInteger(size) || size < 1) fail("`influences.elementSize` must be a positive integer");
  const constant = influences.constant ?? false;
  if ("interleaved" in influences) {
    if (!(influences.interleaved instanceof Float32Array)) fail("`influences.interleaved` must be a Float32Array");
    return [
      method,
      numbers(input.geomBindTransform, "geomBindTransform"),
      numbers(input.jointTransforms, "jointTransforms"),
      undefined,
      undefined,
      influences.interleaved,
      size,
      constant,
    ];
  }
  if (!(influences.indices instanceof Int32Array) || !(influences.weights instanceof Float32Array)) {
    fail("`influences.indices` must be an Int32Array and `influences.weights` a Float32Array");
  }
  return [
    method,
    numbers(input.geomBindTransform, "geomBindTransform"),
    numbers(input.jointTransforms, "jointTransforms"),
    influences.indices,
    influences.weights,
    undefined,
    size,
    constant,
  ];
}
