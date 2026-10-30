// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import type { ArrayHandle } from "./arrays.js";
import type { EditTarget, Layer } from "./stage.js";

/** The arc that reaches an opinion: the scene description field's name, `local` for a local opinion. */
export type ArcKind =
  | "local"
  | "inherits"
  | "variants"
  | "relocates"
  | "references"
  | "payloads"
  | "specializes";

/**
 * What an {@link EditTarget} writes through: `"layer"`, a layer of the
 * stage's own layer stack, every path mapped to itself; `"variant"`, a
 * variant branch on a prim, in such a layer, with no layer offset
 * (OpenUSD's `ForLocalDirectVariant`); `"arc"`, a layer reached through
 * any other arc (a reference, a payload, an inherit, ...) or a variant's
 * node in a sublayer read with an offset.
 */
export type EditTargetKind = "layer" | "variant" | "arc";

/** What uses an asset: a sublayer, a reference or a payload. */
export type AssetUse = "sublayer" | "reference" | "payload";

/**
 * An asset the stage asks for that the host has not supplied yet: it is
 * unavailable, and may still arrive. Supply its bytes under `identifier`
 * ({@link Stage.supply}), or `null` if it does not exist, then call
 * {@link Stage.loadSupplied}; or let {@link Stage.resolve} run the loop.
 */
export interface UnavailableAsset {
  /** The key to supply bytes under: the authored asset path anchored to the anchor layer's directory, `.` and `..` removed. */
  readonly identifier: string;
  /** The asset path as authored. */
  readonly assetPath: string;
  /** The layer that authors the arc or sublayer. */
  readonly anchor: Layer;
  /** What uses it. */
  readonly usedBy: AssetUse;
  /**
   * The paths of the prims composed without it: those whose reference or
   * payload names it, or for a sublayer the first prim to read its layer
   * stack (`/` for the stage's own). They are on the stage, without the
   * asset's opinions, until it loads.
   */
  readonly prims: readonly string[];
}

/**
 * An asset the stage uses that is unresolved: composition reports it and
 * its arc or sublayer contributes nothing, because the asset is missing
 * (`reason` `io.not_found`) or its read failed.
 */
export interface UnresolvedAsset {
  /** The asset path as authored. */
  readonly assetPath: string;
  /** The layer that authors the arc or sublayer. */
  readonly anchor: Layer;
  /** What uses it. */
  readonly usedBy: AssetUse;
  /** The identifier a read of it addressed; `null` when none was read. */
  readonly identifier: string | null;
  /** The code of the read failure (`io.not_found` for one confirmed missing); `null` when no read of it failed. */
  readonly reason: string | null;
}

/** An asset a load could not read. */
export interface AssetFailure {
  /** The asset path as authored. */
  readonly assetPath: string;
  /** The layer that authors it, if any. */
  readonly anchor: Layer | null;
  /** The read failure's code (`io.not_found` for one confirmed absent). */
  readonly code: string;
  /** The failure in words. */
  readonly message: string;
}

/**
 * An authored path a {@link Stage.move} or {@link Stage.delete} leaves
 * naming the old location: one in a spec no composed prim uses, such as
 * a variant no prim selects, which the edit moves or removes with its
 * prim without repairing, as OpenUSD's `UsdNamespaceEditor` does. Once
 * a later edit composes that spec (selecting the variant), the path
 * names a location that is gone. What the edit writes does not depend on
 * it.
 */
export interface UnrepairedPath {
  /** The layer that authors it. */
  readonly layer: Layer;
  /** The prim, variant or property spec holding it, where the edit leaves that spec (`/World/Lamp2{light=off}.lamp`). */
  readonly spec: string;
  /** The field of the spec that authors it. */
  readonly field: UnrepairedField;
  /** For `"metadata"`, the metadata field's name; otherwise `null`. */
  readonly metadata: string | null;
  /**
   * The path it names: the moved or deleted object or a path beneath it
   * (`/World/Lamp/Bulb`); for a path expression, a pattern's literal
   * prefix or an expression reference's prim, made absolute.
   */
  readonly path: string;
}

/**
 * The field that authors an {@link UnrepairedPath}: a prim or variant
 * spec's arcs, a relationship's `targets`, an attribute's `connections`,
 * a path expression in a `default` or `timeSamples`, or a `metadata`
 * field.
 */
export type UnrepairedField =
  | "references"
  | "payloads"
  | "inherits"
  | "specializes"
  | "targets"
  | "connections"
  | "default"
  | "timeSamples"
  | "metadata";

/** What {@link Stage.loadSupplied} read. */
export interface LoadResult {
  /** The layers read, package members included. */
  readonly loaded: readonly Layer[];
  /** The assets that failed to read, kept unresolved. */
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
  /**
   * The stable code, `composition.*` (such as `composition.unresolved_asset`,
   * or `composition.limit_exceeded` where composition reached one of its
   * {@link OpenOptions.limits}).
   */
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
  /** The assets the host has not supplied yet ({@link Stage.unavailable}). */
  readonly unavailable: readonly UnavailableAsset[];
  /** The assets that contribute nothing: missing, or failed to read. */
  readonly unresolved: readonly UnresolvedAsset[];
  /** Whether no asset is unavailable or unresolved. */
  readonly complete: boolean;
}

/** Options of {@link Layerstack.open}. */
export interface OpenOptions {
  /**
   * Include the winning source layer, spec path and field in native resolved
   * values for same-module extensions that check source correspondence
   * (`layerstack::StageOptions::with_provenance`): an extension reading
   * `Attribute::get` (or `read_with_provenance`) gets
   * `Resolved::provenance`, the layer and spec path of the strongest opinion
   * that gave the value and the field (the attribute's name); a schema
   * fallback has none. Defaults to `false`.
   * This package's own reads do not change: {@link Attribute.explain} names
   * the opinions behind a value either way. {@link Stage.provenance} reads it
   * back.
   *
   * It costs nothing measurable: the stage stores nothing more (it records
   * the source of a value as it resolves it, not at composition), and
   * opening, composing and reading every attribute of the demo sample and of
   * a public production scene (62,000 attribute reads) took the same time
   * and module memory with it on as off, within noise; natively, opening
   * and reading production scenes moved peak memory by under 0.3%.
   */
  readonly provenance?: boolean;
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
   *   With `sessionLayer`, that layer takes them (the default then).
   */
  readonly editLayer?: "root" | "session";
  /**
   * A session layer read from these bytes, the source of `identifier`, in
   * place of an anonymous one, as OpenUSD's `UsdStage::Open(rootLayer,
   * sessionLayer)` opens one from a file: the strongest layer of the
   * stage, which takes the edits ({@link Stage.editLayer}). The sublayers
   * it names are wanted as any dependency ({@link Stage.unavailable}) and
   * form the session layer stack; the asset paths it authors anchor to
   * its identifier, so {@link Stage.exportFlattened} anchors them; and
   * `save(layer)` writes it back, while `save()` leaves it alone.
   * `editLayer: "root"` with it is `argument.invalid`.
   */
  readonly sessionLayer?: { readonly identifier: string; readonly bytes: Uint8Array };
  /**
   * How much work composition may do (`layerstack::CompositionLimits`), for
   * the open and every edit after: each limit `undefined` for the module's
   * browser default (`layerstack_wasm::BROWSER_LIMITS`: 2 million prim index
   * nodes, 512 nodes in one index, composed depth 64), `null` for none, or a
   * non-negative integer. Scene description can compose without end;
   * reaching a limit stops composition and reports a
   * `composition.limit_exceeded` {@link Issue} at the prim where it stopped,
   * the stage composed up to it.
   */
  readonly limits?: {
    /** The most prim index nodes composition builds, over all prims. */
    readonly primIndexNodes?: number | null;
    /** The most nodes one prim's index holds. */
    readonly nodesPerPrimIndex?: number | null;
    /** How many levels below the deepest spec its layers author composition places a prim. */
    readonly composedDepth?: number | null;
  };
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
      /** A number, a `bigint` for `int64` and `uint64`, a string for `string`, `token`, `asset`, `pathExpression`, `pathText` (an `SdfPath` value as authored) and `unregistered` (metadata text kept unparsed). */
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
  /**
   * The target that writes where this opinion lives: its layer, through
   * the arc that reaches it, so an edit through it replaces this opinion
   * (`stage.apply(edits, opinion.editTarget)`). An opinion in a referenced
   * asset is edited in that asset, and every prim referencing it changes;
   * a shadowed opinion's edit does not change the value shown. `null`
   * without a composition node, or for a layer outside the node's stack
   * (a value clip's).
   */
  readonly editTarget: EditTarget | null;
}

/** Why an attribute has its value. */
export interface Explanation {
  /** The resolved value, exactly what {@link Attribute.get} reads. */
  readonly value: Value | null;
  /**
   * Where it comes from, as OpenUSD's resolve info names it: a blocked
   * sample held at the time is `timeSamples` with a `null` value and a
   * `block` opinion.
   */
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
 * What one recomposition changed, as prim paths, and the layers whose
 * muting changed. A listener registered with {@link Stage.onChange}
 * receives one per recomposition, in order.
 *
 * Re-read what a path covers: a `resynced` path's whole subtree may have
 * changed; a `changedInfoOnly` prim kept its children, but its fields and
 * properties may have changed; a `resyncedProperties` property may have
 * been added or removed, and nothing else of its prim changed for it.
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
  /**
   * For some `changedInfoOnly` prims, every property field that may have
   * changed: a listener whose dependencies are outside them can skip the
   * prim. A `changedInfoOnly` prim not listed here may have changed in any
   * field: re-read it.
   */
  readonly properties: readonly PrimPropertyChanges[];
  /**
   * Properties (`/Rock.visibility`) a property spec was added to or
   * removed from, on prims not in `resynced`: everything about such a
   * property may have changed, its existence included, and its prim's
   * other properties did not. Its prim is in `changedInfoOnly` only when
   * something else of it changed too. A listener caching a prim's
   * properties, or its property names, must read these too. As OpenUSD's
   * `UsdNotice::ObjectsChanged::GetResyncedPaths` lists property paths.
   */
  readonly resyncedProperties: readonly string[];
  /**
   * Layers muted since the changes heard before
   * ({@link Stage.muteLayer}, or natively through {@link Stage.withNative}),
   * as OpenUSD's `UsdNotice::LayerMutingChanged` names them. A muting is
   * heard once, on the last of the changes it is heard with, with the
   * prims it recomposed; muting a layer the stage does not use changes no
   * prim. Always empty in an {@link EditReceipt}'s changes: muting is not
   * an edit.
   */
  readonly mutedLayers: readonly Layer[];
  /** Layers unmuted since the changes heard before, as `mutedLayers`. */
  readonly unmutedLayers: readonly Layer[];
  /**
   * The identifiers of the layers muted since the changes heard before,
   * sorted, as `UsdNotice::LayerMutingChanged::GetMutedLayers` lists
   * them: those of `mutedLayers`, and of layers muted by identifier before
   * the stage read them, which have no {@link Layer}
   * ({@link Stage.mutedIdentifiers}). An anonymous layer has none.
   */
  readonly mutedIdentifiers: readonly string[];
  /** The identifiers of the layers unmuted since the changes heard before, as `mutedIdentifiers`. */
  readonly unmutedIdentifiers: readonly string[];
}

/** The property fields that may have changed on one `changedInfoOnly` prim, ordered by property, then field. */
export interface PrimPropertyChanges {
  /** The prim's path. */
  readonly prim: string;
  /** Its changed fields. */
  readonly fields: readonly PropertyChange[];
}

/** One property field an edit may have changed. */
export interface PropertyChange {
  /** The property's name. */
  readonly property: string;
  /**
   * The field: its default value, its animation (time samples and spline
   * together), its time samples, its targets (connections or relationship
   * targets), a metadata field, or `other` for a field this package does
   * not name.
   */
  readonly field: "default" | "animation" | "timeSamples" | "targets" | "metadata" | "other";
  /** The metadata field's name, for `field` `"metadata"`; `null` otherwise. */
  readonly metadata: string | null;
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
 * target layer, reading the stage as the defines before it in the same
 * `apply` leave it. Each is refused with `schema.cannot_edit` beneath an
 * instance or in a prototype, as OpenUSD refuses it.
 *
 * - `def`: nothing when the prim is defined and `typeName` is absent or
 *   its type already; otherwise authors `def` (and `typeName`) on its
 *   spec, and `def` on each ancestor that is not defined. Later
 *   schema-shaped edits of the same `apply` (such as
 *   {@link SetShaderInputEdit} on a shader defined here) see the prim and
 *   its type.
 * - `over`: nothing when the prim exists, on the stage or earlier in the
 *   same `apply` (a `def` stays a `def`); otherwise an `over` spec.
 * - `class`: nothing when the prim is a class or inside one; otherwise
 *   defines it as `def` does and authors `class`. Refused with
 *   `prim.not_class` for a defined prim that is not a class, as OpenUSD
 *   refuses it.
 *
 * A `typeName` given with `over` or `class` is then authored on the target
 * layer's spec, as `UsdPrim::SetTypeName` authors it.
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
 * an external arc (resolved against the target layer, as authored), whose
 * `primPath` names a prim in that asset; without one an internal arc, whose
 * `primPath` is a stage path the target maps into its layer, except a root
 * prim path (`/Library`), which is authored as given, as OpenUSD's
 * `_TranslatePath` does. Without `primPath` it targets the default prim. An
 * asset the stage has not read is unavailable ({@link EditReceipt.unavailable}): run
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
 * the selected variant needs that the stage has not read are unavailable
 * ({@link EditReceipt.unavailable}).
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
 * of them is applied atomically through one edit target by
 * {@link Stage.apply}. Paths are stage paths; times are stage time codes:
 * the target maps both into its layer.
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
  | ClearMetadataEdit
  | BindMaterialEdit
  | UnbindMaterialEdit
  | SetShaderInputEdit
  | ConnectToSourceEdit
  | DisconnectSourceEdit
  | SetBaseMaterialEdit
  | CreateGeomSubsetEdit
  | CreateMaterialSubsetEdit
  | SetSubsetFamilyTypeEdit
  | BindCoordSysEdit
  | BlockCoordSysEdit
  | ClearCoordSysEdit
  | SetSdrMetadataEdit;

/**
 * `UsdShadeMaterialBindingAPI::Apply(prim).Bind(material, strength,
 * purpose)`: applies `MaterialBindingAPI` to the prim and binds
 * `material`, directly (`material:binding[:<purpose>]`) or, with
 * `collection`, to the prims of that collection
 * (`material:binding:collection[:<purpose>]:<bindingName>`). A `strength`
 * authors `bindMaterialAs`; without one an authored
 * `strongerThanDescendants` is weakened, as OpenUSD's fallback strength
 * does.
 *
 * `material` is any prim on the stage or defined earlier in the same
 * edits, whatever its type, as OpenUSD's `Bind` binds any prim it is
 * given; one that is not a `Material` resolves no material
 * ({@link MaterialBinding.material} is `null`).
 *
 * Refused with `material.not_found` when no prim is at `material`, as
 * OpenUSD refuses it; `prim.not_found` when the prim to bind is not
 * there; `material.binding_name` for a binding name with a namespace or a
 * purpose that is not an identifier.
 */
export interface BindMaterialEdit {
  readonly type: "bindMaterial";
  /** The prim to bind. */
  readonly path: string;
  /** The material's path. */
  readonly material: string;
  /** The material purpose (`preview`, `full`); all purposes without one. */
  readonly purpose?: string;
  readonly strength?: BindingStrength;
  /** A collection's path (`/World.collection:metal`), for a collection binding. */
  readonly collection?: string;
  /** A collection binding's name; the collection's own name without one. */
  readonly bindingName?: string;
}

/**
 * `UsdShadeMaterialBindingAPI::UnbindDirectBinding`, or
 * `UnbindCollectionBinding` with `bindingName`: an explicit empty target
 * list, which blocks weaker and inherited bindings of the purpose.
 */
export interface UnbindMaterialEdit {
  readonly type: "unbindMaterial";
  readonly path: string;
  readonly purpose?: string;
  readonly bindingName?: string;
}

/**
 * `UsdShadeConnectableAPI::CreateInput(name, type).Set(value, time)`: the
 * input at `path` (`/Looks/Stone/Surface.inputs:roughness`) is created
 * with the value's type when it does not exist, and its default (or the
 * time sample at `time`) set. Works on any prim with inputs: a shader, a
 * node graph or a material's interface.
 *
 * Refused with `shading.type_mismatch` when the input exists with another
 * type, `prim.not_found` when its prim is not there, `path.invalid` for a
 * path that is not an `inputs:` property.
 */
export interface SetShaderInputEdit {
  readonly type: "setShaderInput";
  readonly path: string;
  readonly value: ValueInput;
  readonly time?: number;
}

/**
 * `UsdShadeConnectableAPI::ConnectToSource`: connects the input or output
 * at `path` to the input or output at `source`
 * (`/Looks/Stone/Texture.outputs:rgb`). A source that does not exist is
 * created with the port's type, as OpenUSD creates it. `position`:
 * `replace` (the default) makes it the only connection; `prepend` and
 * `append` add it to the list.
 *
 * A port that does not exist is created with `typeName`
 * (`/Looks/Stone.outputs:surface`, `token`); without one it is refused
 * with `shading.no_port`.
 */
export interface ConnectToSourceEdit {
  readonly type: "connectToSource";
  readonly path: string;
  readonly source: string;
  readonly typeName?: string;
  readonly position?: "replace" | "prepend" | "append";
}

/**
 * `UsdShadeConnectableAPI::DisconnectSource`: removes `source` from the
 * port's connections, or without one disconnects every source (an
 * explicit empty list, which hides weaker connections). Refused with
 * `shading.no_port` when the port does not exist.
 */
export interface DisconnectSourceEdit {
  readonly type: "disconnectSource";
  readonly path: string;
  readonly source?: string;
}

/**
 * `UsdShadeMaterial::SetBaseMaterial`: the material derives from `base`
 * through a specialize arc; `null` clears it (`ClearBaseMaterial`).
 * Refused with `material.not_material` when the prim at `path` is not a
 * `Material`, `prim.not_found` when no prim is there.
 */
export interface SetBaseMaterialEdit {
  readonly type: "setBaseMaterial";
  readonly path: string;
  readonly base: string | null;
}

/**
 * `UsdGeomSubset::CreateGeomSubset`: the `GeomSubset` child `name` of the
 * geometry at `path`, created or (one already there) re-authored, with
 * its element type, indices and family; a `familyType` is authored on the
 * geometry (`subsetFamily:<familyName>:familyType`).
 *
 * The indices are checked against the geometry's topology, where OpenUSD
 * authors any: `subset.indices` for one out of range, `subset.geometry`
 * for geometry that has no elements of the type, `subset.invalid_name`,
 * `subset.name_taken` for a prim of another type at the name.
 */
export interface CreateGeomSubsetEdit {
  readonly type: "createGeomSubset";
  readonly path: string;
  readonly name: string;
  readonly elementType: SubsetElementType;
  readonly indices: Int32Array | readonly number[];
  /** The family (`materialBind`); none without one. */
  readonly familyName?: string;
  readonly familyType?: SubsetFamilyType;
}

/**
 * `UsdShadeMaterialBindingAPI::CreateMaterialBindSubset`: a
 * {@link CreateGeomSubsetEdit} in the `materialBind` family, whose type
 * becomes `nonOverlapping` when it is unauthored or `unrestricted`. Bind
 * a material to the new subset with a {@link BindMaterialEdit} later in
 * the same edits.
 */
export interface CreateMaterialSubsetEdit {
  readonly type: "createMaterialSubset";
  readonly path: string;
  readonly name: string;
  readonly indices: Int32Array | readonly number[];
  /** `face` by default. */
  readonly elementType?: SubsetElementType;
}

/** `UsdGeomSubset::SetFamilyType`: the type of the geometry's subset family `familyName`. */
export interface SetSubsetFamilyTypeEdit {
  readonly type: "setSubsetFamilyType";
  readonly path: string;
  readonly familyName: string;
  readonly familyType: SubsetFamilyType;
}

/**
 * `UsdShadeCoordSysAPI::Apply(prim, name).Bind(target)`: applies the
 * `CoordSysAPI` instance `name` and binds it to `target`, the prim whose
 * space it names (`coordSys:<name>:binding`).
 */
export interface BindCoordSysEdit {
  readonly type: "bindCoordSys";
  readonly path: string;
  readonly name: string;
  readonly target: string;
}

/** `UsdShadeCoordSysAPI::BlockBinding`: an explicit empty target list, so the prim binds nothing for `name`. */
export interface BlockCoordSysEdit {
  readonly type: "blockCoordSys";
  readonly path: string;
  readonly name: string;
}

/**
 * `UsdShadeCoordSysAPI::ClearBinding(removeSpec)`: clears the binding's
 * targets in the target layer, or with `removeSpec` removes its spec.
 * Refused with `coord_sys.no_binding` when no such relationship exists.
 */
export interface ClearCoordSysEdit {
  readonly type: "clearCoordSys";
  readonly path: string;
  readonly name: string;
  readonly removeSpec?: boolean;
}

/**
 * `SetSdrMetadataByKey` on the shader at a prim path, or on the input or
 * output at a property path; `value: null` clears the key
 * (`ClearSdrMetadataByKey`), and every key when `key` is `""`. Refused
 * with `shading.not_shader` for a prim that is not a `Shader`,
 * `shading.no_port` for a port that does not exist.
 */
export interface SetSdrMetadataEdit {
  readonly type: "setSdrMetadata";
  readonly path: string;
  readonly key: string;
  readonly value: string | null;
}

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
  /**
   * Dirty layers a save of the whole stage left unwritten and dirty, as
   * OpenUSD's `UsdStage::Save` saves only the layers the stage uses: a
   * muted one, or one no longer composed, such as an asset edited through
   * its reference before the reference was removed. `save(layer)` writes
   * one. Empty for `save(layer)`.
   */
  readonly skipped: readonly SkippedLayer[];
  /** The bytes written, summed. */
  readonly bytes: number;
}

/** A dirty layer {@link Stage.save} left unwritten ({@link SaveResult.skipped}). */
export interface SkippedLayer {
  /** The layer, still dirty. */
  readonly layer: Layer;
  /** Why: the stage mutes it, or no longer composes it. */
  readonly reason: "muted" | "unused";
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
export interface UnpackagedAsset {
  /** The layer that authors it. */
  readonly layer: Layer;
  /** The asset path as authored. */
  readonly assetPath: string;
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
  readonly unresolved: readonly UnpackagedAsset[];
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

/** How a material binding holds against its prim's descendants' bindings: the `bindMaterialAs` token. */
export type BindingStrength = "weakerThanDescendants" | "strongerThanDescendants";

/**
 * The binding that decides a prim's material, as OpenUSD's
 * `UsdShadeMaterialBindingAPI::ComputeBoundMaterial` resolves it: the
 * prim's own binding or an ancestor's, direct or through a collection,
 * for the purpose asked for and then for all purposes.
 */
export interface MaterialBinding {
  /** The bound `Material`'s path; `null` when the winning binding targets no material (a missing prim, or one of another type). */
  readonly material: string | null;
  /** The path the winning binding targets, whatever is there. */
  readonly target: string;
  /** The binding relationship that decides it (`/World.material:binding`). */
  readonly relationship: string;
  /** A direct binding, or one through a collection. */
  readonly kind: "direct" | "collection";
  /** The collection a collection binding binds (`/World.collection:metal`); `null` for a direct one. */
  readonly collection: string | null;
  /** Its strength. */
  readonly strength: BindingStrength;
  /** The purpose it was found for: the one asked for, or `""` (all purposes) when that found nothing. */
  readonly purpose: string;
  /** Whether the prim authoring it lacks `MaterialBindingAPI`: a legacy binding, which OpenUSD still counts and warns about. */
  readonly withoutBindingApi: boolean;
}

/** The element type of a geometry subset (`elementType`). */
export type SubsetElementType = "face" | "point" | "edge" | "segment" | "tetrahedron";

/** The ownership contract of a geometry subset family (`subsetFamily:<family>:familyType`). */
export type SubsetFamilyType = "unrestricted" | "nonOverlapping" | "partition";

/** What a {@link SubsetProblem} found wrong with a subset family. */
export type SubsetProblemKind =
  | "invalidGeometry"
  | "elementTypeMismatch"
  | "invalidTopology"
  | "noIndices"
  | "oddPairCount"
  | "duplicateIndex"
  | "duplicatePair"
  | "invalidIndex"
  | "invalidPair"
  | "incompletePartition"
  | "other";

/** One violation of a subset family's contract, as OpenUSD's `UsdGeomSubset::ValidateFamily` reports it. */
export interface SubsetProblem {
  /** The family member, or the geometry for a family-wide problem. */
  readonly prim: string;
  /** The stage time it occurs at; `null` for the default time. */
  readonly time: number | null;
  /** What is wrong. */
  readonly kind: SubsetProblemKind;
  /** The index or `[a, b]` pair it names, else `null`. */
  readonly at: number | readonly [number, number] | null;
  /** The problem in words. */
  readonly message: string;
}

/** A `GeomSubset` child of a geometry prim. */
export interface GeomSubset {
  /** Its path. */
  readonly path: string;
  /** Its `elementType`; `null` when it resolves none. */
  readonly elementType: SubsetElementType | null;
  /** Its `familyName`; `null` when it resolves none. */
  readonly familyName: string | null;
  /** Its `indices` at the time read, an `int[]` array; `null` when none resolves. Release it when done. */
  readonly indices: ArrayData | null;
}

/** One `materialBind` face subset of a mesh and the material it is bound to. */
export interface MaterialSubset {
  /** The subset's path. */
  readonly path: string;
  /** Its face indices, an `int[]` array, unchecked against the mesh: see {@link MaterialSubsets.problems}. Release it when done. */
  readonly indices: ArrayData;
  /** Its bound material, inherited ones included; `null` when nothing binds it. */
  readonly binding: MaterialBinding | null;
}

/**
 * A mesh's material face subsets at one time, as a renderer draws them:
 * OpenUSD's `UsdShadeMaterialBindingAPI::GetMaterialBindSubsets`, each
 * subset's `ComputeBoundMaterial`, and `UsdGeomSubset::ValidateFamily`.
 */
export interface MaterialSubsets {
  /** The `materialBind` family's type, `unrestricted` when unauthored. */
  readonly familyType: string;
  /** The binding of the faces in no subset: the mesh's own; `null` when nothing binds it. */
  readonly fallback: MaterialBinding | null;
  /** The family's active face subsets whose indices resolve, in namespace order. */
  readonly subsets: readonly MaterialSubset[];
  /** The family's validation problems, which never hide a subset: a renderer decides what to do with an invalid family. */
  readonly problems: readonly SubsetProblem[];
}

/** A bound coordinate system, as OpenUSD's `UsdShadeCoordSysAPI::Binding` describes it. */
export interface CoordSysBinding {
  /** The `CoordSysAPI` instance name (`paint`). */
  readonly name: string;
  /** The `coordSys:<name>:binding` relationship that binds it. */
  readonly relationship: string;
  /** The prim (or property) whose space it names. */
  readonly target: string;
}

/** A material output terminal. */
export type MaterialTerminal = "surface" | "displacement" | "volume";

/**
 * The source of a material's terminal output, as OpenUSD's
 * `UsdShadeMaterial::ComputeSurfaceSource` finds it: node graphs pass
 * connections through, and an output on a shader ends them.
 */
export interface TerminalSource {
  /** The first source's prim when it is a `Shader`, else `null`. */
  readonly shader: string | null;
  /** The render context that supplied the sources (`""` universal); `null` when none did. */
  readonly context: string | null;
  /** Every source in connection order: the shader output, and the outputs the connection passed through, the material's first. */
  readonly sources: readonly { readonly output: string; readonly chain: readonly string[] }[];
}

/**
 * A connection source named by a path and checked against the stage:
 * {@link Stage.connectionSourceInfo}, OpenUSD's
 * `UsdShadeConnectionSourceInfo(stage, sourcePath)`.
 *
 * The path is read as `SdfPath` parses it. Text that is not a property
 * path (a prim path, a target path such as
 * `/Mat/Shader.outputs:out[/X]`, text that does not parse) gives an empty
 * info. Trailing variant selections are stripped from the prim path
 * (`/Mat/Shader{v=a}.outputs:out` names `/Mat/Shader`). Any prim on the
 * stage is a source prim, an `over`, an inactive prim and an instance
 * proxy included; a missing prim, one beneath an inactive prim, a relative
 * path and a variant selection within the path name none.
 *
 * One difference from OpenUSD: a relational attribute path
 * (`/Mat/Shader.outputs:out[/X].inputs:y`) gives an empty info, where
 * OpenUSD reads it as `inputs:y` of `/Mat/Shader`, a different attribute
 * from the one the path names.
 */
export interface ConnectionSourceInfo {
  /** The source prim's path; `null` when the stage has no prim there. */
  readonly prim: string | null;
  /** The attribute's name without its `inputs:` or `outputs:` namespace (`rgb`); the whole property name in neither namespace; empty when the path is not a property path. */
  readonly name: string;
  /** Whether the attribute is an input or an output; `null` for a name in neither namespace. */
  readonly kind: "input" | "output" | null;
  /** The attribute's declared type name (`color3f`), authored or from the prim's schemas; `null` when no attribute is at the path. */
  readonly typeName: string | null;
  /** The source's property path to connect to (`/Looks/Stone/Texture.outputs:rgb`), variant selections stripped; `null` unless `valid`. */
  readonly source: string | null;
  /** Whether it names a source to connect to: a kind, a name and a source prim (`IsValid`). The attribute need not exist yet. */
  readonly valid: boolean;
}

/** An attribute that produces a shading input's value, and the connections that lead to it. */
export interface ValueProducer {
  /** The input or output attribute. */
  readonly attribute: string;
  /** `shaderOutput` for an output a renderer evaluates; `authoredValue` for an attribute whose value it reads ({@link Stage.attribute}). */
  readonly kind: "shaderOutput" | "authoredValue";
  /** The attributes the connections passed through, the input asked about first. */
  readonly chain: readonly string[];
}

/** Where a shading input gets its value: OpenUSD's `UsdShadeUtils::GetValueProducingAttributes`. */
export interface ValueSources {
  /** The producers, depth first in connection order. */
  readonly sources: readonly ValueProducer[];
  /** Recoverable problems on a branch: a target that names no shading attribute, a cycle, a connection into a shader's input. */
  readonly issues: readonly { readonly kind: "invalidTarget" | "cycle" | "nonContainerInput" | "other"; readonly at: string | null }[];
}

/** A skinning method (`primvars:skel:skinningMethod`): linear blend or dual quaternion. */
export type SkinningMethod = "classicLinear" | "dualQuaternion";

/**
 * Joint influences to skin with: separate `indices` and `weights`, or
 * `interleaved` `[index, weight]` pairs (the form GPU skinning takes,
 * {@link SkinningQuery.interleavedInfluences}); `elementSize` influences
 * per point, or for the whole prim with `constant`. Indices address the
 * joint transforms given with them; weights are used as given.
 */
export type JointInfluencesInput =
  | {
      readonly indices: Int32Array;
      readonly weights: Float32Array;
      readonly elementSize: number;
      readonly constant?: boolean;
    }
  | { readonly interleaved: Float32Array; readonly elementSize: number; readonly constant?: boolean };

/** What {@link Layerstack.skinPoints} and its siblings skin with. Matrices are 16 numbers, row by row, as USD stores them. */
export interface SkinInput {
  /** `classicLinear` by default. */
  readonly method?: SkinningMethod;
  /** The geometry's bind transform (identity when unauthored). */
  readonly geomBindTransform: Float64Array | readonly number[];
  /** The joint palette, 16 numbers per joint in the influences' order ({@link SkinningQuery.skinningTransforms}). */
  readonly jointTransforms: Float64Array | readonly number[];
  readonly influences: JointInfluencesInput;
}

/** A skinning binding's joint palette at one time: what a GPU skins with, beside the influences. */
export interface SkinningTransforms {
  /** Inverse bind times animated skeleton-space transform per joint, in the binding's joint order: a `matrix4d[]` array. Release it when done. */
  readonly transforms: ArrayData;
  /** The geometry's bind transform, 16 numbers row by row. */
  readonly geomBindTransform: Float64Array;
  readonly method: SkinningMethod;
}

/**
 * What a stage's skinning cache has done since the stage opened, and what
 * it holds now ({@link Stage.skinningCache}). Counters are cumulative.
 */
export interface SkinningCacheStats {
  /** Deformed point or normal arrays reused without recomputing. */
  readonly hits: number;
  /** Deformed arrays recomputed, failed ones included. */
  readonly misses: number;
  /** Skeleton definition snapshots built. */
  readonly skeletonBuilds: number;
  /** Geometry binding snapshots built. */
  readonly bindingBuilds: number;
  /** Joint poses evaluated (one per skeleton and time, when needed). */
  readonly poseEvaluations: number;
  /** Joint influence arrays resolved and flattened. */
  readonly influenceResolutions: number;
  /** Points skinned. */
  readonly pointVertices: number;
  /** Normals skinned. */
  readonly normalVectors: number;
  /** Retained entries the stage's edits invalidated. */
  readonly invalidations: number;
  /** Times the stage's change history expired before a skinning read, so the cache started over. */
  readonly resets: number;
  /** Skeletons retained. */
  readonly skeletons: number;
  /** Geometry bindings retained. */
  readonly bindings: number;
  /** Bytes of retained arrays (matrices, influences, outputs) in use. */
  readonly usedBytes: number;
  /** Bytes of retained array capacity. */
  readonly capacityBytes: number;
}

/**
 * What a stage's transform cache has done and holds
 * ({@link Stage.transformCache}). Counters are cumulative since the stage
 * opened.
 */
export interface TransformCacheStats {
  /** Local transforms computed (not reused). */
  readonly localComputed: number;
  /** Local-to-world transforms computed (not reused). */
  readonly worldComputed: number;
  /** Queries answered from the cache. */
  readonly hits: number;
  /** Prims it holds transforms for now. */
  readonly prims: number;
  /** Change batches (edits, undos, loads, mutes) it heard and invalidated by. */
  readonly changeBatches: number;
  /** Times the stage's change history expired before a read, so the cache started over. */
  readonly resets: number;
}

/**
 * What a stage's bounds caches have done and hold
 * ({@link Stage.boundsCache}). Counters are cumulative since the stage
 * opened.
 */
export interface BoundsCacheStats {
  /** Prim bounds computed. */
  readonly computed: number;
  /** Prim bounds reused. */
  readonly hits: number;
  /** Prim bounds edits evicted. */
  readonly invalidated: number;
  /** Prim bounds held now. */
  readonly prims: number;
  /** Caches held: one per set of {@link BoundsOptions}, at most four. */
  readonly caches: number;
  /** Times the stage's change history expired before a read, so the caches started over. */
  readonly resets: number;
}

/** Why an op of `xformOpOrder` did not contribute as written; OpenUSD evaluates it as the identity (a huge scale for `singular`). */
export interface XformProblem {
  /** The `xformOpOrder` entry. */
  readonly op: string;
  /** No such attribute, a name of no op type, no value at the time, a value of another type, or an inverted singular matrix. */
  readonly kind: "noAttribute" | "unknownOpType" | "noValue" | "valueType" | "singular" | "other";
}

/** A prim's local transform at one time (`UsdGeomXformCache::GetLocalTransformation`). */
export interface LocalTransform {
  /** 16 numbers, row by row, in OpenUSD's row-vector convention (see {@link Stage.worldTransforms}); the identity for a prim that is not `Xformable`. */
  readonly matrix: Float64Array;
  /** Whether it resets the transform stack: it is also the prim's world transform. */
  readonly resetsXformStack: boolean;
  /** The ops that did not contribute as written. */
  readonly problems: readonly XformProblem[];
}

/** One op of a prim's `xformOpOrder` ({@link Xformable.ops}). */
export interface XformOp {
  /** The entry as `xformOpOrder` lists it (`!invert!xformOp:translate:pivot`). */
  readonly name: string;
  /** The attribute it reads (`xformOp:translate:pivot`). */
  readonly attribute: string;
  /** Its type (`translate`, `rotateXYZ`, `orient`, `transform`, ...); `null` for a name of no op type, which evaluates as the identity. */
  readonly type: string | null;
  /** Whether it applies its inverse. */
  readonly inverse: boolean;
}

/** A computed visibility: `invisible`, `inherited` (visible unless an ancestor is not), or for a purpose, `visible`. */
export type Visibility = "inherited" | "invisible" | "visible";

/** A prim's computed purpose (`UsdGeomImageable::ComputePurposeInfo`). */
export interface PurposeInfo {
  /** `default`, `render`, `proxy` or `guide`. */
  readonly purpose: string;
  /** Whether it was authored on the prim or an ancestor, and so is inherited by descendants that author none; a fallback is not. */
  readonly inheritable: boolean;
  /** The prim whose authored `purpose` it is; `null` for a fallback. */
  readonly authoredOn: string | null;
}

/**
 * The options of a bounds query: the cache it reads
 * (`UsdGeomBBoxCache(time, includedPurposes, useExtentsHint, ignoreVisibility)`).
 * The stage keeps a cache for each combination it is asked for, at most
 * four, each hearing the stage's edits.
 */
export interface BoundsOptions {
  /** The time code; the default time without one. */
  readonly time?: number;
  /** The purposes whose geometry counts; `["default"]` without them. */
  readonly purposes?: readonly string[];
  /** Whether models' authored `extentsHint` stands for their subtrees. */
  readonly useExtentsHint?: boolean;
  /** Whether invisible prims count too. A queried prim always counts. */
  readonly ignoreVisibility?: boolean;
}

/**
 * Oriented bounding boxes, one per queried prim, in order: each is a range
 * transformed by a matrix, as OpenUSD's `GfBBox3d`.
 *
 * An empty box (no geometry of the included purposes) has a range whose
 * minimum is above its maximum (`Number.MAX_VALUE` and `-Number.MAX_VALUE`),
 * as `GfRange3d()` is.
 */
export interface BoundingBoxes {
  /** The number of boxes. */
  readonly count: number;
  /** Each box's range before its matrix: 6 numbers, `[minX, minY, minZ, maxX, maxY, maxZ]`. */
  readonly ranges: Float64Array;
  /** Each box's matrix: 16 numbers, row by row (`GfBBox3d::GetMatrix`). */
  readonly matrices: Float64Array;
  /** Each box's axis-aligned range after its matrix, 6 numbers as in `ranges` (`GfBBox3d::ComputeAlignedRange`): what to frame a camera on. */
  readonly alignedRanges: Float64Array;
}

/** The options of {@link Stage.pointInstances} (`UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`). */
export interface InstanceOptions {
  /** The time code the transforms are at; the default time without one. */
  readonly time?: number;
  /** The time that fixes topology and the mask and anchors velocities; `time` without one. */
  readonly baseTime?: number;
  /** Whether each prototype's own local transform applies first (`IncludeProtoXform`); true by default. */
  readonly includePrototypeTransform?: boolean;
  /** Whether inactive and invisible ids are left out (`ApplyMask`); true by default. */
  readonly applyMask?: boolean;
}

/** A point instancer's instances at one time ({@link Stage.pointInstances}). */
export interface PointInstances {
  /** The number of instances. */
  readonly count: number;
  /** The `prototypes` targets, which prototype indices address. */
  readonly prototypes: readonly string[];
  /** Each instance's transform in the instancer's space: 16 numbers, row by row. */
  readonly matrices: Float64Array;
  /** Each instance's prototype index. */
  readonly prototypeIndices: Int32Array;
  /** Each instance's id: its `ids` entry, or its position without `ids`. */
  readonly ids: BigInt64Array;
  /** Each instance's position in the authored arrays (before masking). */
  readonly indices: Int32Array;
}
