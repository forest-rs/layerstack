// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The retained draw list: what a renderer draws from a stage, grouped into
// instanced draws and kept up to date from change records. It mirrors
// `layerstack_stagehand::DrawList`, reading the stage through
// `@forest-rs/layerstack` and building buffers in this package's module.
import type { Changes, Stage } from "@forest-rs/layerstack";
import type { Stagehand, StagehandMesh } from "./index.js";
import { readMesh, readShape } from "./read.js";
import type { MeshInput, MeshOptions, PrimvarInput, Shape, Tessellation } from "./types.js";

/** A `UsdGeomImageable` purpose. */
export type Purpose = "default" | "render" | "proxy" | "guide";

/** How a {@link DrawList} reads a stage. */
export interface DrawOptions {
  /** The time read; the default time when absent. */
  readonly time?: number;
  /** The purposes drawn; `default` and `proxy` (as usdview shows a stage) by default. */
  readonly purposes?: readonly Purpose[];
  /** How meshes are planned. */
  readonly mesh?: MeshOptions;
  /** How implicit shapes are cut. */
  readonly tessellation?: Tessellation;
}

/** What an item draws, and why its instances are grouped. */
export type ItemSource =
  /** A gprim outside any instance, drawn once. */
  | { readonly kind: "gprim"; readonly path: string }
  /**
   * A gprim of a native instancing prototype, drawn once per instance
   * binding `material`: `prim` is the gprim in the prototype's first
   * instance ({@link Stage.prototypes}).
   */
  | { readonly kind: "prototype"; readonly prim: string; readonly material: string | null }
  /** A gprim a `PointInstancer` places: its prototype `index`, or a gprim inside it. */
  | { readonly kind: "pointInstancer"; readonly instancer: string; readonly index: number; readonly prim: string };

/** One instanced draw: a retained buffer at one or more world matrices. */
export interface DrawItem {
  /** Its identity: stable while it exists, never reused. */
  readonly id: number;
  readonly source: ItemSource;
  /** The prim whose geometry, primvars and material are drawn. */
  readonly prim: string;
  /** The buffer drawn ({@link DrawList.buffers}). */
  readonly buffer: number;
  /**
   * Each drawn instance's local-to-world matrix, 16 numbers row by row
   * (OpenUSD's row-vector matrices; three.js's `Matrix4.fromArray` reads
   * them as they are). Empty when the item is {@link hidden}.
   */
  readonly matrices: Float64Array;
  /** Each drawn instance's prim, in step with the matrices: the gprim, or its instance proxies; empty for a point instancer. */
  readonly instances: readonly string[];
  /** Each drawn instance's id, for a point instancer; `null` otherwise. */
  readonly ids: BigInt64Array | null;
  /** Whether nothing of it is drawn: invisible, of a purpose not drawn, or every instance masked. Its buffer is kept. */
  readonly hidden: boolean;
  /** The material bound to {@link prim} (`preview` purpose, else all-purpose), if any. */
  readonly material: string | null;
  /** Each face subset's bound material, in the order of {@link DrawBuffers.subsets}. */
  readonly subsetMaterials: readonly (string | null)[];
}

/**
 * A retained buffer, owned by the {@link DrawList}: a mesh's planned
 * buffers or a shape's tessellation. Upload it before the next diff, which
 * may refill (new typed arrays on the same {@link mesh}), rebuild (a new
 * {@link mesh}) or free it (the mesh disposed).
 */
export interface DrawBuffers {
  readonly key: number;
  /** The prim it was read from: the mesh, or the first prim drawn with this shape. */
  readonly prim: string;
  /** The buffers, and for a mesh its report. */
  readonly mesh: StagehandMesh;
  /** The mesh's face subsets, in the order of their draw ranges. */
  readonly subsets: readonly string[];
  /** Bumped each time the buffers are refilled or rebuilt. */
  readonly revision: number;
  /** The shape tessellated, for a shape. */
  readonly shape: Shape | null;
}

/** The work one diff did: what was read, reused, recomputed or rebuilt. */
export interface DrawWork {
  /** Prims visited discovering items under resynced paths. */
  readonly primsTraversed: number;
  /** Items a change reached, whose inputs were read again. */
  readonly itemsRead: number;
  /** Items no change reached: nothing of them was read. */
  readonly itemsUntouched: number;
  /** Instance world matrices computed. */
  readonly matricesComputed: number;
  /** Local transforms the stage's transform cache computed (the rest it reused). */
  readonly localTransforms: number;
  /** Meshes and shapes read again and found unchanged: their buffers reused. */
  readonly buffersReused: number;
  /** Buffers filled again by a kept plan: no triangulation. */
  readonly refills: number;
  /** Meshes planned: faces triangulated and vertices split. */
  readonly triangulations: number;
  /** Implicit shapes tessellated. */
  readonly tessellations: number;
}

/**
 * What changed in a {@link DrawList} since the diff before, for a renderer
 * to apply instead of rebuilding; each list sorted. An added item is in no
 * other item list; a buffer added in this diff is in no other buffer list.
 */
export interface DrawDiff {
  readonly added: readonly number[];
  /** Ids that name nothing now. */
  readonly removed: readonly number[];
  /** Items whose matrices changed: moved, or instances shown or hidden. */
  readonly matrices: readonly number[];
  /** Items that became hidden or shown. */
  readonly visibility: readonly number[];
  /** Items whose bound materials changed. */
  readonly materials: readonly number[];
  /** Items now drawing another buffer. */
  readonly rebound: readonly number[];
  readonly buffersAdded: readonly number[];
  /** Buffers filled again by their plan: same indices and ranges, new positions, normals, `st` or colors. */
  readonly refilled: readonly number[];
  /** Buffers planned again: new indices, ranges and vertices. */
  readonly rebuilt: readonly number[];
  /** Buffers no item draws any more: disposed. */
  readonly freed: readonly number[];
  readonly work: DrawWork;
}

const SHAPES = new Set(["Cube", "Sphere", "Cylinder", "Cone", "Capsule", "Plane", "Cylinder_1", "Capsule_1"]);

/** What a change may have changed of an item. */
interface Dirty {
  instances: boolean;
  transform: boolean;
  visibility: boolean;
  geometry: boolean;
  material: boolean;
}

const none = (): Dirty => ({ instances: false, transform: false, visibility: false, geometry: false, material: false });
const all = (): Dirty => ({ instances: true, transform: true, visibility: true, geometry: true, material: true });
const any = (d: Dirty) => d.instances || d.transform || d.visibility || d.geometry || d.material;
function merge(into: Dirty, from: Dirty): void {
  into.instances ||= from.instances;
  into.transform ||= from.transform;
  into.visibility ||= from.visibility;
  into.geometry ||= from.geometry;
  into.material ||= from.material;
}

const GEOMETRY = new Set([
  "points",
  "normals",
  "faceVertexCounts",
  "faceVertexIndices",
  "holeIndices",
  "orientation",
  "subdivisionScheme",
  "size",
  "radius",
  "radiusTop",
  "radiusBottom",
  "height",
  "width",
  "length",
  "axis",
  // A `GeomSubset`'s, read by its mesh.
  "indices",
  "elementType",
  "familyName",
]);

/** What a property's change may change of the items under its prim, as `layerstack_stagehand` classifies it. */
function propertyDirt(name: string): Dirty {
  const dirty = none();
  if (name === "xformOpOrder" || name.startsWith("xformOp:")) dirty.transform = true;
  else if (name === "visibility" || name === "purpose" || name.endsWith("Visibility")) dirty.visibility = true;
  else if (name.startsWith("material:binding")) dirty.material = true;
  else if (
    GEOMETRY.has(name) ||
    ["primvars:normals", "primvars:st", "primvars:displayColor"].some((p) => name === p || name === `${p}:indices`)
  )
    dirty.geometry = true;
  return dirty;
}

/** Whether `path` is `root` or beneath it. */
function under(path: string, root: string): boolean {
  return root === "/" || path === root || path.startsWith(`${root}/`);
}

function parent(path: string): string | null {
  if (path === "/") return null;
  const at = path.lastIndexOf("/");
  return at <= 0 ? "/" : path.slice(0, at);
}

function sourceKey(source: ItemSource): string {
  switch (source.kind) {
    case "gprim":
      return `g ${source.path}`;
    case "prototype":
      return `p ${source.prim} ${source.material ?? ""}`;
    case "pointInstancer":
      return `i ${source.instancer} ${source.index} ${source.prim}`;
  }
}

function sourcePath(source: ItemSource): string {
  return source.kind === "gprim" ? source.path : source.prim;
}

/** `a * b`, row-vector 4x4 matrices of 16 numbers row by row: `a` applies first. */
function multiply(a: ArrayLike<number>, b: ArrayLike<number>, out: Float64Array, at: number): void {
  for (let row = 0; row < 4; row++) {
    for (let column = 0; column < 4; column++) {
      let sum = 0;
      for (let k = 0; k < 4; k++) sum += a[4 * row + k]! * b[4 * k + column]!;
      out[at + 4 * row + column] = sum;
    }
  }
}

/** The inverse of an affine row-vector matrix (last column 0, 0, 0, 1). */
function inverseAffine(m: ArrayLike<number>): Float64Array {
  const [a, b, c, , d, e, f, , g, h, i] = Array.from({ length: 11 }, (_, k) => m[k]!);
  const det = a! * (e! * i! - f! * h!) - b! * (d! * i! - f! * g!) + c! * (d! * h! - e! * g!);
  const r = new Float64Array(16);
  r[0] = (e! * i! - f! * h!) / det;
  r[1] = (c! * h! - b! * i!) / det;
  r[2] = (b! * f! - c! * e!) / det;
  r[4] = (f! * g! - d! * i!) / det;
  r[5] = (a! * i! - c! * g!) / det;
  r[6] = (c! * d! - a! * f!) / det;
  r[8] = (d! * h! - e! * g!) / det;
  r[9] = (b! * g! - a! * h!) / det;
  r[10] = (a! * e! - b! * d!) / det;
  const [tx, ty, tz] = [m[12]!, m[13]!, m[14]!];
  r[12] = -(tx * r[0] + ty * r[4] + tz * r[8]);
  r[13] = -(tx * r[1] + ty * r[5] + tz * r[9]);
  r[14] = -(tx * r[2] + ty * r[6] + tz * r[10]);
  r[15] = 1;
  return r;
}

function sameArray(a: ArrayLike<number | bigint> | null | undefined, b: ArrayLike<number | bigint> | null | undefined): boolean {
  if (a == null || b == null) return a == b;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function samePrimvar(a: PrimvarInput | null | undefined, b: PrimvarInput | null | undefined): boolean {
  if (a == null || b == null) return a == b;
  return (
    a.interpolation === b.interpolation &&
    (a.elementSize ?? 1) === (b.elementSize ?? 1) &&
    sameArray(a.indices, b.indices) &&
    sameArray(a.values, b.values)
  );
}

/** Whether the plan built from `a` draws `b`: the same topology and subsets. */
function sameTopology(a: MeshInput, b: MeshInput): boolean {
  return (
    sameArray(a.faceVertexCounts, b.faceVertexCounts) &&
    sameArray(a.faceVertexIndices, b.faceVertexIndices) &&
    sameArray(a.holeIndices, b.holeIndices) &&
    (a.orientation ?? "rightHanded") === (b.orientation ?? "rightHanded") &&
    (a.subdivisionScheme ?? "catmullClark") === (b.subdivisionScheme ?? "catmullClark") &&
    (a.subsets ?? []).length === (b.subsets ?? []).length &&
    (a.subsets ?? []).every((s, i) => s.path === b.subsets![i]!.path && sameArray(s.faces, b.subsets![i]!.faces))
  );
}

/** What one discovery covers: everything under a path, a prototype's items, or an instancer's. */
type Scope = { kind: "resync"; root: string } | { kind: "prototype"; first: string } | { kind: "instancer"; path: string };

function covers(scope: Scope, source: ItemSource): boolean {
  switch (scope.kind) {
    case "resync":
      return source.kind === "pointInstancer" ? under(source.instancer, scope.root) : under(sourcePath(source), scope.root);
    case "prototype":
      return source.kind === "prototype" && under(source.prim, scope.first);
    case "instancer":
      return source.kind === "pointInstancer" && source.instancer === scope.path;
  }
}

interface Found {
  sources: ItemSource[];
  scopes: Scope[];
  prims: number;
}

interface Entry {
  item: DrawItem;
  /** Every path whose change reaches the item. */
  depends: string[];
}

interface Retained {
  buffers: DrawBuffers;
  /** The mesh as last read, for a mesh. */
  input: MeshInput | null;
  users: number;
}

/** The diff being gathered. */
class Gather {
  added = new Set<number>();
  removed = new Set<number>();
  matrices = new Set<number>();
  visibility = new Set<number>();
  materials = new Set<number>();
  rebound = new Set<number>();
  buffersAdded = new Set<number>();
  refilled = new Set<number>();
  rebuilt = new Set<number>();
  freed = new Set<number>();
  work = {
    primsTraversed: 0,
    itemsRead: 0,
    itemsUntouched: 0,
    matricesComputed: 0,
    localTransforms: 0,
    buffersReused: 0,
    refills: 0,
    triangulations: 0,
    tessellations: 0,
  };
}

const sorted = (set: Set<number>) => [...set].sort((a, b) => a - b);

/**
 * The items and buffers a renderer draws from a stage, retained and kept
 * up to date from change records: the TypeScript mirror of
 * `layerstack_stagehand::DrawList`, with the same items, diffs and work
 * counts.
 *
 * - Items ({@link DrawItem}) are instanced draws: a gprim outside any
 *   instance is drawn once; each gprim of a native instancing prototype is
 *   one item drawn at every instance binding the same material; each
 *   prototype of a `PointInstancer` is one item drawn at every unmasked
 *   instance. Hidden items stay listed, with no matrices, their buffers
 *   kept.
 * - Buffers ({@link DrawBuffers}) are owned by the list: one per mesh,
 *   planned once and refilled when its points or primvars change; equal
 *   implicit shapes share one. A buffer is disposed, and listed in
 *   {@link DrawDiff.freed}, in the diff that drops its last item;
 *   {@link dispose} frees them all.
 * - Diffs ({@link DrawDiff}): {@link refresh} reads the whole stage;
 *   {@link applyChanges} reads what a change record reaches (resynced
 *   paths rediscover their subtree; changed properties are classified by
 *   name into transform, visibility, material and geometry work under
 *   their prim); {@link setTime} reads every item at a new time and
 *   rebuilds only what changed.
 *
 * ```ts
 * const list = stagehand.drawList(stage, { time });
 * apply(list.refresh());                                     // every item added
 * stage.onChange((changes) => apply(list.applyChanges(changes)));
 * apply(list.setTime(next));                                 // animated meshes refilled
 * list.dispose();                                            // or `using list = ...`
 * ```
 */
export class DrawList implements Disposable {
  readonly #stagehand: Stagehand;
  readonly #stage: Stage;
  #time: number | undefined;
  readonly #purposes: ReadonlySet<string>;
  readonly #meshOptions: MeshOptions;
  readonly #tessellation: Tessellation;
  readonly #entries = new Map<number, Entry>();
  readonly #keys = new Map<string, number>();
  readonly #buffers = new Map<number, Retained>();
  readonly #meshBuffers = new Map<string, number>();
  readonly #shapeBuffers = new Map<string, number>();
  #nextItem = 0;
  #nextBuffer = 0;

  /** @internal Use {@link Stagehand.drawList}. */
  constructor(stagehand: Stagehand, stage: Stage, options: DrawOptions = {}) {
    this.#stagehand = stagehand;
    this.#stage = stage;
    this.#time = options.time;
    this.#purposes = new Set(options.purposes ?? ["default", "proxy"]);
    this.#meshOptions = options.mesh ?? {};
    this.#tessellation = options.tessellation ?? {};
  }

  /** The time it reads; `undefined` for the default time. */
  get time(): number | undefined {
    return this.#time;
  }

  /** The items, in the order they were added. */
  items(): DrawItem[] {
    return [...this.#entries.values()].map((entry) => entry.item);
  }

  /** The item `id`, while it exists. */
  item(id: number): DrawItem | null {
    return this.#entries.get(id)?.item ?? null;
  }

  /** The buffer `key`, while an item draws it. */
  buffers(key: number): DrawBuffers | null {
    return this.#buffers.get(key)?.buffers ?? null;
  }

  /** Reads the whole stage: the first call adds every item; a later one reports what differs, reusing what did not change. */
  refresh(): DrawDiff {
    return this.#apply({ resynced: ["/"], changedInfoOnly: [], properties: [], resyncedProperties: [] });
  }

  /**
   * Brings the list up to the stage after `changes`, one record its
   * change listeners heard ({@link Stage.onChange}); pass every record, in
   * order. Only the items the changes reach are read again.
   */
  applyChanges(changes: Changes): DrawDiff {
    return this.#apply(changes);
  }

  /** Reads every item at `time` (`undefined` for the default time); an animated mesh is refilled, never triangulated again unless its topology changed. */
  setTime(time: number | undefined): DrawDiff {
    const before = this.#stage.transformCache().localComputed;
    this.#time = time;
    const gather = new Gather();
    const read = new Set<number>();
    for (const [id, entry] of [...this.#entries]) {
      read.add(id);
      if (!this.#read(id, entry.item.source, all(), false, gather)) this.#remove(id, gather);
    }
    return this.#finish(gather, read, before);
  }

  /** Disposes every buffer's mesh. Idempotent. */
  dispose(): void {
    for (const retained of this.#buffers.values()) retained.buffers.mesh.dispose();
    this.#buffers.clear();
    this.#entries.clear();
    this.#keys.clear();
    this.#meshBuffers.clear();
    this.#shapeBuffers.clear();
  }

  [Symbol.dispose](): void {
    this.dispose();
  }

  #apply(changes: Pick<Changes, "resynced" | "changedInfoOnly" | "properties" | "resyncedProperties">): DrawDiff {
    const stage = this.#stage;
    const before = stage.transformCache().localComputed;
    const gather = new Gather();
    const dirty = new Map<number, Dirty>();
    const mark = (id: number, what: Dirty) => {
      const at = dirty.get(id);
      if (at === undefined) dirty.set(id, { ...what });
      else merge(at, what);
    };
    const found: Found = { sources: [], scopes: [], prims: 0 };
    const firsts = new Map<string, string>();
    for (const prototype of stage.prototypes()) {
      for (const instance of prototype.instances) firsts.set(instance, prototype.instances[0]!);
    }
    for (const root of changes.resynced) {
      this.#discover(root, firsts, found);
      for (const [id, entry] of this.#entries) {
        if (entry.depends.some((path) => under(path, root))) mark(id, all());
      }
    }
    const reached: [string, Dirty][] = [];
    for (const prim of changes.changedInfoOnly) {
      const inventory = changes.properties.find((changed) => changed.prim === prim);
      const what = inventory === undefined ? all() : none();
      for (const field of inventory?.fields ?? []) merge(what, propertyDirt(field.property));
      reached.push([prim, what]);
    }
    for (const property of changes.resyncedProperties) {
      const dot = property.lastIndexOf(".");
      reached.push([property.slice(0, dot), propertyDirt(property.slice(dot + 1))]);
    }
    const prototypes = new Set<string>();
    for (const [prim, what] of reached) {
      const typeName = stage.prim(prim)?.typeName ?? null;
      const instancer = typeName === "PointInstancer";
      if (instancer) {
        found.scopes.push({ kind: "instancer", path: prim });
        this.#discoverInstancer(prim, found);
      }
      const subsetOf = typeName === "GeomSubset" ? parent(prim) : null;
      for (const [id, entry] of this.#entries) {
        const reach = { ...what };
        let hit = entry.depends.some((path) => under(path, prim)) || subsetOf === entry.item.prim;
        const source = entry.item.source;
        if (instancer && source.kind === "pointInstancer" && source.instancer === prim) {
          reach.instances = true;
          hit = true;
        }
        if (!hit || !any(reach)) continue;
        if (reach.material && source.kind === "prototype") {
          const first = this.#instanceRoot(source.prim);
          if (first !== null) prototypes.add(first);
        }
        mark(id, reach);
      }
    }
    for (const first of prototypes) {
      found.scopes.push({ kind: "prototype", first });
      this.#discoverPrototype(first, found);
    }
    gather.work.primsTraversed = found.prims;
    // New items first, so a buffer a gone item shares with a new one (a
    // renamed prim's shape) is kept rather than freed and made again.
    const seen = new Set(found.sources.map(sourceKey));
    const gone = [...this.#entries]
      .filter(([, entry]) => !seen.has(sourceKey(entry.item.source)) && found.scopes.some((scope) => covers(scope, entry.item.source)))
      .map(([id]) => id);
    const read = new Set<number>();
    const added = new Set<string>();
    for (const source of found.sources) {
      const key = sourceKey(source);
      const known = this.#keys.get(key);
      if (known !== undefined) {
        mark(known, all());
      } else if (!added.has(key)) {
        added.add(key);
        const id = this.#nextItem++;
        read.add(id);
        if (this.#read(id, source, all(), true, gather)) gather.added.add(id);
      }
    }
    for (const id of gone) {
      dirty.delete(id);
      this.#remove(id, gather);
    }
    for (const [id, what] of [...dirty].sort(([a], [b]) => a - b)) {
      const entry = this.#entries.get(id);
      if (entry === undefined) continue;
      read.add(id);
      if (!this.#read(id, entry.item.source, what, false, gather)) this.#remove(id, gather);
    }
    return this.#finish(gather, read, before);
  }

  #finish(gather: Gather, read: Set<number>, localBefore: number): DrawDiff {
    for (const id of gather.added) {
      gather.matrices.delete(id);
      gather.visibility.delete(id);
      gather.materials.delete(id);
      gather.rebound.delete(id);
    }
    for (const key of gather.buffersAdded) {
      gather.refilled.delete(key);
      gather.rebuilt.delete(key);
    }
    for (const key of gather.rebuilt) gather.refilled.delete(key);
    const live = [...read].filter((id) => this.#entries.has(id));
    gather.work.itemsRead = live.length;
    gather.work.itemsUntouched = this.#entries.size - live.length;
    gather.work.localTransforms = this.#stage.transformCache().localComputed - localBefore;
    return {
      added: sorted(gather.added),
      removed: sorted(gather.removed),
      matrices: sorted(gather.matrices),
      visibility: sorted(gather.visibility),
      materials: sorted(gather.materials),
      rebound: sorted(gather.rebound),
      buffersAdded: sorted(gather.buffersAdded),
      refilled: sorted(gather.refilled),
      rebuilt: sorted(gather.rebuilt),
      freed: sorted(gather.freed),
      work: { ...gather.work },
    };
  }

  #drawn(typeName: string | null): boolean {
    return typeName === "Mesh" || (typeName !== null && SHAPES.has(typeName));
  }

  #discover(root: string, firsts: Map<string, string>, found: Found): void {
    const stage = this.#stage;
    // The root may be inside an instance or a point instancer: discover from the outermost one.
    let start = root;
    for (let at = parent(root); at !== null; at = parent(at)) {
      const prim = stage.prim(at);
      if (prim !== null && (prim.instance || prim.typeName === "PointInstancer")) start = at;
    }
    found.scopes.push({ kind: "resync", root: start });
    const prototypes = new Set<string>();
    const stack = [start];
    while (stack.length > 0) {
      const path = stack.pop()!;
      const prim = stage.prim(path);
      if (prim === null) continue;
      found.prims++;
      if (!prim.active || !prim.defined || prim.abstract) continue;
      if (prim.typeName === "PointInstancer") {
        found.scopes.push({ kind: "instancer", path });
        this.#discoverInstancer(path, found);
        continue;
      }
      if (prim.instance) {
        const first = firsts.get(path);
        if (first !== undefined) prototypes.add(first);
        continue;
      }
      if (this.#drawn(prim.typeName)) found.sources.push({ kind: "gprim", path });
      stack.push(...prim.children().reverse());
    }
    for (const first of prototypes) {
      found.scopes.push({ kind: "prototype", first });
      this.#discoverPrototype(first, found);
    }
  }

  #discoverPrototype(first: string, found: Found): void {
    const stage = this.#stage;
    const roots = this.#instancesOf(first);
    const stack = stage.prim(first)?.children() ?? [];
    while (stack.length > 0) {
      const path = stack.pop()!;
      const prim = stage.prim(path);
      if (prim === null) continue;
      found.prims++;
      if (!prim.active || !prim.defined || prim.abstract) continue;
      if (this.#drawn(prim.typeName)) {
        const materials: (string | null)[] = [];
        for (const proxy of proxies(first, path, roots)) {
          const material = this.#material(proxy);
          if (!materials.includes(material)) materials.push(material);
        }
        for (const material of materials) found.sources.push({ kind: "prototype", prim: path, material });
      }
      stack.push(...prim.children());
    }
  }

  #discoverInstancer(instancer: string, found: Found): void {
    const stage = this.#stage;
    const prototypes = stage.relationship(`${instancer}.prototypes`)?.targets() ?? [];
    prototypes.forEach((prototype, index) => {
      const stack = [prototype];
      while (stack.length > 0) {
        const path = stack.pop()!;
        const prim = stage.prim(path);
        if (prim === null) continue;
        found.prims++;
        if (!prim.active || !prim.defined) continue;
        if (this.#drawn(prim.typeName)) found.sources.push({ kind: "pointInstancer", instancer, index, prim: path });
        stack.push(...prim.children());
      }
    });
  }

  #instancesOf(first: string): readonly string[] {
    return this.#stage.prototypes().find((prototype) => prototype.instances.includes(first))?.instances ?? [];
  }

  #instanceRoot(path: string): string | null {
    for (let at = parent(path); at !== null; at = parent(at)) {
      if (this.#stage.prim(at)?.instance === true) return at;
    }
    return null;
  }

  #material(path: string): string | null {
    return this.#stage.prim(path)?.boundMaterial({ purpose: "preview" })?.material ?? null;
  }

  /** Which of `paths` are drawn: visible, of a drawn purpose. */
  #shown(paths: readonly string[]): boolean[] {
    if (paths.length === 0) return [];
    const at = this.#time === undefined ? {} : { time: this.#time };
    const visibility = this.#stage.visibilities(paths, at);
    const purposes = this.#stage.purposeInfos(paths);
    return paths.map(
      (_, i) => visibility[i] !== null && visibility[i] !== "invisible" && this.#purposes.has(purposes[i]?.purpose ?? ""),
    );
  }

  #world(paths: readonly string[]): Float64Array {
    if (paths.length === 0) return new Float64Array();
    return this.#stage.worldTransforms(paths, this.#time === undefined ? {} : { time: this.#time });
  }

  /** Reads item `id` of `source` again for what `what` says may have changed; `false` when it draws nothing any more. */
  #read(id: number, source: ItemSource, what: Dirty, isNew: boolean, gather: Gather): boolean {
    const stage = this.#stage;
    const previous = this.#entries.get(id);
    let item: DrawItem = previous?.item ?? {
      id,
      source,
      prim: sourcePath(source),
      buffer: -1,
      matrices: new Float64Array(),
      instances: [],
      ids: null,
      hidden: true,
      material: null,
      subsetMaterials: [],
    };
    let depends = previous?.depends ?? [];
    if (what.instances || what.transform || what.visibility || isNew) {
      let matrices: Float64Array = new Float64Array();
      let instances: string[] = [];
      let ids: BigInt64Array | null = null;
      if (source.kind === "gprim") {
        if (stage.prim(source.path) === null) return false;
        depends = [source.path];
        if (this.#shown([source.path])[0]) {
          gather.work.matricesComputed++;
          matrices = this.#world([source.path]);
          instances = [source.path];
        }
      } else if (source.kind === "prototype") {
        const first = this.#instanceRoot(source.prim);
        if (first === null || stage.prim(source.prim) === null) return false;
        const roots = this.#instancesOf(first);
        if (roots[0] !== first) return false;
        const everyProxy = proxies(first, source.prim, roots);
        const members = everyProxy.filter((proxy) => this.#material(proxy) === source.material);
        if (members.length === 0) return false;
        const shown = this.#shown(members);
        instances = members.filter((_, i) => shown[i]);
        gather.work.matricesComputed += instances.length;
        matrices = this.#world(instances);
        depends = [...everyProxy, ...roots];
      } else {
        const targets = stage.relationship(`${source.instancer}.prototypes`)?.targets() ?? [];
        const prototype = targets[source.index];
        if (prototype === undefined || !under(source.prim, prototype) || stage.prim(source.prim) === null) return false;
        depends = [source.instancer, prototype, source.prim];
        ids = new BigInt64Array();
        if (this.#shown([source.instancer])[0]) {
          const placed = stage.pointInstances(source.instancer, this.#time === undefined ? {} : { time: this.#time });
          const world = this.#world([source.instancer]);
          // A gprim inside the prototype: its transform relative to the
          // prototype's, which the instance matrices already hold.
          let inner: Float64Array | null = null;
          if (source.prim !== prototype) {
            const both = this.#world([source.prim, prototype]);
            inner = new Float64Array(16);
            multiply(both.subarray(0, 16), inverseAffine(both.subarray(16, 32)), inner, 0);
          }
          const mine: number[] = [];
          placed.prototypeIndices.forEach((index, i) => {
            if (index === source.index) mine.push(i);
          });
          matrices = new Float64Array(16 * mine.length);
          const scratch = new Float64Array(16);
          mine.forEach((i, k) => {
            const instance = placed.matrices.subarray(16 * i, 16 * i + 16);
            if (inner === null) {
              multiply(instance, world, matrices, 16 * k);
            } else {
              multiply(inner, instance, scratch, 0);
              multiply(scratch, world, matrices, 16 * k);
            }
          });
          ids = BigInt64Array.from(mine.map((i) => placed.ids[i]!));
          gather.work.matricesComputed += mine.length;
        }
      }
      if (!isNew) {
        if (!sameArray(matrices, item.matrices) || !sameArray(ids, item.ids) || instances.join("\n") !== item.instances.join("\n")) {
          gather.matrices.add(id);
        }
        if ((matrices.length === 0) !== item.hidden) gather.visibility.add(id);
      }
      item = { ...item, matrices, instances, ids, hidden: matrices.length === 0 };
    }
    if (what.geometry || isNew) {
      const key = this.#geometry(item, gather);
      if (key === null) {
        if (!isNew) this.#release(item.buffer, gather);
        return false;
      }
      if (key !== item.buffer) {
        if (!isNew) {
          gather.rebound.add(id);
          this.#release(item.buffer, gather);
        }
        this.#buffers.get(key)!.users++;
        item = { ...item, buffer: key };
      }
    }
    if (what.material || what.geometry || isNew) {
      const material = source.kind === "prototype" ? source.material : this.#material(item.prim);
      const subsets = (this.#buffers.get(item.buffer)?.buffers.subsets ?? []).map((subset) => this.#material(subset));
      if (!isNew && (material !== item.material || subsets.join("\n") !== item.subsetMaterials.join("\n"))) {
        gather.materials.add(id);
      }
      item = { ...item, material, subsetMaterials: subsets };
    }
    for (const subset of this.#buffers.get(item.buffer)?.buffers.subsets ?? []) {
      if (!depends.includes(subset)) depends = [...depends, subset];
    }
    if (previous === undefined) this.#keys.set(sourceKey(source), id);
    this.#entries.set(id, { item, depends });
    return true;
  }

  /** The buffer `item` draws, read again; `null` when its prim draws nothing. */
  #geometry(item: DrawItem, gather: Gather): number | null {
    const stage = this.#stage;
    const at = this.#time === undefined ? {} : { time: this.#time };
    const typeName = stage.prim(item.prim)?.typeName ?? null;
    if (typeName === "Mesh") {
      const input = readMesh(stage, item.prim, at);
      if (input === null) return null;
      const subsets = (input.subsets ?? []).map((subset) => subset.path ?? "");
      const known = this.#meshBuffers.get(item.prim);
      if (known !== undefined) {
        const retained = this.#buffers.get(known)!;
        const old = retained.input!;
        const mesh = retained.buffers.mesh;
        if (sameTopology(old, input)) {
          const samePoints = sameArray(old.points, input.points) && samePrimvar(old.normals, input.normals);
          const samePrimvars = samePrimvar(old.st, input.st) && samePrimvar(old.displayColor, input.displayColor);
          if (samePoints && samePrimvars) {
            gather.work.buffersReused++;
            retained.input = input;
            return known;
          }
          try {
            if (samePrimvars) mesh.refill(input.points, input.normals ?? null);
            else mesh.refillFrom(input);
            gather.work.refills++;
            gather.refilled.add(known);
            retained.input = input;
            retained.buffers = { ...retained.buffers, revision: retained.buffers.revision + 1 };
            return known;
          } catch (error) {
            if ((error as { code?: string }).code !== "plan.mismatch") throw error;
          }
        }
        mesh.dispose();
        gather.work.triangulations++;
        gather.rebuilt.add(known);
        retained.input = input;
        retained.buffers = {
          ...retained.buffers,
          mesh: this.#stagehand.buildMesh(input, this.#meshOptions),
          subsets,
          revision: retained.buffers.revision + 1,
        };
        return known;
      }
      gather.work.triangulations++;
      const key = this.#nextBuffer++;
      gather.buffersAdded.add(key);
      this.#buffers.set(key, {
        buffers: { key, prim: item.prim, mesh: this.#stagehand.buildMesh(input, this.#meshOptions), subsets, revision: 0, shape: null },
        input,
        users: 0,
      });
      this.#meshBuffers.set(item.prim, key);
      return key;
    }
    const shape = readShape(stage, item.prim, at);
    if (shape === null) return null;
    const shapeKey = JSON.stringify(shape);
    const shared = this.#shapeBuffers.get(shapeKey);
    if (shared !== undefined) {
      if (shared === item.buffer) gather.work.buffersReused++;
      return shared;
    }
    gather.work.tessellations++;
    const key = this.#nextBuffer++;
    gather.buffersAdded.add(key);
    this.#buffers.set(key, {
      buffers: { key, prim: item.prim, mesh: this.#stagehand.tessellate(shape, this.#tessellation), subsets: [], revision: 0, shape },
      input: null,
      users: 0,
    });
    this.#shapeBuffers.set(shapeKey, key);
    return key;
  }

  #release(key: number, gather: Gather): void {
    const retained = this.#buffers.get(key);
    if (retained === undefined) return;
    retained.users = Math.max(0, retained.users - 1);
    if (retained.users > 0) return;
    this.#buffers.delete(key);
    retained.buffers.mesh.dispose();
    if (retained.buffers.shape !== null) this.#shapeBuffers.delete(JSON.stringify(retained.buffers.shape));
    else this.#meshBuffers.delete(retained.buffers.prim);
    gather.freed.add(key);
    gather.buffersAdded.delete(key);
  }

  #remove(id: number, gather: Gather): void {
    const entry = this.#entries.get(id);
    if (entry === undefined) return;
    this.#entries.delete(id);
    this.#keys.delete(sourceKey(entry.item.source));
    this.#release(entry.item.buffer, gather);
    if (!gather.added.delete(id)) gather.removed.add(id);
  }
}

/** The prim at `path` (under the instance root `first`) in each instance of `roots`. */
function proxies(first: string, path: string, roots: readonly string[]): string[] {
  const relative = path.slice(first.length);
  return roots.map((root) => `${root}${relative}`);
}
