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
 * An asset the stage, or a layer read since it last composed, asks for
 * that the host has not supplied yet: it is unavailable, and may still
 * arrive. Supply its bytes under `identifier` ({@link Stage.supply}), or
 * `null` if it does not exist, then call {@link Stage.loadSupplied} (or
 * {@link Stage.readSupplied}, composing once at the end); or let
 * {@link Stage.resolve} run the loop.
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
   * asset's opinions, until it loads. None for one only a layer read since
   * the stage last composed asks for ({@link Stage.readSupplied}).
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

/** What {@link Stage.loadSupplied} or {@link Stage.readSupplied} read, and where its work went. */
export interface LoadResult {
  /** The layers read, package members included. */
  readonly loaded: readonly Layer[];
  /** The assets that failed to read, kept unresolved. */
  readonly failures: readonly AssetFailure[];
  /** Each layer read, in the order the reads began. */
  readonly reads: readonly LayerRead[];
  /** Each composition after a batch of reads, in order. */
  readonly compositions: readonly Composition[];
}

/**
 * One layer {@link Stage.loadSupplied} read: decoding the bytes supplied
 * for it into specs. Supplying them is not part of it.
 *
 * A read resolves the layer's arcs as it goes, and reads a dependency
 * already supplied before it returns, so reads nest: `time.start` to
 * `time.end` covers the nested reads, `time.own` does not. The `own`
 * times of a load's reads add up to the time it spent reading.
 *
 * A USDZ package's first read covers the members it reads with its root
 * layer: its bytes are the archive's, its prims the root layer's. A member
 * read later, through an arc into the package, is a read of its own.
 */
export interface LayerRead {
  /** The layer read; `null` for a read that failed (its asset is among the failures). */
  readonly layer: Layer | null;
  /** The identifier read, as {@link Layer.identifier} gives it. */
  readonly identifier: string | null;
  /** The encoded bytes decoded. */
  readonly bytes: number;
  /** The layer's prim specs outside variant branches, not counting the pseudo-root's; 0 for a read that failed. */
  readonly prims: number;
  /** When it ran; `null` when the module has no clock. */
  readonly time: ReadTime | null;
}

/**
 * When a {@link LayerRead} ran, in milliseconds on `performance.now()`'s
 * timeline, so it lines up with the host's own marks. A browser may coarsen
 * that clock (to 100 µs without cross-origin isolation), so a small layer's
 * read can measure 0: its bytes and prims count its work regardless.
 */
export interface ReadTime {
  /** When the read began. */
  readonly start: number;
  /** When it ended, the reads nested in it included. */
  readonly end: number;
  /** `end - start` less the reads nested in it: this layer's own decoding, in milliseconds. */
  readonly own: number;
}

/**
 * One composition {@link Stage.loadSupplied} ran after a batch of reads:
 * the layers read entering the stage, and the stage recomposing the prims
 * that use them. A batch whose assets were all still unavailable composes
 * too, adding none.
 */
export interface Composition {
  /** The layers the batch added. */
  readonly layers: number;
  /** When it ran, in `performance.now()` milliseconds; `null` when the module has no clock. */
  readonly time: { readonly start: number; readonly end: number } | null;
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

/** What a value clip asset is to its clip set. */
export type ClipRole = "value" | "manifest" | "templateCandidate";

/**
 * Why a clip query needs an asset; `""` for a reason this module names
 * that the package does not.
 */
export type ClipDemandReason =
  | "manifest"
  | "automaticManifest"
  | "activeInterval"
  | "lowerInterpolationBound"
  | "upperInterpolationBound"
  | "templateSchedule"
  | "missingValueSearch"
  | "strongerSourceComposition"
  | "singleClipSamples"
  | "";

/**
 * A value clip asset a clip query needs, with why and whether the stage
 * has it: `loaded`, `missing` (confirmed absent: the clip reads as a
 * gap), `unanswered` (the host has not supplied it) or `notResident`.
 */
export interface ClipDemand {
  /** The asset path as authored, or as a template derives it. */
  readonly assetPath: string;
  /** The layer authoring the clip metadata, which `assetPath` is anchored to. */
  readonly anchor: Layer;
  /** What the asset is to its clip set. */
  readonly role: ClipRole;
  /** Why the query needs it. */
  readonly reason: ClipDemandReason;
  /** Whether the stage has it. */
  readonly status: "loaded" | "missing" | "unanswered" | "notResident";
  /** The prim whose clip set names it. */
  readonly owner: string;
  /** The clip set's name. */
  readonly clipSet: string;
}

/** A value clip asset a clip query needs that the host has not supplied yet: it may still arrive. */
export interface UnavailableClipAsset {
  /** The key to supply bytes under ({@link Stage.supply}), as {@link UnavailableAsset.identifier}. */
  readonly identifier: string;
  /** The asset path as authored, or as a template derives it. */
  readonly assetPath: string;
  /** The layer authoring the clip metadata. */
  readonly anchor: Layer;
  /** What the asset is to its clip set. */
  readonly role: ClipRole;
  /** The prim whose clip set names it. */
  readonly owner: string;
  /** The clip set's name. */
  readonly clipSet: string;
}

/**
 * A clip set's preparation issue: its metadata is invalid or
 * incompatible (`invalidMetadata`), its template range or pattern
 * (`invalidTemplate`), a template candidate is unresolved
 * (`unresolvedTemplate`), a clip spline is unsupported
 * (`unsupportedSpline`), its schedule is malformed (`invalidSchedule`),
 * or an asset path expression failed (`assetExpression`); `""` for one
 * the package does not name.
 */
export interface ClipIssue {
  /** The prim whose clip set it is. */
  readonly owner: string;
  /** The clip set's name. */
  readonly clipSet: string;
  /** The issue. */
  readonly code: string;
}

/** What {@link Stage.prepareClipQuery} planned and read. */
export interface ClipPreparation {
  /**
   * Whether every demand is loaded or confirmed missing, nothing failed,
   * no clip set has an issue and no budget ran out. Once nothing is
   * {@link ClipPreparation.unavailable}, the attribute's clip values, and
   * whether it might vary with time, are OpenUSD's, an issue's set
   * included: OpenUSD rejects the set too.
   */
  readonly complete: boolean;
  /** The assets the query needs, loaded or not. */
  readonly demands: readonly ClipDemand[];
  /** The demanded assets to supply before preparing again. */
  readonly unavailable: readonly UnavailableClipAsset[];
  /** The supplied assets that failed to read; the query stays incomplete. */
  readonly failures: readonly AssetFailure[];
  /** The clip sets' preparation issues. */
  readonly issues: readonly ClipIssue[];
  /** The clip evaluator's error code (such as `invalidActivation`), or `null`. */
  readonly error: string | null;
  /** Whether `maxLoads` or `maxDemands` ran out: prepare again, or with more. */
  readonly budgetExhausted: boolean;
  /** The reads tried, unsupplied and absent assets included. */
  readonly loadAttempts: number;
}

/** Options of {@link Stage.prepareClipQuery}. */
export interface ClipQueryOptions {
  /** The time the query reads; the default time without one, which plans what untimed introspection needs. */
  readonly time?: number;
  /** The most assets read; 64 by default. */
  readonly maxLoads?: number;
  /** The most demands planned; 256 by default. */
  readonly maxDemands?: number;
}

/** Options of {@link Stage.resolveClipQuery}. */
export interface ResolveClipQueryOptions extends ClipQueryOptions, ResolveOptions {}

/** What {@link Stage.resolveClipQuery} did: the last preparation, and its rounds. */
export interface ResolveClipQueryResult extends ClipPreparation {
  /** The rounds of prepare, fetch and supply. */
  readonly rounds: number;
  /** The identifiers fetched. */
  readonly fetched: number;
  /** The bytes supplied. */
  readonly bytes: number;
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
/**
 * A concrete typed schema a stage registers when it opens, besides
 * OpenUSD's: `name` is `base` (OpenUSD's, or one listed before it), with
 * `base`'s properties and none of its own, as a codeless schema whose
 * `generatedSchema.usda` declares `class "Name" (inherits = </Base>)` and
 * nothing else. A prim of type `name` is `base` to every `IsA` read
 * ({@link Prim.isA}), so it draws, bounds and binds as `base` does.
 */
export interface TypedSchema {
  /** The schema's name: an identifier no registered schema has. */
  readonly name: string;
  /** The typed schema it derives from (`Mesh`, `Xform`). */
  readonly base: string;
}

/**
 * One payload load rule, as OpenUSD's `UsdStageLoadRules` holds it: the
 * payloads at `path` and beneath it are included (`all`), only those at
 * `path` (`only`: its descendants' are not, unless a deeper rule includes
 * them), or none (`none`). The rule of the nearest path at or above a
 * prim decides, and loading a prim also loads the payloads of the
 * ancestors it needs ({@link Stage.loadRules}).
 */
export interface LoadRule {
  /** An absolute prim path (`/World/Rock`, `/` for every prim). */
  readonly path: string;
  /** `all`, `only` or `none`: OpenUSD's `AllRule`, `OnlyRule`, `NoneRule`. */
  readonly rule: "all" | "only" | "none";
}

/**
 * Which payloads a stage includes: `"all"` (no rule: OpenUSD's
 * `UsdStage::LoadAll`), `"none"` (`[{ path: "/", rule: "none" }]`:
 * `UsdStage::LoadNone`), or the literal rules.
 */
export type LoadRules = "all" | "none" | readonly LoadRule[];

/**
 * Whether loading a prim's payloads also loads those of its descendants
 * (OpenUSD's `UsdLoadWithDescendants`, the default) or not
 * (`UsdLoadWithoutDescendants`).
 */
export type LoadPolicy = "withDescendants" | "withoutDescendants";

/**
 * What {@link Stage.load}, {@link Stage.unload} or
 * {@link Stage.loadAndUnload} did: whether the load rules changed, and the
 * paths it left out, each with why, as OpenUSD's `UsdStage::LoadAndUnload`
 * skips them with an error and applies the rest:
 *
 * - `inactive`: a path to load whose nearest prim on the stage is inactive.
 * - `not_on_stage`: a path to load with no ancestor on the stage.
 * - `prototype`: a path in an instance prototype (its root prim named
 *   `__Prototype_...`), to load or unload; load it through its instance.
 */
export interface LoadOutcome {
  /** Whether the load rules changed. */
  readonly changed: boolean;
  /** The paths left out, loads before unloads, each in the order given. */
  readonly refused: readonly { readonly path: string; readonly code: "inactive" | "not_on_stage" | "prototype" }[];
}

/**
 * What {@link Stage.reload} and {@link Stage.reloadLayers} read and
 * published: the {@link LoadResult} of the reads, whose `loaded` lists
 * every layer read, those newly reached included, and the layers read
 * again from their sources.
 */
export interface ReloadResult extends LoadResult {
  /**
   * The layers read again from their sources, package members included:
   * those whose source changed (its version, {@link Stage.supply}), the
   * dirty ones discarded, and those forced. Empty when nothing changed:
   * then nothing was read and nothing is heard.
   */
  readonly reloaded: readonly Layer[];
}

/** How {@link Stage.reload} treats layers with edits not saved. */
export interface ReloadOptions {
  /**
   * Replace dirty layers with their sources, losing their edits, as
   * OpenUSD's `UsdStage::Reload` always does. When `false`, the default, a
   * dirty layer the reload would read refuses the reload
   * (`io.dirty_reload`), with nothing changed.
   */
  readonly discardDirty?: boolean;
}

/** How {@link Stage.reloadLayers} reads its layers. */
export interface LayerReloadOptions extends ReloadOptions {
  /**
   * Read every layer even when its source is unchanged, as OpenUSD's
   * `SdfLayer::Reload(force)`; each is heard as read again.
   */
  readonly force?: boolean;
}

/**
 * What {@link Stage.releaseUnusedLayers} released and kept.
 */
export interface ReleasedLayers {
  /** The layers released, by ascending id. */
  readonly released: readonly ReleasedLayer[];
  /**
   * The layers nothing uses that were kept because they hold edits not
   * saved, anonymous layers among them: save them ({@link Stage.save}) or
   * discard their edits, and a later release frees them. OpenUSD frees
   * them and loses the edits.
   */
  readonly keptDirty: readonly Layer[];
  /** The bytes the released layers were read from, summed over those known. */
  readonly sourceBytes: number;
}

/** One layer {@link Stage.releaseUnusedLayers} released. */
export interface ReleasedLayer {
  /** The identifier it was read from: the key to supply again when the stage next asks for it. */
  readonly identifier: string;
  /** The bytes it was read from, or `null` when unknown. The memory it held is larger: decoded specs, values and indexes. */
  readonly sourceBytes: number | null;
  /** Whether the stage mutes it: unmuting it asks for it again. */
  readonly muted: boolean;
}

export interface OpenOptions {
  /**
   * Which payloads the stage includes ({@link LoadRules}): `"all"` (the
   * default), `"none"`, or rules. Opening a large scene with `"none"`
   * composes everything but its payloads, which a viewer then loads
   * where it needs them ({@link Stage.load}), as OpenUSD's
   * `UsdStage::Open(..., UsdStage::LoadNone)` does: while the rules leave
   * any payload out, the stage asks for no payload's asset
   * ({@link Stage.unavailable}) until a rule includes it.
   */
  readonly load?: LoadRules;
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
   * Typed schemas to register besides OpenUSD's, each derived from a
   * registered one ({@link TypedSchema}). A name that is not an
   * identifier or is registered already, or a base that is not a
   * registered typed schema, is `argument.invalid`.
   */
  readonly schemas?: readonly TypedSchema[];
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

/**
 * The samples bracketing a stage time ({@link Attribute.bracketingSamples}),
 * the three answers of OpenUSD's `UsdAttribute::GetBracketingTimeSamples`:
 *
 * - `samples`: the nearest composed sample at or below the time and the
 *   nearest at or above it; both the time itself on a sample, and both the
 *   first (last) sample before the first (after the last);
 * - `noSamples`: a value without time samples (a default, a schema
 *   fallback, or nothing authored), OpenUSD's success without
 *   `hasTimeSamples`;
 * - `noValue`: no sample or default supplies one (a block, an animation
 *   block or a spline ends resolution first, as OpenUSD's call fails for
 *   splines too), a NaN time, or no attribute at the path.
 */
export type BracketingSamples =
  | { readonly kind: "samples"; readonly lower: number; readonly upper: number }
  | { readonly kind: "noSamples" }
  | { readonly kind: "noValue" };

/**
 * An interval of stage time, as OpenUSD's `GfInterval`: an absent end is
 * infinite, and each end is closed unless `minClosed` or `maxClosed` is
 * `false`. A NaN end or a reversed interval holds no time.
 */
export interface TimeInterval {
  /** The lower end; `-Infinity` when absent. */
  readonly min?: number;
  /** The upper end; `Infinity` when absent. */
  readonly max?: number;
  /** Whether `min` itself is inside; `true` when absent. */
  readonly minClosed?: boolean;
  /** Whether `max` itself is inside; `true` when absent. */
  readonly maxClosed?: boolean;
}

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

/**
 * A schema a stage registers ({@link Stage.schemas}), as OpenUSD's
 * `UsdSchemaRegistry` knows it.
 */
export interface SchemaInfo {
  /** Its name: `Sphere`, `CollectionAPI`. */
  readonly name: string;
  /**
   * A typed schema a prim's `typeName` may name (`concreteTyped`) or only
   * other typed schemas inherit (`abstractTyped`); an applied schema
   * applied once (`singleApplyApi`) or once per instance name, as
   * `Name:instance` (`multipleApplyApi`).
   */
  readonly kind: "concreteTyped" | "abstractTyped" | "singleApplyApi" | "multipleApplyApi";
  /** A typed schema's ancestors, nearest first (`Gprim`, `Boundable`, ..., `Typed`); empty for an applied schema. */
  readonly bases: readonly string[];
  /** The typed or applied schemas a prim must be or have for this applied schema to be applied to it; empty when any. */
  readonly canOnlyApplyTo: readonly string[];
  /** A multiple-apply schema's only instance names; empty when any valid name. */
  readonly allowedInstanceNames: readonly string[];
}

/**
 * The role of a {@link ValueTypeInfo}: what its values mean beyond their
 * storage, as OpenUSD's `SdfValueRoleNames` spells it. `Transform`,
 * `PointIndex`, `EdgeIndex` and `FaceIndex` are the legacy types' roles.
 * AOUSD Core §6.5 (semantic roles).
 */
export type ValueRole =
  | "Color"
  | "Frame"
  | "Group"
  | "Normal"
  | "Point"
  | "TextureCoordinate"
  | "Vector"
  | "Transform"
  | "PointIndex"
  | "EdgeIndex"
  | "FaceIndex";

/**
 * A value type name an attribute may declare ({@link CreateAttributeEdit}
 * `typeName`), from {@link Layerstack.valueTypes}: OpenUSD's
 * `SdfValueTypeName`, as `Sdf.ValueTypeNames` lists them. AOUSD Core §6.2
 * (value types), §6.3 (dimensioned types), §7.6.4.1.1 (`typeName`).
 */
export interface ValueTypeInfo {
  /** The name an attribute declares: `color3f`, `color3f[]`. */
  readonly name: string;
  /** Whether it is an array type (`color3f[]`), `IsArray`. */
  readonly isArray: boolean;
  /** The scalar type: itself for a scalar type, `color3f` for `color3f[]` (`GetScalarType`). */
  readonly scalarType: string;
  /** The array type: itself for an array type, `color3f[]` for `color3f`; `null` for `opaque` and `group`, which have none (`GetArrayType`). */
  readonly arrayType: string | null;
  /** Its role (`Color` for `color3f`), or `null` for a type without one (`GetRole`). */
  readonly role: ValueRole | null;
  /**
   * The type without a role whose values it holds, its role changing
   * what they mean, not how they are stored: `float3` for `color3f`, `float3[]` for `color3f[]`,
   * `matrix4d` for `frame4d`, `opaque` for `group`; itself for a type
   * without a role (`GetType`).
   */
  readonly storage: string;
  /** The shape of one value, or of each element of an array: `[]` for one component (`float`, `token`), `[3]` for `float3` or `color3f`, `[4]` for a quaternion, `[4, 4]` for `matrix4d` (`GetDimensions`). */
  readonly dimensions: readonly number[];
  /**
   * One of the legacy types OpenUSD still registers with no current
   * name (`Transform`, `PointIndex`, `EdgeIndex`, `FaceIndex`, and their
   * arrays): read and authored as any other, but not what new scene
   * description declares, so a picker may leave them out.
   */
  readonly legacy: boolean;
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
 * What one recomposition changed, as paths, and the layers whose muting
 * changed. A listener registered with {@link Stage.onChange} receives one
 * per recomposition, in order. As OpenUSD's `UsdNotice::ObjectsChanged`,
 * it tells prims from properties and resyncs from info-only changes, with
 * the fields an info-only change names, so a listener can tell "this
 * prim's own metadata changed, and these of its properties" apart from
 * "only these properties changed".
 *
 * Re-read what a path covers: a `resynced` path's whole subtree may have
 * changed; a `resyncedProperties` property may have been added or
 * removed, and nothing else of its prim changed for it; a
 * `changedInfoOnly` prim's own information changed (the fields in
 * `primFields`), not its children or properties; a `changedProperties`
 * entry names one field of one property, and its prim's own information
 * and other properties did not change for it. Info-only paths never lie
 * beneath a resync.
 *
 * A prim created or removed is resynced alone: its parent, whose child
 * list gained or lost it, is not reported unless its own opinions changed
 * too, as OpenUSD's `UsdNotice::ObjectsChanged` does not report it. A
 * listener caching a prim's children updates them from its children's
 * resyncs. Reordering children (`primOrder`) resyncs the parent; a spec
 * added to or removed from a weaker layer can move a child too, and
 * resyncs that child.
 *
 * An inert spec (an `over` with no opinions of its own) added or removed
 * for a prim that exists before and after changes that prim, and each
 * prim composing from it through an arc, info-only with no field, as
 * OpenUSD reports it; a property spec in it is in `resyncedProperties`. A
 * change to stage metadata that composes nothing (`documentation`,
 * `startTimeCode`, `metersPerUnit` and the like) reports `/` info-only
 * with that field. A layer read again ({@link Stage.reload}) reports `/`,
 * for a layer of the stage's own layer stack, or the prims composing the
 * layer's root prims, info-only with no field, beside what it changed.
 *
 * A sublayer inserted or removed reports the specs it brings or takes, in
 * whichever layer stack it joins or leaves: the prims it defines, or whose
 * arcs it changes, are resynced; those it only overrides are info-only,
 * with the properties it holds for them in `resyncedProperties`; `/` is
 * info-only, with `subLayers` and `subLayerOffsets`, when the stage's own
 * layer stack changed, and otherwise each prim composing from the edited
 * layer's layer stack, with those fields. Reordering a layer's sublayers counts as removing
 * and inserting every one of them, as OpenUSD announces it. Three
 * differences from OpenUSD are deliberate: a sublayer that moves a child
 * among its siblings resyncs that child, where OpenUSD reports it
 * info-only and keeps a stale child order (OpenUSD bug B037); inserting a
 * muted sublayer reports `/` alone, where OpenUSD resyncs the prims it
 * defines, which compose neither before nor after (OpenUSD bug B038); and
 * for a layer a reference reaches that is a sublayer too, a reorder of
 * its sublayer list resyncs the referencing prims as well, and its
 * removal does not resync them, where OpenUSD does though they compose
 * the same.
 */
export interface Changes {
  /** Prims that exist now and did not before. */
  readonly created: readonly string[];
  /** Prims that existed before and do not now. */
  readonly removed: readonly string[];
  /** Roots of subtrees whose composed structure may have changed. `/` when the history was lost: re-read everything. */
  readonly resynced: readonly string[];
  /**
   * Properties (`/Rock.visibility`) a property spec was added to or
   * removed from, on prims not in `resynced`: everything about such a
   * property may have changed, its existence included, and its prim's
   * other properties did not. A listener caching a prim's properties, or
   * its property names, must read these too. As OpenUSD's
   * `UsdNotice::ObjectsChanged::GetResyncedPaths` lists property paths.
   */
  readonly resyncedProperties: readonly string[];
  /**
   * Surviving prims whose own information changed without a resync: their
   * metadata, or their spec stacks. As the prim paths of OpenUSD's
   * `UsdNotice::ObjectsChanged::GetChangedInfoOnlyPaths`.
   */
  readonly changedInfoOnly: readonly string[];
  /**
   * The fields changed of the `changedInfoOnly` prims, by prim, then
   * field, as OpenUSD's `GetChangedFields` names them. A prim listed with
   * none changed its spec stack, or was read again.
   */
  readonly primFields: readonly PrimFieldChange[];
  /**
   * Properties changed info-only, one entry per property and field, by
   * prim, then property name, then field. As the property paths of
   * OpenUSD's `GetChangedInfoOnlyPaths`, with their `GetChangedFields`.
   */
  readonly changedProperties: readonly PropertyFieldChange[];
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

/** One changed field of a prim's own information. */
export interface PrimFieldChange {
  /** The prim's path. */
  readonly prim: string;
  /** The field, as OpenUSD names it: `documentation`, `subLayers`, a metadata field's name. */
  readonly field: string;
}

/** One property field an edit may have changed. */
export interface PropertyFieldChange {
  /** The property's path (`/Rock.size`). */
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

/**
 * A sublayer a layer authors ({@link Stage.sublayers}): its asset path
 * as authored, and the offset its layer is read at. AOUSD Core §7.6.1
 * (`subLayers`), §12.3.2.1 (layer offsets).
 */
export interface Sublayer {
  /** The asset path as authored (`./shot.usda`), anchored at the authoring layer. */
  readonly asset: string;
  readonly layerOffset: { readonly offset: number; readonly scale: number };
}

/**
 * What one layer authors at one path ({@link Stage.layerSpec}): the spec
 * there, as OpenUSD's `SdfLayer::GetPseudoRoot`, `GetPrimAtPath` and
 * `GetPropertyAtPath` find it. A union on `type`:
 *
 * - `pseudoRoot`: the layer's own spec at `/`, holding its metadata,
 *   sublayers and root prims ({@link PseudoRootSpec});
 * - `prim`: a prim spec, outside variants or inside them
 *   (`/World/Robot{finish=painted}Decal`); `variant`: a variant spec
 *   (`/World/Robot{finish=painted}`), which holds what a prim spec does
 *   but a specifier and a type ({@link PrimSpec});
 * - `attribute` ({@link AttributeSpec}) and `relationship`
 *   ({@link RelationshipSpec}): a property spec.
 *
 * Every kind has its `path` in the layer's namespace and the `fields` it
 * authors. A spec is a snapshot: read it again after an edit.
 */
export type LayerSpec = PseudoRootSpec | PrimSpec | AttributeSpec | RelationshipSpec;

/** What every {@link LayerSpec} has. */
export interface SpecBase {
  /** The spec's path in its layer, as the layer spells it (`/World/Robot{finish=painted}Decal.size`). */
  readonly path: string;
  /**
   * The fields the spec authors, by their OpenUSD keys (`SdfFieldKeys`,
   * the names {@link Changes.primFields} uses), sorted: `specifier`,
   * `typeName`, `kind`, `active`, `references`, `payload`, `inheritPaths`,
   * `variantSelection`, `apiSchemas`, `documentation`, `default`,
   * `timeSamples`, `targetPaths`, `upAxis`, `subLayers`, ... A field is
   * listed when the layer holds an opinion for it, an emptied list op
   * included (`references` with no items, as OpenUSD keeps one). The
   * children lists (`primChildren`, `properties`, `variantSetChildren`)
   * are not fields here: read them from the spec's own members. `custom`
   * and `variability` are listed only when they are not their fallbacks
   * (`custom`, `uniform`), as USDA writes them.
   *
   * Most fields an editor acts on are members of the spec as well; any
   * other's value is read composed ({@link Prim.metadata},
   * {@link Stage.metadata}), or as an opinion ({@link Attribute.explain}).
   */
  readonly fields: readonly string[];
}

/**
 * The pseudo-root of a layer: its metadata, sublayers and root prims.
 * AOUSD Core §7.6.1 (layer fields), §7.6.1.2.3 (`defaultPrim`).
 */
export interface PseudoRootSpec extends SpecBase {
  readonly type: "pseudoRoot";
  /** `defaultPrim` as authored (`World`); `null` when the layer authors none. */
  readonly defaultPrim: string | null;
  /** `documentation` (`doc` in USDA); `null` when the layer authors none. */
  readonly documentation: string | null;
  /** Its sublayers, strongest first, as {@link Stage.sublayers} reads them. */
  readonly sublayers: readonly Sublayer[];
  /** Its root prims' names, in the order the layer holds them (`primChildren`). */
  readonly primChildren: readonly string[];
}

/**
 * A prim spec (`type` `"prim"`), or a variant spec (`"variant"`), which
 * holds the same fields but `specifier` and `typeName`. A member is
 * `null` when the spec does not author its field. AOUSD Core §7.6.2
 * (prim fields), §7.6.7 (variant specs), §7.3.6 (variant set specs).
 */
export interface PrimSpec extends SpecBase {
  readonly type: "prim" | "variant";
  /** `specifier`; `null` for a variant. */
  readonly specifier: "def" | "over" | "class" | null;
  /** `typeName` (`Xform`); `null` for a variant. */
  readonly typeName: string | null;
  /** `kind` (`component`). */
  readonly kind: string | null;
  /** `active`. */
  readonly active: boolean | null;
  /** `instanceable`. */
  readonly instanceable: boolean | null;
  /** `documentation` (`doc` in USDA). */
  readonly documentation: string | null;
  /** `references`, each item as authored. */
  readonly references: ListOp<AuthoredReference> | null;
  /** `payload`, each item as authored. */
  readonly payloads: ListOp<AuthoredReference> | null;
  /** `inheritPaths`: prim paths in this layer's namespace. */
  readonly inherits: ListOp<string> | null;
  /** `specializes`: prim paths in this layer's namespace. */
  readonly specializes: ListOp<string> | null;
  /** `apiSchemas` (`CollectionAPI:parts`). */
  readonly apiSchemas: ListOp<string> | null;
  /** `variantSetNames` (`variantSets` in USDA): the variant sets it says the prim has. AOUSD Core §7.6.2.3.5. */
  readonly variantSetNames: ListOp<string> | null;
  /** `variantSelection`: each variant set's selection, by set name. AOUSD Core §7.6.2.3.4. */
  readonly variantSelections: ReadonlyMap<string, string> | null;
  /** The variant set specs it holds (`variantSetChildren`), in authored order, each with its variants: `[]` for none. */
  readonly variantSets: readonly VariantSetSpec[];
  /** Its child prims' names, in authored order (`primChildren`). */
  readonly primChildren: readonly string[];
  /** Its properties' names, in authored order (`properties`). */
  readonly properties: readonly string[];
}

/** A variant set spec a {@link PrimSpec} holds, and the variants it holds. */
export interface VariantSetSpec {
  readonly name: string;
  /** Its variants' names, in authored order (`variantChildren`). */
  readonly variants: readonly string[];
}

/**
 * An attribute spec. Its values are not read here: `default` says
 * whether one is authored and whether it is a block, and `timeSamples`
 * the times of its samples. AOUSD Core §7.6.3–§7.6.4 (property and
 * attribute fields).
 */
export interface AttributeSpec extends SpecBase {
  readonly type: "attribute";
  /** `typeName`, the value type it declares (`double`, `point3f[]`); `null` for an override that declares none. */
  readonly typeName: string | null;
  /** `custom`. */
  readonly custom: boolean;
  /** `variability`: `uniform` or the fallback `varying`. */
  readonly variability: "varying" | "uniform";
  /**
   * `default`: `"value"` for a value, `"block"` for a value block
   * (`None`), `"animationBlock"` for an animation block; `null` when none
   * is authored. AOUSD Core §7.6.4.2.1, §12.3 (value blocking).
   */
  readonly default: "value" | "block" | "animationBlock" | null;
  /**
   * The times of its `timeSamples`, ascending, in the layer's own time
   * (`SdfLayer::ListTimeSamplesForPath`); `null` when it authors no
   * `timeSamples` field, `[]` for an empty one. An edit target maps a
   * stage time into it ({@link EditTarget.mapTime}). AOUSD Core
   * §7.6.4.2.2.
   */
  readonly timeSamples: readonly number[] | null;
  /** `connectionPaths`: attribute paths in this layer's namespace. AOUSD Core §7.6.4.2.3. */
  readonly connections: ListOp<string> | null;
}

/** A relationship spec. AOUSD Core §7.6.5. */
export interface RelationshipSpec extends SpecBase {
  readonly type: "relationship";
  /** `custom`. */
  readonly custom: boolean;
  /** `targetPaths`: paths in this layer's namespace. AOUSD Core §7.6.5.1.1. */
  readonly targets: ListOp<string> | null;
}

/**
 * A list op one spec authors, with each of its lists apart, as OpenUSD's
 * `SdfListOp` keeps them: what the spec adds to, removes from or replaces
 * in what weaker opinions author. An item can be removed from where it
 * was added (`removeReference`) only in the layer whose list holds it.
 * AOUSD Core §12.4 (list ops).
 */
export interface ListOp<T> {
  /**
   * The explicit items (`references = [...]`), which replace weaker
   * opinions and make the other lists spurious; `null` when the op is not
   * explicit. `references = None` is explicit and empty: `[]`.
   */
  readonly explicit: readonly T[] | null;
  /** `prepend` items. */
  readonly prepended: readonly T[];
  /** `append` items. */
  readonly appended: readonly T[];
  /** `delete` items. */
  readonly deleted: readonly T[];
  /** The legacy `add` items. */
  readonly added: readonly T[];
  /** `reorder` items: the order to move the items it names into. */
  readonly ordered: readonly T[];
}

/**
 * A reference or payload as a list op authors it (OpenUSD's
 * `SdfReference`, `SdfPayload`), in the authoring layer's terms. AOUSD
 * Core §10.3.2.1–§10.3.2.2.
 */
export interface AuthoredReference {
  /** The asset path as authored (`./robot.usda`), anchored at the authoring layer; `null` for an internal arc (`</World/Spare>`). */
  readonly asset: string | null;
  /** The prim path as authored; `null` for the target layer's `defaultPrim`. */
  readonly primPath: string | null;
  /** The layer offset across the arc: identity when none is authored. */
  readonly layerOffset: { readonly offset: number; readonly scale: number };
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
 * `UsdRelationship::AddTarget`: adds `target` to the relationship's
 * target list op in the target layer at `position` (`backOfPrepend` by
 * default, as OpenUSD's), so the layer adds to what weaker layers
 * author where {@link SetTargetsEdit} replaces it. An explicit list takes
 * it whatever the position, and a target already in the chosen list
 * moves. A relationship nothing declares is created `custom`, as
 * {@link SetTargetsEdit} creates one. AOUSD Core §7.6.5.1.1
 * (`targetPaths`), §12.4 (list ops).
 */
export interface AddTargetEdit {
  readonly type: "addTarget";
  /** The relationship's property path. */
  readonly path: string;
  /** A prim or property path. */
  readonly target: string;
  readonly position?: ListPosition;
}

/**
 * `UsdRelationship::RemoveTarget`: removes `target` from the target
 * layer's explicit list, or else from its prepended and appended
 * targets, adding it to the deleted ones, which removes it from weaker
 * layers too.
 */
export interface RemoveTargetEdit {
  readonly type: "removeTarget";
  readonly path: string;
  readonly target: string;
}

/** `UsdRelationship::ClearTargets`: removes the targets the target layer authors, keeping its spec; weaker layers' targets show again. */
export interface ClearTargetsEdit {
  readonly type: "clearTargets";
  readonly path: string;
}

/**
 * `UsdAttribute::AddConnection`: adds the connection `source` to the
 * attribute's connection list op in the target layer, as
 * {@link AddTargetEdit} adds a target. The attribute needs a type:
 * refused with `edit.rejected.undeclared_type` when nothing declares it
 * (create it first, in the same edits if need be, with
 * {@link CreateAttributeEdit}), and `edit.rejected.not_an_attribute` for
 * a relationship. A shading port has {@link ConnectToSourceEdit} too.
 * AOUSD Core §7.6.4.2.3 (`connectionPaths`), §12.4.
 */
export interface AddConnectionEdit {
  readonly type: "addConnection";
  /** The attribute's property path. */
  readonly path: string;
  /** The connected property's path (`/Looks/Stone/Texture.outputs:rgb`). */
  readonly source: string;
  /** `backOfPrepend` by default, as OpenUSD's. */
  readonly position?: ListPosition;
}

/** `UsdAttribute::RemoveConnection`: removes `source` as {@link RemoveTargetEdit} removes a target; refused as {@link AddConnectionEdit} is. */
export interface RemoveConnectionEdit {
  readonly type: "removeConnection";
  readonly path: string;
  readonly source: string;
}

/** `UsdAttribute::ClearConnections`: removes the connections the target layer authors, keeping its spec; `edit.rejected.not_an_attribute` for a relationship. */
export interface ClearConnectionsEdit {
  readonly type: "clearConnections";
  readonly path: string;
}

/**
 * `UsdPrim::ApplyAPI`: adds the applied API schema `schema` (with
 * `instance` for a multiple-apply schema, such as `CollectionAPI` and
 * `"lights"`) to the prim's `apiSchemas` in the target layer, unless the
 * layer applies it already. Later edits of the same `apply` see the
 * schema's properties ({@link SetValueEdit} of one of its attributes).
 * Refused with `schema.cannot_apply` for a schema that is not a
 * registered applied schema ({@link Stage.schemas}), an instance name
 * missing, unexpected or not allowed, or a prim the schema cannot apply
 * to; `prim.not_found` when no prim is at `path`; `schema.cannot_edit`
 * beneath an instance. AOUSD Core §13.2.1.2 (`apiSchemas`), §13.3.2.
 */
export interface ApplyAPIEdit {
  readonly type: "applyAPI";
  readonly path: string;
  readonly schema: string;
  readonly instance?: string;
}

/**
 * `UsdPrim::RemoveAppliedSchema` of `schema` (`schema:instance` with an
 * `instance`): removes it from the target layer's explicit `apiSchemas`,
 * or else from its prepended and appended items, adding it to the
 * deleted ones, which removes weaker layers' applications too. Any name
 * is removed, registered or not, so a schema from a plugin the stage
 * lacks can be taken off. `prim.not_found` when no prim is at `path`.
 * AOUSD Core §13.2.1.2, §12.4.
 */
export interface RemoveAPIEdit {
  readonly type: "removeAPI";
  readonly path: string;
  readonly schema: string;
  readonly instance?: string;
}

/**
 * `SdfCopySpec` in the target layer: copies the prim spec at `path`, with
 * every spec beneath it, or the property spec at `path`, to `to`,
 * replacing whatever `to` holds there; missing ancestors of `to` are
 * created as `over`s. Duplicating `/World/Rock` to `/World/Pebble` is
 * one. Paths the copy authors that point into the copied prim follow it
 * to `to` (relationship targets, connections, inherits, specializes,
 * internal arcs, and path expressions, which OpenUSD leaves pointing at
 * the source).
 *
 * Both paths are stage paths the edit target maps into its one layer: a
 * copy reads and writes that layer only, so a spec another layer
 * authors is not copied (`edit.rejected.no_such_spec` when the target
 * layer has none at `path`). Refused with
 * `edit.rejected.copy_between_kinds` for a prim to a property or back,
 * and `edit.rejected.copy_into_source` onto, into or around its source.
 * AOUSD Core §7 (scene description), §8 (paths).
 */
export interface CopySpecEdit {
  readonly type: "copySpec";
  /** The prim or property path to copy. */
  readonly path: string;
  /** The path of the copy, of the same kind. */
  readonly to: string;
}

/** A layer offset: time offset and scale; identity by default. AOUSD Core §12.3.2.1. */
export interface LayerOffsetInput {
  readonly offset?: number;
  /** Finite and positive. */
  readonly scale?: number;
}

/**
 * `SdfLayer::InsertSubLayerPath` of the edit target's layer: the asset
 * at `asset`, anchored at that layer, as its sublayer at `index` (`0` is
 * the strongest; at the end, the weakest, without one), read through
 * `layerOffset`. An asset the stage has not read is unavailable
 * ({@link EditReceipt.unavailable}) until supplied, as an added
 * reference's is: run {@link Stage.resolve}. Refused with
 * `edit.rejected.no_such_sublayer` for an index past the end and
 * `edit.rejected.duplicate_sublayer` for an asset path listed already.
 * AOUSD Core §7.6.1 (`subLayers`), §10.3.1 (layer stacks).
 */
export interface InsertSublayerEdit {
  readonly type: "insertSublayer";
  readonly asset: string;
  readonly index?: number;
  readonly layerOffset?: LayerOffsetInput;
}

/** `SdfLayer::RemoveSubLayerPath`: removes the edit target's layer's sublayer at `index` ({@link Stage.sublayers}), with its offset. `edit.rejected.no_such_sublayer` past the end. */
export interface RemoveSublayerEdit {
  readonly type: "removeSublayer";
  readonly index: number;
}

/** `SdfLayer::SetSubLayerOffset`: the layer offset of the edit target's layer's sublayer at `index`. AOUSD Core §12.3.2.1. */
export interface SetSublayerOffsetEdit {
  readonly type: "setSublayerOffset";
  readonly index: number;
  readonly layerOffset: LayerOffsetInput;
}

/**
 * `SdfLayer::SetSubLayerPaths` with its offsets: replaces the edit
 * target's layer's sublayers, strongest first, which reorders them. An
 * asset path the layer lists already keeps the layer it reads; a new
 * one is asked for as {@link InsertSublayerEdit} asks. Refused as that
 * edit is.
 */
export interface SetSublayersEdit {
  readonly type: "setSublayers";
  readonly sublayers: readonly { readonly asset: string; readonly layerOffset?: LayerOffsetInput }[];
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
  readonly layerOffset?: LayerOffsetInput;
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
 * `UsdInherits::AddInherit` / `UsdSpecializes::AddSpecialize`: the prim
 * at `path` inherits or specializes the prim at `primPath`, added to the
 * target layer's list op at `position` (`backOfPrepend` by default, as
 * OpenUSD's). `primPath` is a stage path the edit target maps into its
 * layer, except a root prim path (`/Base`), which is authored as given;
 * one the target does not map rejects the edit, as an internal
 * reference's does ({@link AddArcEdit}). AOUSD Core
 * §10 (inherits, specializes), §12.4 (list ops).
 */
export interface AddClassArcEdit {
  readonly type: "addInherit" | "addSpecialize";
  /** The prim path. */
  readonly path: string;
  /** The prim it inherits or specializes. */
  readonly primPath: string;
  readonly position?: ListPosition;
}

/**
 * `UsdInherits::RemoveInherit` / `UsdSpecializes::RemoveSpecialize`:
 * removes the arc to `primPath` from the target layer's lists, and adds
 * it to the deleted items, which removes it from weaker layers too.
 */
export interface RemoveClassArcEdit {
  readonly type: "removeInherit" | "removeSpecialize";
  readonly path: string;
  readonly primPath: string;
}

/**
 * `UsdVariantSets::AddVariantSet`: the variant set `set` on the prim, in
 * the target layer: its spec, created unless the layer holds one, and
 * its name in the prim's `variantSetNames` at `position`
 * (`backOfPrepend` by default). Refused with
 * `edit.rejected.invalid_variant_set_name` for a name that is not an
 * identifier. Author inside a variant through
 * {@link Prim.variantEditTarget}. AOUSD Core §7.3.6 (variant set specs),
 * §7.6.2.3.5 (`variantSetNames`), §12.4.
 */
export interface AddVariantSetEdit {
  readonly type: "addVariantSet";
  readonly path: string;
  readonly set: string;
  readonly position?: ListPosition;
}

/**
 * `UsdVariantSet::AddVariant`: the variant `variant` of the set `set`,
 * which is added as {@link AddVariantSetEdit} adds it; a variant the
 * target layer holds already is left as it is. A variant name is
 * letters, digits, `_`, `|` and `-`, after an optional `.`
 * (`path.invalid` otherwise). AOUSD Core §7.3.6.
 */
export interface AddVariantEdit {
  readonly type: "addVariant";
  readonly path: string;
  readonly set: string;
  readonly variant: string;
  /** Where the set's name goes in `variantSetNames`; `backOfPrepend` by default. */
  readonly position?: ListPosition;
}

/**
 * `SdfPrimSpec::RemoveVariantSet`: removes the target layer's spec of
 * the variant set `set`, with its variants and everything authored in
 * them. As in OpenUSD, the prim's `variantSetNames` keeps the name, so
 * the set stays listed, with no opinions in this layer; removing a set
 * the layer does not hold does nothing. AOUSD Core §7.3.6, §7.6.2.3.5.
 */
export interface RemoveVariantSetEdit {
  readonly type: "removeVariantSet";
  readonly path: string;
  readonly set: string;
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

/**
 * `UsdPrim::SetActive`; `null` clears the target layer's opinion
 * (`ClearActive`), so weaker layers' show again. AOUSD Core §7.6
 * (`active`), §11 (stage population).
 */
export interface SetActiveEdit {
  readonly type: "setActive";
  readonly path: string;
  readonly active: boolean | null;
}

/** `UsdPrim::SetInstanceable`; `null` clears the target layer's opinion (`ClearInstanceable`). AOUSD Core §11.3.3. */
export interface SetInstanceableEdit {
  readonly type: "setInstanceable";
  readonly path: string;
  readonly instanceable: boolean | null;
}

/**
 * `UsdPrim::SetSpecifier`: the specifier of the prim's spec in the target
 * layer, created if need be. Unlike {@link DefinePrimEdit}, it authors
 * exactly this spec, whatever the prim's ancestors or other layers say.
 * AOUSD Core §7.6 (specifier), §12.2.1 (specifier resolution).
 */
export interface SetSpecifierEdit {
  readonly type: "setSpecifier";
  readonly path: string;
  readonly specifier: "def" | "over" | "class";
}

/**
 * `UsdPrim::SetTypeName` (`Xform`, `Sphere`) on the prim's spec in the
 * target layer, created as an `over` if need be; `null` clears it
 * (`ClearTypeName`). AOUSD Core §7.6 (`typeName`), §12.2.3.
 */
export interface SetTypeNameEdit {
  readonly type: "setTypeName";
  readonly path: string;
  readonly typeName: string | null;
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
 * Sets the metadata field `key` of the edit target's layer (`SdfLayer`'s
 * pseudo-root spec, which `UsdStage::SetMetadata` writes): `documentation`,
 * `customLayerData`, `startTimeCode`, `metersPerUnit`, `upAxis` and the
 * like. Sublayers ({@link InsertSublayerEdit} and its siblings) and
 * relocates ({@link SetRelocatesEdit}) have edits of their own and are
 * refused here. A field that composes nothing, set on a layer of the stage's own
 * layer stack, changes `/` info-only ({@link Changes}).
 */
export interface SetLayerMetadataEdit {
  readonly type: "setLayerMetadata";
  readonly key: string;
  readonly value: ValueInput;
}

/**
 * `UsdPrim::SetChildrenReorder`: the order the prim's children take
 * (`reorder nameChildren`) in the target layer; on `/`, the layer's root
 * prim order (`SdfLayer::SetRootPrimOrder`). Names are authored as given
 * and applied as OpenUSD's `SdfApplyListOrdering` applies them: a name
 * with no child is ignored. `null` clears it (`ClearChildrenReorder`).
 * AOUSD Core §7.6.2 (prim spec fields), §11 (child order).
 */
export interface SetChildrenReorderEdit {
  readonly type: "setChildrenReorder";
  readonly path: string;
  /** Child names (`Rock`), not paths. */
  readonly order: readonly string[] | null;
}

/**
 * `UsdPrim::SetPropertyOrder`: the order the prim's properties take
 * (`reorder properties`) in the target layer, names authored as given;
 * `null` clears it (`ClearPropertyOrder`). AOUSD Core §7.6.2.
 */
export interface SetPropertyOrderEdit {
  readonly type: "setPropertyOrder";
  readonly path: string;
  /** Property names (`size`), not paths. */
  readonly order: readonly string[] | null;
}

/**
 * `SdfLayer::SetRelocates`: replaces the edit target's layer's
 * `relocates`, each moving what arcs bring to `source` (a prim the layer
 * stack reaches through a reference, payload or other arc) to `target`,
 * or with `target` `null` removing it from namespace (`<>`). A prim
 * defined in the layer stack itself is not moved by it. `null` clears
 * the field (`SdfLayer::ClearRelocates`), where `[]` authors an empty
 * one. Paths are in the layer's own namespace, authored as given, not
 * mapped through the edit target. Refused with
 * `edit.rejected.invalid_relocate` for a root prim source or a target
 * that is the source, an ancestor or a descendant of it, and
 * `edit.rejected.duplicate_relocate_source` for a source named twice.
 * AOUSD Core §10.3.2.6 (relocates).
 */
export interface SetRelocatesEdit {
  readonly type: "setRelocates";
  readonly relocates: readonly { readonly source: string; readonly target: string | null }[] | null;
}

/** Clears the metadata field `key` of the edit target's layer, as {@link SetLayerMetadataEdit} sets it. */
export interface ClearLayerMetadataEdit {
  readonly type: "clearLayerMetadata";
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
  | AddTargetEdit
  | RemoveTargetEdit
  | ClearTargetsEdit
  | AddConnectionEdit
  | RemoveConnectionEdit
  | ClearConnectionsEdit
  | CopySpecEdit
  | AddArcEdit
  | RemoveArcEdit
  | AddClassArcEdit
  | RemoveClassArcEdit
  | AddVariantSetEdit
  | AddVariantEdit
  | RemoveVariantSetEdit
  | SetVariantSelectionEdit
  | SetActiveEdit
  | SetInstanceableEdit
  | SetSpecifierEdit
  | SetTypeNameEdit
  | SetChildrenReorderEdit
  | SetPropertyOrderEdit
  | SetKindEdit
  | SetMetadataEdit
  | ClearMetadataEdit
  | SetLayerMetadataEdit
  | ClearLayerMetadataEdit
  | InsertSublayerEdit
  | RemoveSublayerEdit
  | SetSublayerOffsetEdit
  | SetSublayersEdit
  | SetRelocatesEdit
  | ApplyAPIEdit
  | RemoveAPIEdit
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
  | SetSdrMetadataEdit
  | SetCommonTransformEdit
  | SetTranslateEdit
  | SetRotateEdit
  | SetScaleEdit
  | SetPivotEdit;

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

/** The order a common transform applies its Euler rotations: `XYZ` rotates about X first. */
export type RotationOrder = "XYZ" | "XZY" | "YXZ" | "YZX" | "ZXY" | "ZYX";

/**
 * `UsdGeomXformCommonAPI::SetXformVectors`: authors the whole common
 * transform stack (translation, pivot, rotation, scale, inverse pivot) at
 * `time`, or the default. Missing ops are added (translation `double3`,
 * the others `float3`) and `xformOpOrder` becomes the common stack,
 * keeping a reset. Refused with nothing authored, as OpenUSD refuses the
 * call: `xform.incompatible_ops` when the prim's ops are not a common
 * stack, `xform.rotation_order` when an existing rotation op has another
 * order, `xform.op_listed` when an op to add is already listed,
 * `xform.attribute_type` when an op holds another type, and
 * `xform.not_xformable` for a prim that is not `Xformable`.
 */
export interface SetCommonTransformEdit {
  readonly type: "setCommonTransform";
  readonly path: string;
  readonly translate: readonly [number, number, number];
  /** Degrees about X, Y and Z. */
  readonly rotate: readonly [number, number, number];
  readonly scale: readonly [number, number, number];
  readonly pivot: readonly [number, number, number];
  /** `XYZ` by default. */
  readonly rotationOrder?: RotationOrder;
  readonly time?: number;
}

/** `UsdGeomXformCommonAPI::SetTranslate`: one component of the common stack, adding only its op; refused as {@link SetCommonTransformEdit} is. */
export interface SetTranslateEdit {
  readonly type: "setTranslate";
  readonly path: string;
  readonly value: readonly [number, number, number];
  readonly time?: number;
}

/** `UsdGeomXformCommonAPI::SetRotate`: degrees about X, Y and Z in `rotationOrder` (`XYZ` by default), which an existing rotation op must have. */
export interface SetRotateEdit {
  readonly type: "setRotate";
  readonly path: string;
  readonly value: readonly [number, number, number];
  readonly rotationOrder?: RotationOrder;
  readonly time?: number;
}

/** `UsdGeomXformCommonAPI::SetScale`. */
export interface SetScaleEdit {
  readonly type: "setScale";
  readonly path: string;
  readonly value: readonly [number, number, number];
  readonly time?: number;
}

/** `UsdGeomXformCommonAPI::SetPivot`: the point rotation and scale apply about, adding both pivot ops. */
export interface SetPivotEdit {
  readonly type: "setPivot";
  readonly path: string;
  readonly value: readonly [number, number, number];
  readonly time?: number;
}

/**
 * A prim's transform as translation, pivot, Euler rotation and scale
 * ({@link Xformable.commonTransform}), as OpenUSD's
 * `UsdGeomXformCommonAPI::GetXformVectorsByAccumulation` reads it.
 */
export interface CommonTransform {
  readonly translation: readonly [number, number, number];
  /** Degrees about X, Y and Z, applied in {@link CommonTransform.rotationOrder}. */
  readonly rotation: readonly [number, number, number];
  readonly scale: readonly [number, number, number];
  readonly pivot: readonly [number, number, number];
  readonly rotationOrder: RotationOrder;
  /**
   * `ops`: the prim's ops are a common stack, and each value is its op's;
   * `accumulated`: they reduce to one by multiplying adjacent ops of one
   * type; `decomposed`: factored from the local transform, with no pivot,
   * in `XYZ`, losing any shear.
   */
  readonly source: "ops" | "accumulated" | "decomposed";
  /** Whether the common transform edits can author the prim's ops as they are (a valid `UsdGeomXformCommonAPI`). */
  readonly compatible: boolean;
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
/**
 * A collection's membership asked of some prims
 * ({@link Stage.collectionMembership}): `UsdCollectionAPI::
 * ComputeMembershipQuery`, then `IsPathIncluded` for each.
 */
export interface CollectionMembership {
  /**
   * Equal for two collections exactly when their memberships are: their
   * rule maps, included collections, expansion rules and resolved
   * `membershipExpression`s. Opaque: compare it, do not read it.
   */
  readonly key: string;
  /**
   * Whether its rule map is the pseudo-root with `expandPrims` alone: it
   * includes every prim, as the `UsdLuxLightAPI` link fallback
   * (`includeRoot = true`) does; UsdImaging gives such a collection no
   * light-link category (`_IsQueryTrivial`).
   */
  readonly everything: boolean;
  /** For each path asked, in order, whether the collection includes it. */
  readonly included: readonly boolean[];
  /**
   * The collections it reads, by collection path
   * (`/World.collection:robots`): those it includes, directly or through
   * them, and those its `membershipExpression` references. An edit to one
   * of them can change this membership.
   */
  readonly reads: readonly string[];
  /** Whether its expression references a collection that does not exist (yet): any collection made later may change it. */
  readonly readsAny: boolean;
}

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

/**
 * Why a mesh's topology is invalid ({@link MeshTopology.problem}):
 * `faceCounts` (a negative face count), `sizeMismatch` (the counts do not
 * sum to the number of indices), `vertexIndex` (an index names no point),
 * `decode` (an array failed to decode).
 */
export type MeshTopologyProblemCode = "faceCounts" | "sizeMismatch" | "vertexIndex" | "decode";

/**
 * A mesh's topology at one time, what a renderer triangulates
 * ({@link Prim.meshTopology}): the arrays copied out of the module, so
 * they stay valid and need no release. An absent array is empty, as
 * OpenUSD's `Get` leaves it: a mesh without faces is valid and draws
 * nothing.
 *
 * OpenUSD: the `UsdGeomMesh` attribute reads and
 * `UsdGeomMesh::ValidateTopology`, which, unlike this, accepts negative
 * face counts that sum to the number of indices.
 */
export interface MeshTopology {
  /** `faceVertexCounts`: each face's number of vertices. */
  readonly faceVertexCounts: Int32Array;
  /** `faceVertexIndices`: each face's vertices, as indices into `points`. */
  readonly faceVertexIndices: Int32Array;
  /** `holeIndices`: the faces a renderer leaves out. */
  readonly holeIndices: Int32Array;
  /** `orientation`: `rightHanded` (the fallback) or `leftHanded`. */
  readonly orientation: string;
  /** `subdivisionScheme`: `catmullClark` (the fallback), `loop`, `bilinear` or `none`. */
  readonly subdivisionScheme: string;
  /** The number of `points` at the time. */
  readonly pointCount: number;
  /** Whether the topology is valid: the counts sum to the indices, and every index names a point. */
  readonly valid: boolean;
  /** Why it is invalid; `null` when valid. */
  readonly problem: { readonly code: MeshTopologyProblemCode; readonly reason: string } | null;
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
  /** Retained entries the stage's edits invalidated (skinning and blend shapes). */
  readonly invalidations: number;
  /** Times the stage's change history expired before a skinning read, so the cache started over. */
  readonly resets: number;
  /** Skeletons retained. */
  readonly skeletons: number;
  /** Geometry bindings retained. */
  readonly bindings: number;
  /** Bytes of retained arrays (matrices, influences, outputs, blend-shaped arrays) in use. */
  readonly usedBytes: number;
  /** Bytes of retained array capacity. */
  readonly capacityBytes: number;
  /** Blend-shaped point or normal arrays reused without recomputing ({@link SkinningQuery.blendShapedPoints}). */
  readonly blendShapeHits: number;
  /** Blend-shaped arrays recomputed, failed ones included. */
  readonly blendShapeMisses: number;
  /** Points and normals blend-shaped (not skinned). */
  readonly blendShapedVectors: number;
  /** Geometry blend-shape bindings retained. */
  readonly blendShapeBindings: number;
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

/** How {@link Stage.camera} fits a camera's aperture to an aspect ratio (`CameraUtilConformWindowPolicy`). */
export type ConformWindowPolicy =
  /** Keep the height, change the width. */
  | "matchVertically"
  /** Keep the width, change the height. */
  | "matchHorizontally"
  /** Grow the width or the height: everything the camera saw stays in view. */
  | "fit"
  /** Shrink the width or the height: the view is filled, edges cut. */
  | "crop"
  /** Leave the aperture as it is. */
  | "dontConform";

/** What {@link Stage.camera} reads. */
export interface CameraOptions {
  /** The time; the default time when absent. */
  readonly time?: number;
  /** Fit the aperture to `aspect` (width over height) by `policy` first (`CameraUtilConformWindow`). */
  readonly conform?: { readonly policy: ConformWindowPolicy; readonly aspect: number };
}

/**
 * A `UsdGeomCamera` at one time, as a renderer reads it ({@link Stage.camera}).
 * Matrices are 16 numbers row by row in OpenUSD's row-vector convention;
 * eye space looks down -Z; the projection maps depth to OpenGL's `[-1, 1]`.
 */
export interface Camera {
  readonly projection: "perspective" | "orthographic";
  /** The camera's local-to-world matrix, scale and shear included. */
  readonly cameraToWorld: Float64Array;
  /** World to eye, conformed as `GfFrustum::ComputeViewMatrix` conforms it (no scale or shear). */
  readonly view: Float64Array;
  /** Eye to clip (`GfFrustum::ComputeProjectionMatrix`). */
  readonly projectionMatrix: Float64Array;
  /** The window `[left, bottom, right, top]`: at depth 1 for a perspective camera, in scene units for an orthographic one; offsets included. */
  readonly window: Float64Array;
  /** `[near, far]`, in scene units. */
  readonly clippingRange: Float64Array;
  /** `[horizontalAperture, verticalAperture]`, in tenths of a scene unit; conformed when asked. */
  readonly aperture: Float64Array;
  /** `[horizontalApertureOffset, verticalApertureOffset]`, in tenths of a scene unit. */
  readonly apertureOffset: Float64Array;
  /** `focalLength`, in tenths of a scene unit. */
  readonly focalLength: number;
  /** `fStop`: the lens aperture for depth of field; 0 turns it off. */
  readonly fStop: number;
  /** `focusDistance`, in scene units. */
  readonly focusDistance: number;
  /** `clippingPlanes`, four numbers `[a, b, c, d]` a plane, in camera space. */
  readonly clippingPlanes: Float64Array;
  /** `[shutter:open, shutter:close]`, in time codes relative to the frame. */
  readonly shutter: Float64Array;
  /** The exposure controls (`exposure`, `exposure:fStop`, `exposure:iso`, `exposure:responsivity`, `exposure:time`). */
  readonly exposure: {
    readonly exposure: number;
    readonly fStop: number;
    readonly iso: number;
    readonly responsivity: number;
    readonly time: number;
  };
  /** `UsdGeomCamera::ComputeLinearExposureScale`: what scene luminance is multiplied by. */
  readonly linearExposureScale: number;
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

/**
 * A native instancing prototype ({@link Stage.prototypes}): the instance
 * roots that share it, whose descendants are instance proxies of one
 * composed subtree.
 */
export interface Prototype {
  /** The instance roots, in traversal order; the first is where its prims are read. */
  readonly instances: readonly string[];
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
