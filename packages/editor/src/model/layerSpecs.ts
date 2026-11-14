// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What one layer authors about composition at one spec, read with
// `Stage.layerSpec(layer, path)`: the prim or variant spec there, and its
// arcs as the items of its list ops (`prepend references =
// @./robot.usda@</Robot>`), each list apart. The stage composes arcs but
// does not say which layer authored one; the inspector reads the specs it
// needs (the selected prim's site in each layer of each arc's stack, and
// its ancestors') to offer removing an arc where edits go, and to name the
// layer to edit otherwise. Nothing is exported or scanned, so a layer of
// any size is read the same. AOUSD Core §7.6.2 (prim spec fields:
// `references`, `payload`, `inherits`, `specializes`, `variantSetNames`,
// `variantSelection`), §7.3.6 (variant set specs), §12.4 (list ops).
import type { AuthoredReference, Layer, Layerstack, LayerSpec, ListOp, PrimSpec, Stage } from "@forest-rs/layerstack";

/** The prim spec fields that author arcs, by their OpenUSD keys. */
export type ArcField = "references" | "payload" | "inherits" | "specializes";

/** How an arc list op item is authored: `prepend`, `append`, `add`, `delete`, `reorder`, or `explicit` (no keyword). */
export type ListOpKind = "explicit" | "prepend" | "append" | "add" | "delete" | "reorder";

/** One arc a spec authors: an item of one of its arc list ops. */
export interface AuthoredArc {
  readonly field: ArcField;
  readonly op: ListOpKind;
  /** The asset path as authored, without its `@`s; `null` for an internal reference or payload, and for class arcs. */
  readonly asset: string | null;
  /** The prim path as authored, without its `<>`; `null` for a reference or payload to the layer's default prim. */
  readonly primPath: string | null;
  /** The layer offset across a reference or payload; identity when none is authored. */
  readonly offset: number;
  readonly scale: number;
  /** Its place among the arcs of its spec ({@link AuthoredSpec.arcs}): what tells two equal items of one spec apart. */
  readonly index: number;
}

/** A prim or variant spec one layer holds, with its arcs as list op items. */
export interface AuthoredSpec {
  /** The spec as `Stage.layerSpec` reads it. */
  readonly spec: PrimSpec;
  /** Its arcs, field by field (references, payload, inherits, specializes), each list op's lists in the order USDA writes them. */
  readonly arcs: readonly AuthoredArc[];
}

/** Reads the prim or variant spec `layer` holds at `path` (in its own namespace); `null` when it holds none there. */
export type SpecsOf = (layer: Layer, path: string) => AuthoredSpec | null;

/** Each list of a list op, in the order USDA writes them, with the keyword that authors it. */
const LISTS: readonly (readonly [keyof ListOp<unknown>, ListOpKind])[] = [
  ["explicit", "explicit"],
  ["deleted", "delete"],
  ["added", "add"],
  ["prepended", "prepend"],
  ["appended", "append"],
  ["ordered", "reorder"],
];

/** The arcs `spec` authors, as {@link AuthoredSpec.arcs} lists them. */
export function arcsOf(spec: PrimSpec): AuthoredArc[] {
  const arcs: AuthoredArc[] = [];
  const add = <T>(field: ArcField, op: ListOp<T> | null, item: (value: T) => Pick<AuthoredArc, "asset" | "primPath" | "offset" | "scale">) => {
    if (op === null) return;
    for (const [list, kind] of LISTS) {
      for (const value of (op[list] as readonly T[] | null) ?? []) arcs.push({ field, op: kind, ...item(value), index: arcs.length });
    }
  };
  const reference = (value: AuthoredReference) => ({ asset: value.asset, primPath: value.primPath, offset: value.layerOffset.offset, scale: value.layerOffset.scale });
  const path = (value: string) => ({ asset: null, primPath: value, offset: 0, scale: 1 });
  add("references", spec.references, reference);
  add("payload", spec.payloads, reference);
  add("inherits", spec.inherits, path);
  add("specializes", spec.specializes, path);
  return arcs;
}

/** `spec` with its arcs, when it is a prim or variant spec; `null` otherwise. */
export function authoredSpec(spec: LayerSpec | null): AuthoredSpec | null {
  if (spec === null || (spec.type !== "prim" && spec.type !== "variant")) return null;
  return { spec, arcs: arcsOf(spec) };
}

/**
 * The specs of `stage`'s layers, each layer and path read once (a spec
 * read is a snapshot: make another cache after an edit), so what it gives
 * is the same object each time it is asked: an arc taken by one composed
 * arc is the same item when the next asks.
 */
export class SpecCache {
  readonly #stage: Pick<Stage, "layerSpec">;
  readonly #specs = new Map<Layer, Map<string, LayerSpec | null>>();
  readonly #prims = new Map<LayerSpec, AuthoredSpec | null>();

  constructor(stage: Pick<Stage, "layerSpec">) {
    this.#stage = stage;
  }

  /** What `layer` holds at `path` (`Stage.layerSpec`): any kind of spec, or `null`. Throws what that throws. */
  spec(layer: Layer, path: string): LayerSpec | null {
    let paths = this.#specs.get(layer);
    if (paths === undefined) this.#specs.set(layer, (paths = new Map()));
    const cached = paths.get(path);
    if (cached !== undefined) return cached;
    const spec = this.#stage.layerSpec(layer, path);
    paths.set(path, spec);
    return spec;
  }

  /** The prim or variant spec `layer` holds at `path`, with its arcs ({@link SpecsOf}). */
  readonly prim: SpecsOf = (layer, path) => {
    const spec = this.spec(layer, path);
    if (spec === null) return null;
    let read = this.#prims.get(spec);
    if (read === undefined) this.#prims.set(spec, (read = authoredSpec(spec)));
    return read;
  };
}

/** A {@link SpecsOf} over `stage` that reads each layer and path once ({@link SpecCache}). */
export function specReader(stage: Pick<Stage, "layerSpec">): SpecsOf {
  return new SpecCache(stage).prim;
}

/** A root prim of a layer: its name, specifier and type. */
export interface RootPrim {
  readonly name: string;
  readonly specifier: "def" | "over" | "class";
  readonly typeName: string | null;
}

/** A layer's default prim and root prims, as the dialog that adds an arc offers them; or why they were not read. */
export type LayerSummary =
  | { readonly kind: "read"; readonly defaultPrim: string | null; readonly roots: readonly RootPrim[] }
  | { readonly kind: "unread"; readonly reason: string };

/** The default prim and root prims `layer` of `stage` authors (its pseudo-root's `defaultPrim` and `primChildren`, each root's spec). */
export function layerSummary(stage: Pick<Stage, "layerSpec">, layer: Layer): LayerSummary {
  const root = stage.layerSpec(layer, "/");
  const roots = root.primChildren.map((name): RootPrim => {
    const spec = stage.layerSpec(layer, `/${name}`);
    const prim = spec !== null && spec.type === "prim" ? spec : null;
    return { name, specifier: prim?.specifier ?? "over", typeName: prim?.typeName ?? null };
  });
  return { kind: "read", defaultPrim: root.defaultPrim === "" ? null : root.defaultPrim, roots };
}

/**
 * The default prim and root prims of a layer the stage has not read, from
 * its bytes (USDA or a crate): opened on a stage of its own, its root
 * layer's specs read ({@link layerSummary}) and the stage freed, nothing
 * it names fetched.
 */
export function summarizeBytes(layerstack: Pick<Layerstack, "open">, identifier: string, bytes: Uint8Array): LayerSummary {
  const stage = layerstack.open({ identifier, bytes });
  try {
    return layerSummary(stage, stage.root);
  } finally {
    stage.free();
  }
}
