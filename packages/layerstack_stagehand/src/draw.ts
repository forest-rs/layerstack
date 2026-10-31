// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The retained draw list: what a renderer draws from a stage, grouped into
// instanced draws and kept up to date from change records. It mirrors
// `layerstack_stagehand::DrawList`, reading the stage through
// `@forest-rs/layerstack` and building buffers in this package's module.
import type { Changes, PointInstances, Prim, Stage } from "@forest-rs/layerstack";
import type { Stagehand, StagehandMesh } from "./index.js";
import { CAMERA_INPUTS, LIGHT_INPUTS, lightKey, lightKind, readLight } from "./light.js";
import type { CameraItem, LightInstance, LightItem, LightKind } from "./light.js";
import { readMaterial } from "./material.js";
import type { MaterialItem } from "./material.js";
import { readMesh, readShape, shapeSchema } from "./read.js";
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
  /**
   * The render contexts whose material outputs are read, in order
   * (`["mtlx"]` reads `outputs:mtlx:surface` first); the universal
   * `outputs:surface` after them, or alone when empty.
   */
  readonly renderContexts?: readonly string[];
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

/**
 * One instance of a point instancer's item: the item, and the instance's
 * id ({@link DrawItem.ids}): its `ids` entry, or its position in the
 * instancer's arrays when `ids` is unauthored.
 */
export interface ItemInstance {
  readonly item: number;
  readonly id: bigint;
}

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
  /** Items {@link DrawList.setTime} did not read because nothing they read might differ at another time; counted in {@link itemsUntouched} too. */
  readonly itemsTimeInvariant: number;
  /**
   * Point instancers whose instances, and the lights they place, were
   * derived again: each one a change discovered, and each one
   * {@link DrawList.setTime} read because which instances it places might
   * differ at another time. One whose instances only move is not.
   */
  readonly instancersRederived: number;
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
  /** Cameras read: computed again. */
  readonly camerasRead: number;
  /** Lights read again. */
  readonly lightsRead: number;
  /** Materials read: their networks walked and their records built. */
  readonly materialsRead: number;
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
  /** Instances a point instancer's item draws now and did not before: new in its `protoIndices` or `ids`, unmasked, or shown; sorted by item, then id. Their items are in {@link matrices} too. */
  readonly instancesAdded: readonly ItemInstance[];
  /** Instances a point instancer's item no longer draws: gone from its arrays, masked (`invisibleIds`, `inactiveIds`) or hidden. Their items are in {@link matrices} too. */
  readonly instancesRemoved: readonly ItemInstance[];
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
  readonly camerasAdded: readonly string[];
  readonly camerasRemoved: readonly string[];
  /** Cameras read again and found different: moved, or a lens changed. */
  readonly camerasChanged: readonly string[];
  /** New lights, by key ({@link LightItem.key}). */
  readonly lightsAdded: readonly string[];
  readonly lightsRemoved: readonly string[];
  /** Lights read again and found different. */
  readonly lightsChanged: readonly string[];
  /** Materials newly bound by an item. */
  readonly materialsAdded: readonly string[];
  /** Materials no item binds any more, or no longer materials. */
  readonly materialsRemoved: readonly string[];
  /** Materials read again and found different: an input, a connection, a texture's file or sampling. */
  readonly materialsChanged: readonly string[];
  readonly work: DrawWork;
}

/** What a prim's schema makes of it for a draw list, by `IsA`: a drawn gprim, a point instancer, a face subset. */
interface Kind {
  readonly gprim: "mesh" | "shape" | null;
  readonly instancer: boolean;
  readonly subset: boolean;
  readonly camera: boolean;
  readonly light: LightKind | null;
}

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

/** The attributes a mesh's or a shape's buffer is read from: points, normals, topology, the drawn primvars and their indices, a shape's size. */
const GEOMETRY_INPUTS = [
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
  "primvars:normals",
  "primvars:normals:indices",
  "primvars:st",
  "primvars:st:indices",
  "primvars:displayColor",
  "primvars:displayColor:indices",
];

/** A `GeomSubset`'s attributes, read by its mesh. */
const SUBSET_INPUTS = ["indices", "elementType", "familyName"];

/** The attributes a `PointInstancer` places its instances from. */
const INSTANCER_INPUTS = [
  "protoIndices",
  "ids",
  "positions",
  "orientations",
  "orientationsf",
  "scales",
  "velocities",
  "accelerations",
  "angularVelocities",
  "invisibleIds",
];

/**
 * The attributes that decide which instances a point instancer places, and
 * of which prototype, as `layerstack_stagehand` reads them: `protoIndices`,
 * `ids`, `positions` (none placed when their count is not
 * `protoIndices`') and `invisibleIds`. `prototypes` is a relationship and
 * `inactiveIds` prim metadata: neither has time samples.
 */
const INSTANCE_SET_INPUTS = ["protoIndices", "ids", "positions", "invisibleIds"];

const GEOMETRY = new Set([...GEOMETRY_INPUTS, ...SUBSET_INPUTS]);

/** Whether a camera or light reading `inputs` reads its own property `name`, as `layerstack_stagehand` decides it. */
function reads(inputs: readonly string[], name: string): boolean {
  return (
    inputs.includes(name) ||
    name === "treatAsPoint" ||
    name === "treatAsLine" ||
    name === "poleAxis" ||
    name.startsWith("collection:lightLink:") ||
    name.startsWith("collection:shadowLink:")
  );
}

/** What a property's change may change of the items under its prim, as `layerstack_stagehand` classifies it. */
function propertyDirt(name: string): Dirty {
  const dirty = none();
  if (name === "xformOpOrder" || name.startsWith("xformOp:")) dirty.transform = true;
  else if (name === "visibility" || name === "purpose" || name.endsWith("Visibility")) dirty.visibility = true;
  else if (name.startsWith("material:binding")) dirty.material = true;
  else if (GEOMETRY.has(name)) dirty.geometry = true;
  return dirty;
}

/** What of an item might differ at another time, as last read: what {@link DrawList.setTime} reads again. */
interface Varies {
  /** Its matrices: a drawn instance's world transform, or an instancer's placement. */
  matrices: boolean;
  /** Whether it, or any instance, is shown. */
  visibility: boolean;
  /** Its buffer's inputs. */
  geometry: boolean;
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

/** A light discovery found: its path, the point instancer's instance placing it, its kind. */
interface FoundLight {
  readonly path: string;
  readonly instance: LightInstance | null;
  readonly kind: LightKind;
}

interface Found {
  sources: ItemSource[];
  scopes: Scope[];
  cameras: string[];
  /** By key ({@link lightKey}). */
  lights: Map<string, FoundLight>;
  instancers: Map<string, Instancer>;
  prims: number;
}

/** A point instancer's prototypes and its unmasked instances, by position: prototype index and matrix. */
interface Placed {
  readonly prototypes: readonly string[];
  /** By the instance's key ({@link instanceKey}): its prototype index, matrix and instance. */
  readonly instances: Map<string, [number, Float64Array, LightInstance]>;
}

/** A point instancer as last discovered: the lights inside its prototypes (prototype index, path, kind), and whether which instances it places might differ at another time. */
interface Instancer {
  readonly lights: readonly [number, string, LightKind][];
  readonly varies: boolean;
}

/**
 * Each of a point instancer's unmasked instances ({@link Stage.pointInstances}
 * at `time`) as a {@link LightInstance}: by its `ids` entry when the
 * instancer authors `ids` and they are distinct, by its position
 * otherwise, as `layerstack_stagehand` keys them.
 */
function placedInstances(stage: Stage, instancer: string, placed: PointInstances, time: number | undefined): LightInstance[] {
  // Whether `ids` has a value at `time`; its elements are `placed.ids`.
  const ids = stage.attribute(`${instancer}.ids`)?.get(time) ?? null;
  if (ids?.kind === "array") ids.array.release();
  const authored = ids !== null && new Set(placed.ids).size === placed.ids.length;
  return Array.from(placed.indices, (index, i) =>
    Object.freeze(authored ? { instancer, id: placed.ids[i]! } : { instancer, index }),
  );
}

/** An instance's part of its {@link lightKey}. */
function instanceKey(instance: LightInstance): string {
  return "id" in instance ? `#${instance.id}` : `[${instance.index}]`;
}

/**
 * A camera, a light or a material, whether what it reads might differ at
 * another time, and the other prims it reads (a light's connection
 * sources, a material's shaders and node graphs), as last read.
 */
interface Held<T> {
  item: T;
  varies: boolean;
  sources: readonly string[];
}

/** Whether a property name is a shading port or a shader's identity: what a material or a connected light input reads of a shading prim. */
function shadingProperty(name: string): boolean {
  return name.startsWith("inputs:") || name.startsWith("outputs:") || name.startsWith("info:");
}

/**
 * Whether an info-only change of `prim` reaches nothing a draw list reads:
 * the pseudo-root's, whose fields are the stage's metadata
 * (`documentation`, `customLayerData`, the start and end time codes,
 * `metersPerUnit`, `upAxis`, or a sublayer's `subLayers`, whose prims are
 * listed on their own). No buffer, matrix, camera, light or material reads
 * them; a renderer that scales or orients the scene by `metersPerUnit` or
 * `upAxis` reads them off the stage again. As `layerstack_stagehand`
 * decides it.
 */
function stageMetadata(prim: string): boolean {
  return prim === "/";
}

/** Whether `changes` reach the shading properties of the prim at `prim`, as `layerstack_stagehand` decides it. */
function shadingTouched(changes: Pick<Changes, "changedInfoOnly" | "properties" | "resyncedProperties">, roots: readonly string[], prim: string): boolean {
  return (
    roots.some((root) => under(prim, root)) ||
    changes.changedInfoOnly.some((changed) => {
      if (stageMetadata(changed)) return false;
      const inventory = changes.properties.find((properties) => properties.prim === changed);
      return inventory === undefined ? under(prim, changed) : changed === prim && inventory.fields.some((field) => shadingProperty(field.property));
    }) ||
    changes.resyncedProperties.some((property) => {
      const dot = property.lastIndexOf(".");
      return property.slice(0, dot) === prim && shadingProperty(property.slice(dot + 1));
    })
  );
}

/** Cameras or lights added, removed and changed in the diff being gathered. */
class Changed {
  added = new Set<string>();
  removed = new Set<string>();
  changed = new Set<string>();

  finish(): [string[], string[], string[]] {
    for (const path of this.added) this.changed.delete(path);
    const sortedPaths = (set: Set<string>) => [...set].sort();
    return [sortedPaths(this.added), sortedPaths(this.removed), sortedPaths(this.changed)];
  }
}

/** Whether two records hold the same values, typed arrays included. */
function sameRecord(a: unknown, b: unknown): boolean {
  const plain = (_: string, value: unknown) =>
    ArrayBuffer.isView(value) ? Array.from(value as Float64Array) : typeof value === "bigint" ? `${value}n` : value;
  return JSON.stringify(a, plain) === JSON.stringify(b, plain);
}

interface Entry {
  item: DrawItem;
  /** Every path whose change reaches the item. */
  depends: string[];
  /** What of it might differ at another time. */
  varies: Varies;
}

interface Retained {
  buffers: DrawBuffers;
  /** The mesh as last read, for a mesh. */
  input: MeshInput | null;
  /** The texture coordinate sets besides `st` its items' materials read, sorted: those it was read with. */
  uvSets: readonly string[];
  users: number;
}

/** The diff being gathered. */
class Gather {
  added = new Set<number>();
  removed = new Set<number>();
  matrices = new Set<number>();
  visibility = new Set<number>();
  /** By `${item}#${id}`. */
  instancesAdded = new Map<string, ItemInstance>();
  instancesRemoved = new Map<string, ItemInstance>();
  materials = new Set<number>();
  rebound = new Set<number>();
  buffersAdded = new Set<number>();
  refilled = new Set<number>();
  rebuilt = new Set<number>();
  freed = new Set<number>();
  cameras = new Changed();
  lights = new Changed();
  shading = new Changed();
  /** Materials read in this diff, so none is read twice. */
  materialsRead = new Set<string>();
  /** The materials the native instancing prototype groups this diff adds bind, by gprim: a new mesh buffer is planned once with the sets all of its groups read. */
  groups = new Map<string, string[]>();
  work = {
    primsTraversed: 0,
    itemsRead: 0,
    itemsUntouched: 0,
    itemsTimeInvariant: 0,
    instancersRederived: 0,
    matricesComputed: 0,
    localTransforms: 0,
    buffersReused: 0,
    refills: 0,
    triangulations: 0,
    tessellations: 0,
    camerasRead: 0,
    lightsRead: 0,
    materialsRead: 0,
  };
}

const sorted = (set: Set<number>) => [...set].sort((a, b) => a - b);

/** The instances, sorted by item then id, but those of the items in `skip`. */
function sortedInstances(instances: Map<string, ItemInstance>, skip: (item: number) => boolean): ItemInstance[] {
  return [...instances.values()]
    .filter((instance) => !skip(instance.item))
    .sort((a, b) => a.item - b.item || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

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
 * - Cameras ({@link CameraItem}) and lights ({@link LightItem}) are held
 *   by path beside the items, and diffed as added, removed and changed.
 * - Materials ({@link MaterialItem}) are held for the materials the items
 *   bind, by path, and diffed the same way: one is read when an item comes
 *   to bind it, and again only when a shading property (`inputs:*`,
 *   `outputs:*`, `info:*`) of it or of a prim its network reads changes.
 * - Diffs ({@link DrawDiff}): {@link refresh} reads the whole stage;
 *   {@link applyChanges} reads what a change record reaches (resynced
 *   paths rediscover their subtree; changed properties are classified by
 *   name into transform, visibility, material and geometry work under
 *   their prim; the pseudo-root changed info-only, which is the stage's
 *   metadata, reads nothing); {@link setTime} reads at a new time only the items
 *   whose inputs might vary with time, and rebuilds only what changed.
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
  readonly #cameras = new Map<string, Held<CameraItem>>();
  readonly #lights = new Map<string, Held<LightItem>>();
  readonly #materials = new Map<string, Held<MaterialItem>>();
  /** The point instancers discovered, by path. */
  readonly #instancers = new Map<string, Instancer>();
  readonly #contexts: readonly string[];
  /** Each prim type's {@link Kind}: `IsA` depends on the type alone. */
  readonly #kinds = new Map<string, Kind>();
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
    this.#contexts = [...(options.renderContexts ?? [])];
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

  /** The cameras ({@link CameraItem}), by path. */
  cameras(): CameraItem[] {
    return [...this.#cameras.values()].map((held) => held.item);
  }

  /** The camera at `path`, while it is listed. */
  camera(path: string): CameraItem | null {
    return this.#cameras.get(path)?.item ?? null;
  }

  /**
   * The lights ({@link LightItem}), by key: a light inside a native
   * instance once per instance (its instance proxy's path), one inside a
   * point instancer's prototype once per unmasked instance.
   */
  lights(): LightItem[] {
    return [...this.#lights.values()].map((held) => held.item);
  }

  /** The light of `key` ({@link LightItem.key}: its path, for one no point instancer places), while it is listed. */
  light(key: string): LightItem | null {
    return this.#lights.get(key)?.item ?? null;
  }

  /** The materials the items bind ({@link MaterialItem}), by path. */
  materials(): MaterialItem[] {
    return [...this.#materials.values()].map((held) => held.item);
  }

  /** The material at `path`, while an item binds it. */
  material(path: string): MaterialItem | null {
    return this.#materials.get(path)?.item ?? null;
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

  /**
   * Reads at `time` (`undefined` for the default time) the items whose
   * inputs might differ at another time, and only what of them might; an
   * animated mesh is refilled, never triangulated again unless its topology
   * changed. An item's inputs might vary when time samples (even one), a
   * spline or value clips supply them
   * ({@link Stage.valueSourcesMightBeTimeVarying}): its geometry's
   * attributes (and its face subsets'), the `visibility` of it or an
   * ancestor, its world transform's chain
   * ({@link Stage.worldTransformsMightVary}), or a point instancer's
   * placement attributes. The rest are left untouched and counted in
   * {@link DrawWork.itemsTimeInvariant}. What might vary is found as the
   * item is read, so {@link applyChanges} keeps it up to date. Cameras,
   * lights and materials whose inputs might vary are read again too; when
   * a material's `UsdPrimvarReader_float2` `varname` names other sets at
   * `time`, the meshes drawing them are planned again, listed in
   * {@link DrawDiff.rebuilt}.
   */
  setTime(time: number | undefined): DrawDiff {
    const before = this.#stage.transformCache().localComputed;
    this.#time = time;
    const gather = new Gather();
    const read = new Set<number>();
    for (const [id, entry] of [...this.#entries]) {
      const { matrices, visibility, geometry } = entry.varies;
      const what: Dirty = { instances: matrices, transform: matrices, visibility, geometry, material: false };
      if (!any(what)) {
        gather.work.itemsTimeInvariant++;
        continue;
      }
      read.add(id);
      if (!this.#read(id, entry.item.source, what, false, gather)) this.#remove(id, gather);
    }
    for (const [path, held] of [...this.#cameras]) if (held.varies) this.#readCamera(path, gather);
    // A point instancer whose instances might differ places its lights
    // again: those of instances gone or masked are removed, those of new or
    // unmasked ones added.
    const placements = new Map<string, Placed | null>();
    const placing = new Map<string, FoundLight>();
    for (const [instancer, held] of [...this.#instancers]) {
      if (!held.varies) continue;
      gather.work.instancersRederived++;
      if (held.lights.length === 0) continue;
      const mine = new Map<string, FoundLight>();
      this.#placeLights(instancer, held.lights, placements, mine);
      for (const [key, light] of [...this.#lights]) {
        if (light.item.instance?.instancer === instancer && !mine.has(key)) {
          this.#lights.delete(key);
          gather.lights.removed.add(key);
        }
      }
      for (const [key, light] of mine) placing.set(key, light);
    }
    const lights = new Map<string, FoundLight>();
    for (const [key, held] of this.#lights) {
      const { path, instance, kind } = held.item;
      if (held.varies) lights.set(key, { path, instance, kind });
    }
    for (const [key, light] of placing) lights.set(key, light);
    for (const [, light] of [...lights].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) this.#readLight(light, placements, gather);
    for (const [path, held] of [...this.#materials]) if (held.varies) this.#readMaterial(path, gather);
    // A `varname` read at the new time may name other sets.
    this.#applyToUvSets(gather);
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
    this.#cameras.clear();
    this.#lights.clear();
    this.#materials.clear();
    this.#instancers.clear();
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
    const found: Found = { sources: [], scopes: [], cameras: [], lights: new Map(), instancers: new Map(), prims: 0 };
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
      if (stageMetadata(prim)) continue;
      const inventory = changes.properties.find((changed) => changed.prim === prim);
      const what = inventory === undefined ? all() : none();
      for (const field of inventory?.fields ?? []) merge(what, this.#propertyDirt(field.property));
      reached.push([prim, what]);
    }
    for (const property of changes.resyncedProperties) {
      const dot = property.lastIndexOf(".");
      reached.push([property.slice(0, dot), this.#propertyDirt(property.slice(dot + 1))]);
    }
    const prototypes = new Set<string>();
    for (const [prim, what] of reached) {
      const kind = this.#kind(stage.prim(prim));
      const instancer = kind.instancer;
      if (instancer) {
        found.scopes.push({ kind: "instancer", path: prim });
        this.#discoverInstancer(prim, found);
      }
      const subsetOf = kind.subset ? parent(prim) : null;
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
      this.#discoverPrototype(first, found, false);
    }
    gather.work.primsTraversed = found.prims;
    gather.work.instancersRederived = found.instancers.size;
    this.#applyToViews(changes, found, gather);
    // The instancers discovered replace those the discovery covered.
    for (const path of [...this.#instancers.keys()]) {
      const covered = found.scopes.some((scope) => (scope.kind === "resync" ? under(path, scope.root) : scope.kind === "instancer" && scope.path === path));
      if (covered) this.#instancers.delete(path);
    }
    for (const [path, instancer] of found.instancers) this.#instancers.set(path, instancer);
    // New items first, so a buffer a gone item shares with a new one (a
    // renamed prim's shape) is kept rather than freed and made again.
    const seen = new Set(found.sources.map(sourceKey));
    const gone = [...this.#entries]
      .filter(([, entry]) => !seen.has(sourceKey(entry.item.source)) && found.scopes.some((scope) => covers(scope, entry.item.source)))
      .map(([id]) => id);
    for (const source of found.sources) {
      if (source.kind !== "prototype" || source.material === null || this.#keys.has(sourceKey(source))) continue;
      const materials = gather.groups.get(source.prim) ?? [];
      if (!materials.includes(source.material)) gather.groups.set(source.prim, [...materials, source.material]);
    }
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
    this.#applyToMaterials(changes, found.scopes.flatMap((scope) => (scope.kind === "resync" ? [scope.root] : [])), gather);
    return this.#finish(gather, read, before);
  }

  /**
   * Brings the materials up to the stage once the items are, as
   * `layerstack_stagehand` does: one no item binds any more is dropped, a
   * newly bound one read, and a held one read again when a shading
   * property of it, or of a prim its network reads, changes.
   */
  #applyToMaterials(changes: Pick<Changes, "changedInfoOnly" | "properties" | "resyncedProperties">, roots: readonly string[], gather: Gather): void {
    const bound = new Set<string>();
    for (const entry of this.#entries.values()) {
      if (entry.item.material !== null) bound.add(entry.item.material);
      for (const material of entry.item.subsetMaterials) if (material !== null) bound.add(material);
    }
    for (const path of [...this.#materials.keys()]) {
      if (!bound.has(path)) {
        this.#materials.delete(path);
        gather.shading.removed.add(path);
      }
    }
    for (const path of [...bound].sort()) {
      if (gather.materialsRead.has(path)) continue;
      const held = this.#materials.get(path);
      if (held === undefined || [path, ...held.sources].some((prim) => shadingTouched(changes, roots, prim))) this.#readMaterial(path, gather);
    }
    this.#applyToUvSets(gather);
  }

  /**
   * Plans again the meshes whose items' materials now read other texture
   * coordinate sets besides `st`, listed as rebuilt, as
   * `layerstack_stagehand` does; only when a material or a binding
   * changed, or an item came.
   */
  #applyToUvSets(gather: Gather): void {
    const { added, changed, removed } = gather.shading;
    if (added.size + changed.size + removed.size + gather.materials.size + gather.added.size === 0) return;
    const wanted = new Map<number, Set<string>>();
    for (const entry of this.#entries.values()) {
      const retained = this.#buffers.get(entry.item.buffer);
      if (retained === undefined || retained.input === null) continue;
      let names = wanted.get(entry.item.buffer);
      if (names === undefined) wanted.set(entry.item.buffer, (names = new Set()));
      for (const material of [entry.item.material, ...entry.item.subsetMaterials]) {
        const held = material === null ? undefined : this.#materials.get(material);
        for (const name of held === undefined ? [] : uvSetsRead(held.item)) names.add(name);
      }
    }
    const at = this.#time === undefined ? {} : { time: this.#time };
    const planned = new Set<number>();
    for (const [key, set] of wanted) {
      const names = [...set].sort();
      const retained = this.#buffers.get(key)!;
      if (names.join("\n") === retained.uvSets.join("\n")) continue;
      const input = readMesh(this.#stage, retained.buffers.prim, { ...at, uvSets: names });
      if (input === null) continue;
      retained.buffers.mesh.dispose();
      gather.work.triangulations++;
      gather.rebuilt.add(key);
      retained.input = input;
      retained.uvSets = names;
      retained.buffers = {
        ...retained.buffers,
        mesh: this.#stagehand.buildMesh(input, this.#meshOptions),
        subsets: (input.subsets ?? []).map((subset) => subset.path ?? ""),
        revision: retained.buffers.revision + 1,
      };
      planned.add(key);
    }
    // The sets drawn now might vary where the old ones did not.
    for (const entry of this.#entries.values()) {
      if (planned.has(entry.item.buffer)) entry.varies = { ...entry.varies, geometry: this.#geometryMightVary(entry.item.prim, entry.item.buffer) };
    }
  }

  /**
   * The texture coordinate sets besides `st` the materials bound to a new
   * mesh item read (its own or its prototype group's, and its
   * `materialBind` subsets'); a material not held yet is read now, once
   * for the diff, so the mesh is planned once with them.
   */
  #uvSetsFor(item: DrawItem, gather: Gather): string[] {
    const prim = this.#stage.prim(item.prim);
    const materials = [item.source.kind === "prototype" ? item.source.material : this.#material(item.prim)];
    if (item.source.kind === "prototype") materials.push(...(gather.groups.get(item.source.prim) ?? []));
    for (const subset of prim?.geomSubsets({ elementType: "face", familyName: "materialBind" }) ?? []) {
      materials.push(this.#material(subset.path));
      subset.indices?.release();
    }
    const names = new Set<string>();
    for (const material of materials) {
      if (material === null) continue;
      if (!this.#materials.has(material) && !gather.materialsRead.has(material)) this.#readMaterial(material, gather);
      const held = this.#materials.get(material);
      for (const name of held === undefined ? [] : uvSetsRead(held.item)) names.add(name);
    }
    return [...names].sort();
  }

  /** Whether the geometry `prim` draws into the buffer `key` might differ at another time: its attributes, its face subsets', or a texture coordinate set the buffer draws. */
  #geometryMightVary(prim: string, key: number): boolean {
    const retained = this.#buffers.get(key);
    const uvSets = retained?.uvSets ?? [];
    return (
      this.#inputsMightVary([prim], GEOMETRY_INPUTS) ||
      this.#inputsMightVary(retained?.buffers.subsets ?? [], SUBSET_INPUTS) ||
      this.#inputsMightVary([prim], uvSets.flatMap((name) => [`primvars:${name}`, `primvars:${name}:indices`]))
    );
  }

  /** Reads the material at `path` again ({@link readMaterial}) and records whether it changed. */
  #readMaterial(path: string, gather: Gather): void {
    gather.work.materialsRead++;
    gather.materialsRead.add(path);
    const reading = readMaterial(this.#stage, path, this.#time, this.#contexts);
    if (reading === null) {
      if (this.#materials.delete(path)) gather.shading.removed.add(path);
      return;
    }
    const attributes = reading.attributes;
    const varies = attributes.length > 0 && this.#stage.valueSourcesMightBeTimeVarying(attributes).includes(true);
    const old = this.#materials.get(path);
    this.#materials.set(path, { item: reading.item, varies, sources: reading.sources });
    if (old === undefined) gather.shading.added.add(path);
    else if (!sameRecord(old.item, reading.item)) gather.shading.changed.add(path);
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
    // An added or removed item's instances are not listed.
    const skip = (item: number) => gather.added.has(item) || gather.removed.has(item);
    const [camerasAdded, camerasRemoved, camerasChanged] = gather.cameras.finish();
    const [lightsAdded, lightsRemoved, lightsChanged] = gather.lights.finish();
    const [materialsAdded, materialsRemoved, materialsChanged] = gather.shading.finish();
    const live = [...read].filter((id) => this.#entries.has(id));
    gather.work.itemsRead = live.length;
    gather.work.itemsUntouched = this.#entries.size - live.length;
    gather.work.localTransforms = this.#stage.transformCache().localComputed - localBefore;
    return {
      added: sorted(gather.added),
      removed: sorted(gather.removed),
      matrices: sorted(gather.matrices),
      visibility: sorted(gather.visibility),
      instancesAdded: sortedInstances(gather.instancesAdded, skip),
      instancesRemoved: sortedInstances(gather.instancesRemoved, skip),
      materials: sorted(gather.materials),
      rebound: sorted(gather.rebound),
      buffersAdded: sorted(gather.buffersAdded),
      refilled: sorted(gather.refilled),
      rebuilt: sorted(gather.rebuilt),
      freed: sorted(gather.freed),
      camerasAdded,
      camerasRemoved,
      camerasChanged,
      lightsAdded,
      lightsRemoved,
      lightsChanged,
      materialsAdded,
      materialsRemoved,
      materialsChanged,
      work: { ...gather.work },
    };
  }

  /**
   * What `prim`'s schema makes of it, by `IsA`, as `layerstack_stagehand`
   * classifies a prim: a schema derived from `Mesh` or a shape is drawn
   * as one. Cached by type name.
   */
  #kind(prim: Prim | null): Kind {
    if (prim === null) return { gprim: null, instancer: false, subset: false, camera: false, light: null };
    const key = prim.typeName ?? "";
    let kind = this.#kinds.get(key);
    if (kind === undefined) {
      const mesh = prim.typeName === "Mesh" || prim.isA("Mesh");
      kind = {
        gprim: mesh ? "mesh" : shapeSchema(prim) !== null ? "shape" : null,
        instancer: prim.typeName === "PointInstancer" || prim.isA("PointInstancer"),
        subset: prim.typeName === "GeomSubset" || prim.isA("GeomSubset"),
        camera: prim.typeName === "Camera" || prim.isA("Camera"),
        light: lightKind(prim),
      };
      this.#kinds.set(key, kind);
    }
    // A mesh is a light by its applied API schemas, not its type.
    return kind.gprim === "mesh" ? { ...kind, light: lightKind(prim) } : kind;
  }

  /** {@link propertyDirt}, and a texture coordinate set a buffer is drawn with (`primvars:<name>`, its `:indices`) as geometry. */
  #propertyDirt(name: string): Dirty {
    const dirty = propertyDirt(name);
    if (name.startsWith("primvars:")) {
      const set = name.slice("primvars:".length).replace(/:indices$/, "");
      for (const retained of this.#buffers.values()) if (retained.uvSets.includes(set)) dirty.geometry = true;
    }
    return dirty;
  }

  #drawn(prim: Prim): boolean {
    return this.#kind(prim).gprim !== null;
  }

  #discover(root: string, firsts: Map<string, string>, found: Found): void {
    const stage = this.#stage;
    // The root may be inside an instance or a point instancer: discover from the outermost one.
    let start = root;
    for (let at = parent(root); at !== null; at = parent(at)) {
      const prim = stage.prim(at);
      if (prim !== null && (prim.instance || this.#kind(prim).instancer)) start = at;
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
      if (this.#kind(prim).instancer) {
        found.scopes.push({ kind: "instancer", path });
        this.#discoverInstancer(path, found);
        continue;
      }
      if (prim.instance) {
        const first = firsts.get(path);
        if (first !== undefined) prototypes.add(first);
        continue;
      }
      const kind = this.#kind(prim);
      if (kind.gprim !== null) found.sources.push({ kind: "gprim", path });
      // A mesh light is a gprim drawn and a light listed.
      if (kind.camera) found.cameras.push(path);
      else if (kind.light !== null) found.lights.set(path, { path, instance: null, kind: kind.light });
      stack.push(...prim.children().reverse());
    }
    for (const first of prototypes) {
      found.scopes.push({ kind: "prototype", first });
      this.#discoverPrototype(first, found, true);
    }
  }

  /**
   * Brings the cameras and lights up to the stage after `changes`, given
   * what discovery found, as `layerstack_stagehand` does: new ones are
   * added, those a resync covers but did not find removed, and those a
   * change reaches read again (an input it reads, its transform or
   * visibility or one above it, an unlisted change of it or above it, a
   * resync over it).
   */
  #applyToViews(
    changes: Pick<Changes, "resynced" | "changedInfoOnly" | "properties" | "resyncedProperties">,
    found: Found,
    gather: Gather,
  ): void {
    const roots = found.scopes.flatMap((scope) => (scope.kind === "resync" ? [scope.root] : []));
    const resynced = (path: string) => roots.some((root) => under(path, root));
    // The properties changed on each prim; `null` when the record has no
    // inventory of them, which reaches everything under the prim.
    const reached: [string, string[] | null][] = [];
    for (const prim of changes.changedInfoOnly) {
      if (stageMetadata(prim)) continue;
      const inventory = changes.properties.find((changed) => changed.prim === prim);
      reached.push([prim, inventory === undefined ? null : inventory.fields.map((field) => field.property)]);
    }
    for (const property of changes.resyncedProperties) {
      const dot = property.lastIndexOf(".");
      reached.push([property.slice(0, dot), [property.slice(dot + 1)]]);
    }
    const placement = (name: string) => {
      const dirt = propertyDirt(name);
      return dirt.transform || dirt.visibility;
    };
    // A camera or light reads its own inputs, and the transform and
    // visibility of its prim and the prims above it.
    const reaches = (path: string, inputs: readonly string[]) =>
      resynced(path) ||
      reached.some(([prim, names]) =>
        names === null
          ? under(path, prim)
          : prim === path
            ? names.some((name) => placement(name) || reads(inputs, name))
            : under(path, prim) && names.some(placement),
      );

    const cameras = new Set(found.cameras);
    for (const path of [...this.#cameras.keys()]) {
      if (resynced(path) && !cameras.has(path)) {
        this.#cameras.delete(path);
        gather.cameras.removed.add(path);
      }
    }
    for (const path of this.#cameras.keys()) if (reaches(path, CAMERA_INPUTS)) cameras.add(path);
    for (const path of [...cameras].sort()) this.#readCamera(path, gather);

    // A point instancer's discovery covers the lights it places.
    const instancers = new Set(found.scopes.flatMap((scope) => (scope.kind === "instancer" ? [scope.path] : [])));
    const lights = new Map(found.lights);
    for (const [key, held] of [...this.#lights]) {
      const { path, instance } = held.item;
      if (!lights.has(key) && (resynced(path) || (instance !== null && instancers.has(instance.instancer)))) {
        this.#lights.delete(key);
        gather.lights.removed.add(key);
      }
    }
    for (const [key, held] of this.#lights) {
      const touched = reaches(held.item.path, LIGHT_INPUTS) || held.sources.some((source) => shadingTouched(changes, roots, source));
      if (touched && !lights.has(key)) lights.set(key, { path: held.item.path, instance: held.item.instance, kind: held.item.kind });
    }
    const placements = new Map<string, Placed | null>();
    for (const [, light] of [...lights].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) this.#readLight(light, placements, gather);
  }

  /** Computes the camera at `path` again ({@link Stage.camera}) and records whether it changed. */
  #readCamera(path: string, gather: Gather): void {
    gather.work.camerasRead++;
    const prim = this.#stage.prim(path);
    if (prim === null || !this.#kind(prim).camera) {
      if (this.#cameras.delete(path)) gather.cameras.removed.add(path);
      return;
    }
    let item: CameraItem;
    try {
      item = Object.freeze({ path, camera: this.#stage.camera(path, this.#time === undefined ? {} : { time: this.#time }), error: null });
    } catch (error) {
      if ((error as { code?: string }).code !== "camera.invalid") throw error;
      item = Object.freeze({ path, camera: null, error: (error as Error).message });
    }
    const varies = this.#worldMightVary([path]) || this.#inputsMightVary([path], CAMERA_INPUTS);
    const old = this.#cameras.get(path);
    this.#cameras.set(path, { item, varies, sources: [] });
    if (old === undefined) gather.cameras.added.add(path);
    else if (!sameRecord(old.item, item)) gather.cameras.changed.add(path);
  }

  /**
   * Reads the light `light` again ({@link readLight}) and records whether
   * it changed; drops it when its prim, or the instance placing it, is
   * gone.
   */
  #readLight(light: FoundLight, placements: Map<string, Placed | null>, gather: Gather): void {
    gather.work.lightsRead++;
    const { path, instance, kind } = light;
    const key = lightKey(path, instance);
    const matrix =
      this.#stage.prim(path) === null ? null : instance === null ? this.#world([path]) : this.#placedMatrix(path, instance, placements);
    if (matrix === null) {
      if (this.#lights.delete(key)) gather.lights.removed.add(key);
      return;
    }
    const at = this.#time === undefined ? {} : { time: this.#time };
    const visibility = this.#stage.visibilities(instance === null ? [path] : [path, instance.instancer], at);
    const hidden = visibility.includes("invisible");
    const sources: string[] = [];
    const item = readLight(this.#stage, path, kind, this.#time, matrix, hidden, sources, instance);
    // A connected input's source might vary as well as its own value; a
    // placed light moves with its instancer's placement.
    const varies =
      this.#worldMightVary([path]) ||
      this.#visibilityMightVary([path]) ||
      this.#inputsMightVary([path], LIGHT_INPUTS) ||
      this.#shadingMightVary(sources) ||
      (instance !== null &&
        (this.#inputsMightVary([instance.instancer], INSTANCER_INPUTS) ||
          this.#visibilityMightVary([instance.instancer]) ||
          this.#worldMightVary([instance.instancer])));
    const old = this.#lights.get(key);
    this.#lights.set(key, { item, varies, sources });
    if (old === undefined) gather.lights.added.add(key);
    else if (!sameRecord(old.item, item)) gather.lights.changed.add(key);
  }

  /**
   * The world matrix of the light at `path` placed by `instance`, as
   * `layerstack_stagehand` places it: its transform relative to its
   * prototype, the instance's (the prototype's own transform included,
   * masked instances left out), then the instancer's world transform;
   * `null` when the instance is masked or gone, or not of a prototype
   * holding the light.
   */
  #placedMatrix(path: string, instance: LightInstance, placements: Map<string, Placed | null>): Float64Array | null {
    const placed = this.#placed(instance.instancer, placements);
    const found = placed?.instances.get(instanceKey(instance));
    const prototype = found === undefined ? undefined : placed!.prototypes[found[0]];
    if (found === undefined || prototype === undefined || !under(path, prototype)) return null;
    const world = this.#world([instance.instancer]);
    const out = new Float64Array(16);
    if (path === prototype) {
      multiply(found[1], world, out, 0);
      return out;
    }
    const both = this.#world([path, prototype]);
    const inner = new Float64Array(16);
    multiply(both.subarray(0, 16), inverseAffine(both.subarray(16, 32)), inner, 0);
    const scratch = new Float64Array(16);
    multiply(inner, found[1], scratch, 0);
    multiply(scratch, world, out, 0);
    return out;
  }

  /**
   * The point instancer `instancer`'s prototypes and unmasked instances at
   * the list's time, once per diff in `placements`; `null` when it is not
   * one, or its arrays place nothing.
   */
  #placed(instancer: string, placements: Map<string, Placed | null>): Placed | null {
    let placed = placements.get(instancer);
    if (placed !== undefined) return placed;
    placed = null;
    try {
      const read = this.#stage.pointInstances(instancer, this.#time === undefined ? {} : { time: this.#time });
      const instances = new Map<string, [number, Float64Array, LightInstance]>();
      placedInstances(this.#stage, instancer, read, this.#time).forEach((placed, i) =>
        instances.set(instanceKey(placed), [read.prototypeIndices[i]!, read.matrices.subarray(16 * i, 16 * i + 16), placed]),
      );
      placed = { prototypes: read.prototypes, instances };
    } catch (error) {
      if (!String((error as { code?: string }).code).startsWith("instancer.") && (error as { code?: string }).code !== "prim.not_found") throw error;
    }
    placements.set(instancer, placed);
    return placed;
  }

  /** Adds to `into` each light of `lights` (inside `instancer`'s prototypes) once per unmasked instance of its prototype. */
  #placeLights(
    instancer: string,
    lights: readonly [number, string, LightKind][],
    placements: Map<string, Placed | null>,
    into: Map<string, FoundLight>,
  ): void {
    for (const [prototype, , instance] of this.#placed(instancer, placements)?.instances.values() ?? []) {
      for (const [index, path, kind] of lights) {
        if (index === prototype) into.set(lightKey(path, instance), { path, instance, kind });
      }
    }
  }

  /**
   * Finds a native instancing prototype's items from its first instance;
   * with `views`, its cameras and lights too, once per instance, by their
   * instance proxies' paths, as `layerstack_stagehand` lists them.
   */
  #discoverPrototype(first: string, found: Found, views: boolean): void {
    const stage = this.#stage;
    const roots = this.#instancesOf(first);
    const stack = stage.prim(first)?.children() ?? [];
    while (stack.length > 0) {
      const path = stack.pop()!;
      const prim = stage.prim(path);
      if (prim === null) continue;
      found.prims++;
      if (!prim.active || !prim.defined || prim.abstract) continue;
      if (this.#drawn(prim)) {
        const materials: (string | null)[] = [];
        for (const proxy of proxies(first, path, roots)) {
          const material = this.#material(proxy);
          if (!materials.includes(material)) materials.push(material);
        }
        for (const material of materials) found.sources.push({ kind: "prototype", prim: path, material });
      }
      const kind = this.#kind(prim);
      if (views && (kind.camera || kind.light !== null)) {
        for (const proxy of proxies(first, path, roots)) {
          if (kind.camera) found.cameras.push(proxy);
          else found.lights.set(proxy, { path: proxy, instance: null, kind: kind.light! });
        }
      }
      stack.push(...prim.children());
    }
  }

  /** Finds a point instancer's items, and the lights inside its prototypes once per unmasked instance. */
  #discoverInstancer(instancer: string, found: Found): void {
    const stage = this.#stage;
    const prototypes = stage.relationship(`${instancer}.prototypes`)?.targets() ?? [];
    const lights: [number, string, LightKind][] = [];
    prototypes.forEach((prototype, index) => {
      const stack = [prototype];
      while (stack.length > 0) {
        const path = stack.pop()!;
        const prim = stage.prim(path);
        if (prim === null) continue;
        found.prims++;
        if (!prim.active || !prim.defined) continue;
        if (this.#drawn(prim)) found.sources.push({ kind: "pointInstancer", instancer, index, prim: path });
        const light = this.#kind(prim).light;
        if (light !== null) lights.push([index, path, light]);
        stack.push(...prim.children());
      }
    });
    found.instancers.set(instancer, { lights, varies: this.#inputsMightVary([instancer], INSTANCE_SET_INPUTS) });
    if (lights.length > 0) this.#placeLights(instancer, lights, new Map(), found.lights);
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

  /** Whether any of `paths`' world transforms might differ at another time. */
  #worldMightVary(paths: readonly string[]): boolean {
    if (paths.length === 0) return false;
    const at = this.#time === undefined ? {} : { time: this.#time };
    return this.#stage.worldTransformsMightVary(paths, at).includes(true);
  }

  /** Whether any of the attributes `names` of any of `prims` might hold another value at another time. */
  #inputsMightVary(prims: readonly string[], names: readonly string[]): boolean {
    const paths = prims.flatMap((prim) => names.map((name) => `${prim}.${name}`));
    return paths.length > 0 && this.#stage.valueSourcesMightBeTimeVarying(paths).includes(true);
  }

  /** Whether any shading input or output of `prims` might hold another value at another time. */
  #shadingMightVary(prims: readonly string[]): boolean {
    const paths = prims.flatMap((prim) =>
      (this.#stage.prim(prim)?.attributeNames() ?? []).filter(shadingProperty).map((name) => `${prim}.${name}`),
    );
    return paths.length > 0 && this.#stage.valueSourcesMightBeTimeVarying(paths).includes(true);
  }

  /** Whether any of `paths`' computed visibility might differ at another time: its `visibility`, or an ancestor's, might. */
  #visibilityMightVary(paths: readonly string[]): boolean {
    const chain = new Set<string>();
    for (const path of paths) {
      for (let at: string | null = path; at !== null && at !== "/"; at = parent(at)) chain.add(at);
    }
    return this.#inputsMightVary([...chain], ["visibility"]);
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
    const varies: Varies = { ...(previous?.varies ?? { matrices: false, visibility: false, geometry: false }) };
    if (what.instances || what.transform || what.visibility || isNew) {
      let matrices: Float64Array = new Float64Array();
      let instances: string[] = [];
      let ids: BigInt64Array | null = null;
      varies.matrices = false;
      if (source.kind === "gprim") {
        if (stage.prim(source.path) === null) return false;
        depends = [source.path];
        varies.visibility = this.#visibilityMightVary([source.path]);
        if (this.#shown([source.path])[0]) {
          gather.work.matricesComputed++;
          matrices = this.#world([source.path]);
          instances = [source.path];
          varies.matrices = this.#worldMightVary([source.path]);
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
        varies.visibility = this.#visibilityMightVary(members);
        varies.matrices = this.#worldMightVary(instances);
        depends = [...everyProxy, ...roots];
      } else {
        const targets = stage.relationship(`${source.instancer}.prototypes`)?.targets() ?? [];
        const prototype = targets[source.index];
        if (prototype === undefined || !under(source.prim, prototype) || stage.prim(source.prim) === null) return false;
        depends = [source.instancer, prototype, source.prim];
        ids = new BigInt64Array();
        varies.visibility = this.#visibilityMightVary([source.instancer]);
        if (this.#shown([source.instancer])[0]) {
          // The prim's world chain covers its transform relative to the
          // prototype and the instancer's.
          varies.matrices =
            this.#inputsMightVary([source.instancer], INSTANCER_INPUTS) ||
            this.#worldMightVary([source.instancer]) ||
            (source.prim !== prototype && this.#worldMightVary([source.prim]));
          // Arrays that place nothing (none at this time, or of unequal
          // lengths) draw no instance, as `layerstack_stagehand` reads them.
          let placed: Pick<PointInstances, "prototypeIndices" | "matrices" | "ids"> = {
            prototypeIndices: new Int32Array(),
            matrices: new Float64Array(),
            ids: new BigInt64Array(),
          };
          try {
            placed = stage.pointInstances(source.instancer, this.#time === undefined ? {} : { time: this.#time });
          } catch (error) {
            if (!String((error as { code?: string }).code).startsWith("instancer.")) throw error;
          }
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
        if (!sameArray(ids, item.ids)) {
          const before = new Set(item.ids ?? []);
          const after = new Set(ids ?? []);
          for (const at of after) if (!before.has(at)) gather.instancesAdded.set(`${id}#${at}`, { item: id, id: at });
          for (const at of before) if (!after.has(at)) gather.instancesRemoved.set(`${id}#${at}`, { item: id, id: at });
        }
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
      varies.geometry = this.#geometryMightVary(item.prim, key);
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
    this.#entries.set(id, { item, depends, varies });
    return true;
  }

  /** The buffer `item` draws, read again; `null` when its prim draws nothing. */
  #geometry(item: DrawItem, gather: Gather): number | null {
    const stage = this.#stage;
    const at = this.#time === undefined ? {} : { time: this.#time };
    if (this.#kind(stage.prim(item.prim)).gprim === "mesh") {
      const known = this.#meshBuffers.get(item.prim);
      const uvSets = known === undefined ? this.#uvSetsFor(item, gather) : this.#buffers.get(known)!.uvSets;
      const input = readMesh(stage, item.prim, { ...at, uvSets });
      if (input === null) return null;
      const subsets = (input.subsets ?? []).map((subset) => subset.path ?? "");
      if (known !== undefined) {
        const retained = this.#buffers.get(known)!;
        const old = retained.input!;
        const mesh = retained.buffers.mesh;
        if (sameTopology(old, input)) {
          const samePoints = sameArray(old.points, input.points) && samePrimvar(old.normals, input.normals);
          const samePrimvars =
            samePrimvar(old.st, input.st) &&
            samePrimvar(old.displayColor, input.displayColor) &&
            (old.uvSets ?? []).length === (input.uvSets ?? []).length &&
            (old.uvSets ?? []).every((set, i) => set.name === input.uvSets![i]!.name && samePrimvar(set.primvar, input.uvSets![i]!.primvar));
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
        uvSets,
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
      uvSets: [],
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

/** The texture coordinate sets besides `st` a material's textures read (their primvar readers' `varname`). */
function uvSetsRead(material: MaterialItem): string[] {
  return material.textures.flatMap((texture) => {
    const name = texture.coordinates.primvar;
    return name === null || name === "st" ? [] : [name];
  });
}

/** The prim at `path` (under the instance root `first`) in each instance of `roots`. */
function proxies(first: string, path: string, roots: readonly string[]): string[] {
  const relative = path.slice(first.length);
  return roots.map((root) => `${root}${relative}`);
}
