// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import type { ArrayHandle } from "./arrays.js";
import type { Layer } from "./stage.js";

/** The arc that reaches an opinion: the scene description field's name, `local` for a local opinion. */
export type ArcKind =
  | "local"
  | "inherits"
  | "variants"
  | "relocates"
  | "references"
  | "payloads"
  | "specializes";

/** What asks for an asset: a sublayer, a reference or a payload. */
export type NeedKind = "sublayers" | "references" | "payloads";

/**
 * An asset the stage wants: storage has no answer for it yet. Supply its
 * bytes under `identifier` ({@link Stage.supply}), or `null` if it does
 * not exist, then call {@link Stage.loadSupplied}; or let
 * {@link Stage.resolve} run the loop.
 */
export interface AssetNeed {
  /** The key to supply bytes under: the authored asset path anchored to the anchor layer's directory, `.` and `..` removed. */
  readonly identifier: string;
  /** The asset path as authored. */
  readonly asset: string;
  /** The layer that authors the arc or sublayer. */
  readonly anchor: Layer;
  /** What asks for it. */
  readonly arc: NeedKind;
}

/** An asset the stage uses that is confirmed absent: composition reports it and its arc contributes nothing. */
export interface MissingAsset {
  /** The asset path as authored. */
  readonly asset: string;
  /** The layer that authors the arc or sublayer. */
  readonly anchor: Layer;
  /** What asks for it. */
  readonly arc: NeedKind;
}

/** An asset a load could not read. */
export interface AssetFailure {
  /** The asset path as authored. */
  readonly asset: string;
  /** The layer that authors it, if any. */
  readonly anchor: Layer | null;
  /** The read failure's code (`io.not_found` for one confirmed absent). */
  readonly code: string;
  /** The failure in words. */
  readonly message: string;
}

/** What {@link Stage.loadSupplied} read. */
export interface LoadResult {
  /** The layers read, package members included. */
  readonly loaded: readonly Layer[];
  /** The assets that failed to read, kept as missing. */
  readonly failures: readonly AssetFailure[];
}

/** What {@link Stage.resolve} did, over every round. */
export interface ResolveResult extends LoadResult {
  /** The rounds of want, fetch, supply and load. */
  readonly rounds: number;
  /** The identifiers fetched. */
  readonly fetched: number;
  /** The bytes supplied. */
  readonly bytes: number;
}

/** Options of {@link Stage.resolve}. */
export interface ResolveOptions {
  /** Aborts the loop (`resolve.aborted`); it is also passed to each fetch. */
  readonly signal?: AbortSignal;
  /** The most rounds before failing with `resolve.limit`; 100 by default. */
  readonly maxRounds?: number;
}

/**
 * Where {@link Stage.resolve} gets bytes. `fetch` returns the bytes of
 * an identifier, or `null` when it confirmed there is no such asset.
 */
export interface AssetSource {
  fetch(identifier: string, signal?: AbortSignal): Promise<Uint8Array | null>;
}

/** A composition error: not thrown, reported with the stage ({@link Stage.status}). */
export interface Issue {
  /** The stable code, `composition.*` (such as `composition.unresolved_asset`). */
  readonly code: string;
  /** The error in words, with its paths. */
  readonly message: string;
  /** The composed prim whose composition found it; `null` for an error in a layer stack. */
  readonly prim: string | null;
}

/** The stage's state now. */
export interface StageStatus {
  /** The prims the default traversal visits (active, loaded, defined, non-abstract). */
  readonly prims: number;
  /** The composition errors. */
  readonly compositionErrors: readonly Issue[];
  /** The assets the stage wants now ({@link Stage.wanted}). */
  readonly wanted: readonly AssetNeed[];
  /** The assets confirmed absent. */
  readonly missing: readonly MissingAsset[];
  /** Whether nothing is wanted or missing. */
  readonly complete: boolean;
}

/** Options of {@link Layerstack.open}. */
export interface OpenOptions {
  /**
   * `"strict"` (the default) rejects a layer whose content is malformed
   * (`io.rejected`); `"recover"` imports what parses.
   */
  readonly importPolicy?: "strict" | "recover";
  /**
   * Where edits go by default ({@link Stage.editLayer}).
   *
   * - `"root"` (the default): the root layer, when it can be saved. A
   *   read-only root (a USDZ package) then has no edit layer, and edits
   *   targeting it fail with `layer.read_only`.
   * - `"session"`: an empty anonymous session layer, placed in front of
   *   the root (the strongest layer of the stage), takes the edits. This
   *   is how to edit a USDZ package: the package stays as it was read.
   */
  readonly editLayer?: "root" | "session";
}

/** A scalar's storage type in an array: `f16` is half floats as raw bits, `bool` is bytes of 0 or 1. */
export type Scalar = "bool" | "u8" | "i32" | "u32" | "i64" | "u64" | "f16" | "f32" | "f64" | "string";

/** The scalars of a numeric array, as one typed array. */
export type NumericElements =
  | Uint8Array
  | Uint16Array
  | Int32Array
  | Uint32Array
  | BigInt64Array
  | BigUint64Array
  | Float32Array
  | Float64Array;

/** The elements of an array, as one typed array of scalars, or strings for text. */
export type ArrayElements = NumericElements | string[];

/**
 * An array value: its layout, and its elements as one buffer.
 *
 * `length` elements of `components` scalars each, flattened
 * (`point3f[]` is `f32` with 3 components). Matrices are row-major;
 * quaternions are imaginary first, `[i, j, k, r]`.
 *
 * A numeric array is an {@link ArrayHandle}: its elements stay in the
 * module, and `view()` is a typed array over them, valid until the next
 * call into the module. A text array's strings are copied out once.
 */
export interface ArrayData extends Disposable {
  /** The value type name, such as `point3f[]`. */
  readonly type: string;
  /** The storage type of each scalar. */
  readonly scalar: Scalar;
  /** Scalars per element. */
  readonly components: number;
  /** Elements. */
  readonly length: number;
  /** The bytes of the scalars (two per UTF-16 code unit for text). */
  readonly byteLength: number;
  /** Whether it was released: a released handle no longer reads. */
  readonly released: boolean;
  /**
   * The elements without a copy. For a numeric array, a typed array over
   * the module's memory: valid until the next call into the module, and
   * never to be written. For text, the frozen strings.
   */
  view(): NumericElements | readonly string[];
  /** A new JS-owned copy of the elements; each call copies. */
  copy(): ArrayElements;
  /** Releases what it holds natively (a numeric array's pinned buffer). Idempotent. */
  release(): void;
}

/** A composed value. `kind` says its shape; `type` is its value type name (`float`, `color3f`, `point3f[]`). */
export type Value =
  | {
      readonly kind: "scalar";
      readonly type: string;
      /** A number, a `bigint` for `int64` and `uint64`, a string for `string`, `token`, `asset`, `pathExpression`, `path` and `unregistered` (metadata text kept unparsed). */
      readonly value: boolean | number | bigint | string;
    }
  | {
      readonly kind: "tuple";
      readonly type: string;
      /** A vector, a matrix (row-major) or a quaternion (`[i, j, k, r]`). */
      readonly value: readonly number[];
    }
  | { readonly kind: "array"; readonly type: string; readonly array: ArrayData }
  | { readonly kind: "dictionary"; readonly type: string; readonly value: ReadonlyMap<string, Value | null> }
  | { readonly kind: "list"; readonly type: string; readonly value: readonly (Value | null)[] }
  | {
      /** A value this package does not represent yet. */
      readonly kind: "opaque";
      readonly type: string;
    };

/** Where a resolved value comes from. */
export type ValueSource = "none" | "default" | "timeSamples" | "spline" | "valueClips" | "fallback";

/** One consulted opinion and its part in the value. */
export interface Opinion {
  /** The layer that authors it. */
  readonly layer: Layer;
  /** The spec path in that layer (`/Base.size`). */
  readonly specPath: string;
  /** The arc that reaches it; `null` without a composition node. */
  readonly arc: ArcKind | null;
  /** Its part: it contributed, it is a block in effect, or it was ignored. */
  readonly role: "contributed" | "block" | "ignored";
  /** What it contributed, for a contributing opinion. */
  readonly contribution: "value" | "arrayEdit" | "dictionary" | "listEdit" | null;
  /** Why it was ignored, for an ignored opinion. */
  readonly cause: "shadowed" | "cutOffByBlock" | "incompatible" | null;
  /** The layer offset mapping stage time to the opinion's layer time. */
  readonly layerOffset: { readonly offset: number; readonly scale: number };
}

/** Why an attribute has its value. */
export interface Explanation {
  /** The resolved value, exactly what {@link Attribute.get} reads. */
  readonly value: Value | null;
  /** Where it comes from. */
  readonly source: ValueSource;
  /** The bracketing sample times, for `timeSamples`. */
  readonly samples: { readonly lower: number; readonly upper: number } | null;
  /** Every opinion consulted, strongest first. */
  readonly opinions: readonly Opinion[];
}

/** A variant set of a prim. */
export interface VariantSet {
  /** The set's name. */
  readonly name: string;
  /** The selected variant; `null` when nothing selects one. */
  readonly selection: string | null;
  /** The set's variant names, in composed order. */
  readonly variants: readonly string[];
}

/**
 * What one recomposition changed, as prim paths. A listener registered
 * with {@link Stage.onChange} receives one per recomposition, in order.
 *
 * Re-read what a path covers: a `resynced` path's whole subtree may have
 * changed; a `changedInfoOnly` prim kept its children, but its fields and
 * properties may have changed.
 */
export interface Changes {
  /** Prims that exist now and did not before. */
  readonly created: readonly string[];
  /** Prims that existed before and do not now. */
  readonly removed: readonly string[];
  /** Roots of subtrees whose composed structure may have changed. `/` when the history was lost: re-read everything. */
  readonly resynced: readonly string[];
  /** Surviving prims whose opinions changed without a resync of their subtree. */
  readonly changedInfoOnly: readonly string[];
}

/** A listener of {@link Stage.onChange}. */
export type ChangeListener = (changes: Changes) => void;

/** A typed array of numbers or bigints, sent as one buffer. */
export type NumericArray =
  | Float32Array
  | Float64Array
  | Int8Array
  | Int16Array
  | Int32Array
  | Uint8Array
  | Uint16Array
  | Uint32Array
  | BigInt64Array
  | BigUint64Array;

/**
 * A value to author: its value type name and its scalars, or a
 * {@link Value} read from a stage, sent back as it is.
 *
 * `value` is in the layout {@link Value} reads:
 *
 * - a scalar: `true`, `2.5`, `9007199254740993n` (a `bigint` keeps
 *   `int64`/`uint64` exact past 2^53), `"render"` (`string`, `token`,
 *   `asset`, `pathExpression`);
 * - a tuple: `[1, 0, 0]` (`double3`, `color3f`), a matrix row-major
 *   (`matrix4d`), a quaternion `[i, j, k, r]`;
 * - an array: the elements flattened (`point3f[]` as six numbers for two
 *   points, or `[[0, 0, 0], [1, 2, 3]]`), or one typed array, which is
 *   copied once (`Float32Array` for `point3f[]`, `BigInt64Array` for
 *   `int64[]`, half floats as raw bits in a `Uint16Array`), or an
 *   {@link ArrayHandle} read from the stage, which is shared, not copied;
 * - a dictionary: {@link DictionaryInput}.
 *
 * The type decides the conversion: numbers round to `float` and `half`,
 * and must be exact integers of an integer type (`value.range`).
 */
export type ValueInput =
  | Value
  | {
      /** The value type name (`double`, `color3f`, `token[]`), as {@link Attribute.typeName} reads it. */
      readonly type: string;
      /** The scalars. */
      readonly value: ScalarInput | readonly (ScalarInput | readonly ScalarInput[])[] | NumericArray;
    }
  | DictionaryInput;

/**
 * A `dictionary` to author (`customData`, `assetInfo`): each entry a
 * {@link ValueInput} of its own type, nested dictionaries included. It is
 * authored whole, replacing the field's dictionary in the target layer
 * (OpenUSD's `UsdObject::SetMetadata`), and kept ordered by key.
 *
 * ```ts
 * { type: "dictionary", value: { note: { type: "string", value: "moss" }, scale: { type: "double3", value: [1, 1, 1] } } }
 * ```
 */
export interface DictionaryInput {
  readonly type: "dictionary";
  readonly value: ReadonlyMap<string, ValueInput> | { readonly [key: string]: ValueInput };
}

/** One scalar of a {@link ValueInput}. */
export type ScalarInput = boolean | number | bigint | string;

/** Where an added reference or payload goes in the list op of the prim (OpenUSD's `UsdListPosition`). */
export type ListPosition = "frontOfPrepend" | "backOfPrepend" | "frontOfAppend" | "backOfAppend";

/**
 * `UsdStage::DefinePrim` (`specifier` `"def"`, the default),
 * `OverridePrim` (`"over"`) or `CreateClassPrim` (`"class"`), on the
 * target layer, reading the stage as it was before the edits:
 *
 * - `def`: nothing when the prim is defined and `typeName` is absent or
 *   its type already; otherwise authors `def` (and `typeName`) on its
 *   spec, and `def` on each ancestor that is not defined.
 * - `over`: nothing when the prim exists; otherwise an `over` spec.
 * - `class`: nothing when the prim is a class; otherwise authors `class`.
 */
export interface DefinePrimEdit {
  readonly type: "definePrim";
  /** The prim path. */
  readonly path: string;
  /** The type name (`Xform`, `Sphere`) to author. */
  readonly typeName?: string;
  /** `def` by default. */
  readonly specifier?: "def" | "over" | "class";
}

/** `UsdStage::RemovePrim`: removes the prim's spec in the target layer, with everything beneath it. Other layers keep their opinions. */
export interface RemovePrimEdit {
  readonly type: "removePrim";
  readonly path: string;
}

/** `UsdPrim::RemoveProperty`: removes the property's spec in the target layer. */
export interface RemovePropertyEdit {
  readonly type: "removeProperty";
  /** The property path (`/World/Rock.size`). */
  readonly path: string;
}

/**
 * `UsdPrim::CreateAttribute`: makes sure the target layer has a spec for
 * the attribute (its prim spec created as an `over` when missing), then
 * authors `value` as its default when given.
 *
 * As in OpenUSD, a spec the target layer already has is kept, and a new
 * spec copies the prim's schema definition of the attribute, else another
 * layer's declaration of it: `typeName`, `variability` and `custom` apply
 * only to an attribute nothing defines or declares yet.
 */
export interface CreateAttributeEdit {
  readonly type: "createAttribute";
  /** The property path. */
  readonly path: string;
  /** A registered value type name (`double`, `color3f[]`). */
  readonly typeName: string;
  /** `varying` by default. */
  readonly variability?: "varying" | "uniform";
  /** `true` by default, as OpenUSD's `CreateAttribute`; `false` for a property a schema defines. */
  readonly custom?: boolean;
  /** A default value. */
  readonly value?: ValueInput;
}

/**
 * `UsdAttribute::Set`: the default value, or the time sample at `time`
 * (a stage time code), leaving other samples. The value must have the
 * attribute's declared type (`edit.rejected.type_mismatch`); a missing
 * attribute spec is created with the composed declaration's type.
 */
export interface SetValueEdit {
  readonly type: "setValue";
  /** The property path. */
  readonly path: string;
  readonly value: ValueInput;
  /** The time code of a time sample; the default value without one. */
  readonly time?: number;
}

/** `UsdAttribute::ClearAtTime`: clears the default value, or with `time` the one time sample there. */
export interface ClearValueEdit {
  readonly type: "clearValue";
  readonly path: string;
  readonly time?: number;
}

/** `UsdAttribute::Block`: clears the time samples and authors a blocked default, hiding weaker opinions. */
export interface BlockValueEdit {
  readonly type: "blockValue";
  readonly path: string;
}

/** `UsdRelationship::SetTargets`: the explicit target list, creating a `custom` relationship when nothing declares it. */
export interface SetTargetsEdit {
  readonly type: "setTargets";
  /** The relationship's property path. */
  readonly path: string;
  /** Prim or property paths. */
  readonly targets: readonly string[];
}

/**
 * `UsdReferences::AddReference` / `UsdPayloads::AddPayload`. With `asset`
 * an external arc (resolved against the target layer, as authored); without
 * one an internal arc. Without `primPath` it targets the default prim. An
 * asset the stage has not read is wanted ({@link EditReceipt.wanted}): run
 * {@link Stage.resolve}.
 */
export interface AddArcEdit {
  readonly type: "addReference" | "addPayload";
  /** The prim path. */
  readonly path: string;
  readonly asset?: string;
  readonly primPath?: string;
  /** `backOfPrepend` by default, as OpenUSD's. */
  readonly position?: ListPosition;
  /** Time offset and scale across the arc; identity by default. */
  readonly layerOffset?: { readonly offset?: number; readonly scale?: number };
}

/**
 * `UsdReferences::RemoveReference` / `UsdPayloads::RemovePayload`: removes
 * the arc naming `asset` and `primPath` from the target layer's lists, and
 * adds it to the deleted items, which removes it from weaker layers too.
 */
export interface RemoveArcEdit {
  readonly type: "removeReference" | "removePayload";
  readonly path: string;
  readonly asset?: string;
  readonly primPath?: string;
}

/**
 * `UsdVariantSet::SetVariantSelection`; `null` (or `""`) clears it. Assets
 * the selected variant needs that the stage has not read are wanted
 * ({@link EditReceipt.wanted}).
 */
export interface SetVariantSelectionEdit {
  readonly type: "setVariantSelection";
  readonly path: string;
  readonly set: string;
  readonly variant: string | null;
}

/** `UsdPrim::SetActive`. */
export interface SetActiveEdit {
  readonly type: "setActive";
  readonly path: string;
  readonly active: boolean;
}

/** `UsdPrim::SetInstanceable`. */
export interface SetInstanceableEdit {
  readonly type: "setInstanceable";
  readonly path: string;
  readonly instanceable: boolean;
}

/** `UsdModelAPI::SetKind`: the `kind` metadata token (`component`, `assembly`, `group`). */
export interface SetKindEdit {
  readonly type: "setKind";
  readonly path: string;
  readonly kind: string;
}

/**
 * `UsdObject::SetMetadata` of a prim (prim path) or property (property
 * path). A property the target layer has no spec for gets one copied from
 * its schema definition or declaration, as for {@link CreateAttributeEdit}.
 * Fields with edits of their own (`active`, `typeName`, `references`,
 * `variantSelection`, ...) fail with `edit.rejected.reserved_field`.
 */
export interface SetMetadataEdit {
  readonly type: "setMetadata";
  readonly path: string;
  readonly key: string;
  readonly value: ValueInput;
}

/** `UsdObject::ClearMetadata`. */
export interface ClearMetadataEdit {
  readonly type: "clearMetadata";
  readonly path: string;
  readonly key: string;
}

/**
 * One edit: plain data, so edits can be logged, sent and replayed. A list
 * of them is applied atomically to one layer by {@link Stage.apply}.
 * Paths are stage paths; times are stage time codes.
 */
export type Edit =
  | DefinePrimEdit
  | RemovePrimEdit
  | RemovePropertyEdit
  | CreateAttributeEdit
  | SetValueEdit
  | ClearValueEdit
  | BlockValueEdit
  | SetTargetsEdit
  | AddArcEdit
  | RemoveArcEdit
  | SetVariantSelectionEdit
  | SetActiveEdit
  | SetInstanceableEdit
  | SetKindEdit
  | SetMetadataEdit
  | ClearMetadataEdit;

/** A layer file format: USDA text or USDC binary. */
export type LayerFormat = "usda" | "usdc";

/** A layer {@link Stage.save} wrote: persist `bytes` under `identifier`. */
export interface SavedLayer {
  /** The layer, now clean ({@link Stage.isDirty} is false). */
  readonly layer: Layer;
  /** The identifier it was read from, and is saved to. */
  readonly identifier: string;
  /** The layer in its own format (its identifier's extension), a copy for the host. */
  readonly bytes: Uint8Array;
}

/** A layer {@link Stage.save} could not write; it stays dirty. */
export interface LayerFailure {
  readonly layer: Layer;
  /** `io.read_only` for a package member, or the writer's `io.*` code. */
  readonly code: string;
  readonly message: string;
}

/**
 * What {@link Stage.save} did. The module writes nothing anywhere: the
 * host persists `written`, which also become the sources the stage reads
 * those identifiers from.
 */
export interface SaveResult {
  /** The layers written. */
  readonly written: readonly SavedLayer[];
  /** The layers that could not be written. */
  readonly failures: readonly LayerFailure[];
  /** Dirty anonymous layers (the session layer), which have no identifier to save to: use {@link Stage.exportLayer}. */
  readonly anonymous: readonly Layer[];
  /** The bytes written, summed. */
  readonly bytes: number;
}

/** Composed content a flatten could not write ({@link FlattenResult.lost}). */
export interface FlattenLoss {
  /** The composed prim or property path. */
  readonly path: string;
  /** What was lost, in words. */
  readonly message: string;
}

/** What {@link Stage.exportFlattened} wrote. */
export interface FlattenResult {
  /** The flattened layer: no composition arcs, every composed prim and value. */
  readonly bytes: Uint8Array;
  /** Whether everything composed was written; when not, `lost` says what was left out. */
  readonly lossless: boolean;
  readonly lost: readonly FlattenLoss[];
}

/** A dependency {@link Stage.exportPackage} could not package. */
export interface UnresolvedAsset {
  /** The layer that authors it. */
  readonly layer: Layer;
  /** The asset path as authored. */
  readonly asset: string;
  /** Why it could not be packaged (not supplied yet, confirmed absent, ...). */
  readonly reason: string;
}

/** What {@link Stage.exportPackage} made. */
export interface PackageResult {
  /** The USDZ package; `null` when a dependency is unresolved. */
  readonly bytes: Uint8Array | null;
  /** The layers packaged, the root first. */
  readonly layers: number;
  /** The other assets packaged (textures, ...). */
  readonly assets: number;
  /** The dependencies that kept the package from being made. */
  readonly unresolved: readonly UnresolvedAsset[];
  /** Layers the stage composes that the package leaves out: the session layer, which is not part of the root's dependencies. */
  readonly omitted: readonly Layer[];
}

/** The version of this package and of the native module it runs. */
export interface Version {
  /** This package's version. */
  readonly package: string;
  /** The `layerstack_wasm` crate version the module was built from. */
  readonly crate: string;
  /** The source revision the module was built from, `unknown` when not recorded. */
  readonly revision: string;
  /** The module's ABI, equal to this package's. */
  readonly abi: number;
}

/** Bytes that crossed the boundary through this instance, for measuring transfer costs. */
export interface Transfer {
  /** Bytes copied into the module: opened and supplied sources, and values authored from JS buffers. */
  readonly bytesIn: number;
  /** Bytes copied out: array copies ({@link ArrayData.copy}), saved and exported bytes. */
  readonly bytesOut: number;
  /** Views built over the module's memory ({@link ArrayHandle.view}): no copy. */
  readonly views: number;
  /** The bytes those views exposed. */
  readonly bytesViewed: number;
  /** The module's memory now, in bytes (0 when the module does not expose it). */
  readonly memoryBytes: number;
}
