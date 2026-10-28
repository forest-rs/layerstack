// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

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
}

/** A scalar's storage type in an array: `f16` is half floats as raw bits, `bool` is bytes of 0 or 1. */
export type Scalar = "bool" | "u8" | "i32" | "u32" | "i64" | "u64" | "f16" | "f32" | "f64" | "string";

/** The elements of an array, as one typed array of scalars, or strings for text. */
export type ArrayElements =
  | Uint8Array
  | Uint16Array
  | Int32Array
  | Uint32Array
  | BigInt64Array
  | BigUint64Array
  | Float32Array
  | Float64Array
  | string[];

/**
 * An array value: its layout, and its elements as one buffer.
 *
 * `length` elements of `components` scalars each, flattened
 * (`point3f[]` is `f32` with 3 components). Matrices are row-major;
 * quaternions are imaginary first, `[i, j, k, r]`.
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
  /** A new JS-owned copy of the elements; each call copies. */
  copy(): ArrayElements;
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
  /** Bytes copied into the module: opened and supplied sources. */
  readonly bytesIn: number;
  /** Bytes copied out: array elements. */
  readonly bytesOut: number;
  /** The module's memory now, in bytes (0 when the module does not expose it). */
  readonly memoryBytes: number;
}
