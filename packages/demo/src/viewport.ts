// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the viewport draws, read from the stage alone, kept by a stagehand
// `DrawList`: instanced draws (a gprim once, a native instance prototype's
// gprim at every instance, a point instancer's prototype at every visible
// instance) over buffers stagehand built and keeps (a mesh's planned
// triangles, an implicit shape's tessellation), their world matrices,
// visibility and purpose, their colors (the `displayColor` primvar, per
// vertex where it varies, else the bound material's `diffuseColor`), their
// face subsets' draw ranges, the world bounds to frame, the stage's
// cameras and lights with renderer-ready values, and the bound materials as
// `UsdPreviewSurface` records (constants, primvars and texture lookups,
// which `viewport3d.ts` turns into three.js materials). After each
// action the model hands the list the editor's change records, and the
// list answers with diffs: what to add, remove, move, show or hide, refill
// or rebuild. Nothing here draws: `viewport3d.ts` applies the diffs to
// three.js, and `scripts/acceptance.ts` checks `viewportSummary` and the
// diffs' counts headless.
import type { Stage } from "@forest-rs/layerstack";
import type {
  CameraItem,
  DrawBuffers,
  DrawDiff,
  DrawItem,
  DrawList,
  DrawWork,
  LightItem,
  MaterialItem,
  MaterialValue,
  Stagehand,
} from "@forest-rs/layerstack_stagehand";
import type { Editor } from "./editor.ts";
import { releaseValue } from "./values.ts";

/** The purposes the viewport draws and frames, as usdview shows them by default. */
export const PURPOSES = ["default", "proxy"] as const;

/** Segments around and rings from pole to pole of curved implicit shapes. */
const TESSELLATION = { segments: 24, rings: 12 } as const;

/** One draw: a stagehand item, its buffers and its colors. */
export interface ViewItem {
  readonly item: DrawItem;
  readonly buffers: DrawBuffers;
  /** `mesh` for a `Mesh`; else the implicit shape's type (`Sphere`, `Cube`, ...). */
  readonly kind: string;
  /** Linear RGB. */
  readonly color: readonly [number, number, number];
  /** Where the color comes from. */
  readonly colorSource: "displayColor" | "material" | "fallback";
  /** Each range's color: its subset's bound material's `diffuseColor`, else `null` for the item's. */
  readonly rangeColors: readonly (readonly [number, number, number] | null)[];
}

/** The viewport's contents at one time. */
export interface ViewportScene {
  readonly items: readonly ViewItem[];
  /** The gprims and instancers left out because they are invisible, or of a purpose not drawn. */
  readonly hidden: readonly string[];
  /** The aligned world bounds of everything drawable, `[minX, minY, minZ, maxX, maxY, maxZ]`; `null` when empty. */
  readonly bounds: readonly number[] | null;
  /** The stage's lights, by path. */
  readonly lights: readonly LightItem[];
  /** The camera the viewport looks through: the stage's first, by path; `null` when it has none. */
  readonly camera: CameraItem | null;
  /** The materials the draws bind, by path. */
  readonly materials: ReadonlyMap<string, MaterialItem>;
}

/**
 * What one sync changed, merged over the change records it applied: the
 * renderer's to-do list. `reset` when the stage is a new one (opened,
 * reopened): drop everything and add `added`.
 */
export interface ViewportUpdate {
  readonly reset: boolean;
  /** The change records applied, and whether the time changed. */
  readonly records: number;
  readonly timeChanged: boolean;
  /** Item ids. */
  readonly added: ReadonlySet<number>;
  readonly removed: ReadonlySet<number>;
  readonly matrices: ReadonlySet<number>;
  readonly visibility: ReadonlySet<number>;
  /** Items whose materials or colors to set again: rebound materials, recolored primvars. */
  readonly recolored: ReadonlySet<number>;
  readonly rebound: ReadonlySet<number>;
  /** Buffer keys. */
  readonly buffersAdded: ReadonlySet<number>;
  readonly refilled: ReadonlySet<number>;
  readonly rebuilt: ReadonlySet<number>;
  readonly freed: ReadonlySet<number>;
  /** Light paths: added, removed, read again and found different. */
  readonly lightsAdded: ReadonlySet<string>;
  readonly lightsRemoved: ReadonlySet<string>;
  readonly lightsChanged: ReadonlySet<string>;
  /** Camera paths whose camera to set again: added, removed or changed. */
  readonly cameras: ReadonlySet<string>;
  /** Material paths: added, removed, read again and found different. */
  readonly materialsAdded: ReadonlySet<string>;
  readonly materialsRemoved: ReadonlySet<string>;
  readonly materialsChanged: ReadonlySet<string>;
  /** The work done, summed. */
  readonly work: DrawWork;
}

/** What the acceptance script checks of the viewport, independent of pixels. */
export interface ViewportSummary {
  /** The prims drawn, sorted; an instancer as `path ×count`. */
  readonly drawn: readonly string[];
  readonly hidden: readonly string[];
  readonly triangles: number;
  readonly instances: number;
  /** The framed world bounds, to 3 decimals. */
  readonly bounds: readonly number[] | null;
  /** Each drawn mesh's stagehand report, in brief, sorted: triangles, vertices, concave faces, triangles per range. */
  readonly meshes: readonly string[];
  /** Each light, `path kind`, with `hidden` when invisible, sorted. */
  readonly lights: readonly string[];
  /** The camera looked through. */
  readonly camera: string | null;
  /** Each material, `path: input source, ...` for its non-default inputs, its textures and alpha, sorted. */
  readonly materials: readonly string[];
}

/** The counts of an update, as the acceptance script checks them and the page shows them. */
export interface UpdateSummary {
  readonly added: number;
  readonly removed: number;
  readonly moved: number;
  readonly shown: number;
  readonly refilled: number;
  readonly rebuilt: number;
  readonly triangulations: number;
  readonly tessellations: number;
  /** Draws read, summed over the change records applied. */
  readonly itemsRead: number;
  /** Draws the last change record applied did not read. */
  readonly itemsUntouched: number;
  /** Draws a time change did not read because nothing they read is animated, summed. */
  readonly itemsTimeInvariant: number;
  readonly localTransforms: number;
  /** Lights read, summed. */
  readonly lightsRead: number;
  /** Lights added, removed or changed. */
  readonly lights: number;
  /** Cameras read, summed. */
  readonly camerasRead: number;
  /** Materials read, summed. */
  readonly materialsRead: number;
  /** Materials added, removed or changed. */
  readonly materials: number;
}

const EMPTY_WORK: DrawWork = {
  primsTraversed: 0,
  itemsRead: 0,
  itemsUntouched: 0,
  itemsTimeInvariant: 0,
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

/** The `diffuseColor` of the surface shader of the material at `material`, if it has one. */
function diffuse(stage: Stage, material: string | null, time?: number): [number, number, number] | null {
  const shader = material === null ? null : (stage.material(material)?.surface().shader ?? null);
  const value = shader === null ? null : (stage.shader(shader)?.input("diffuseColor")?.get(time) ?? null);
  if (value?.kind === "tuple" && value.value.length === 3) {
    const [r, g, b] = value.value;
    return [r!, g!, b!];
  }
  return null;
}

/** Each range's color: a subset's bound material's `diffuseColor`, `null` for the item's own. */
function rangeColors(stage: Stage, path: string, geometry: DrawBuffers, time?: number): (readonly [number, number, number] | null)[] {
  const ranges = geometry.mesh.ranges;
  if (geometry.subsets.length === 0) return ranges.map(() => null);
  const bound = new Map<string, string | null>();
  for (const subset of stage.prim(path)?.materialSubsets(time === undefined ? {} : { time }).subsets ?? []) {
    bound.set(subset.path, subset.binding?.material ?? null);
    subset.indices.release();
  }
  return ranges.map((range) =>
    range.subset === null ? null : diffuse(stage, bound.get(geometry.subsets[range.subset] ?? "") ?? null, time),
  );
}

/**
 * The prim's color: its `displayColor` primvar, its own or inherited (the
 * first value: the viewport draws one color per prim), else its bound
 * material's surface `diffuseColor`, else a neutral gray.
 */
export function primColor(stage: Stage, path: string, time?: number): Pick<ViewItem, "color" | "colorSource"> {
  const primvar = stage.primvar(path, "displayColor", { inherited: true });
  const value = primvar?.flattened(time) ?? null;
  if (value?.kind === "array" && value.array.length > 0) {
    const [r, g, b] = Array.from(value.array.copy() as ArrayLike<number>);
    releaseValue(value);
    return { color: [r!, g!, b!], colorSource: "displayColor" };
  }
  releaseValue(value);
  const binding = stage.prim(path)?.boundMaterial() ?? null;
  const color = diffuse(stage, binding?.material ?? null, time);
  return color === null ? { color: [0.6, 0.6, 0.6], colorSource: "fallback" } : { color, colorSource: "material" };
}

/** A diff's work added to `into`; `itemsUntouched` is the last diff's. */
function addWork(into: DrawWork, work: DrawWork): DrawWork {
  return {
    primsTraversed: into.primsTraversed + work.primsTraversed,
    itemsRead: into.itemsRead + work.itemsRead,
    itemsUntouched: work.itemsUntouched,
    itemsTimeInvariant: into.itemsTimeInvariant + work.itemsTimeInvariant,
    matricesComputed: into.matricesComputed + work.matricesComputed,
    localTransforms: into.localTransforms + work.localTransforms,
    buffersReused: into.buffersReused + work.buffersReused,
    refills: into.refills + work.refills,
    triangulations: into.triangulations + work.triangulations,
    tessellations: into.tessellations + work.tessellations,
    camerasRead: into.camerasRead + work.camerasRead,
    lightsRead: into.lightsRead + work.lightsRead,
    materialsRead: into.materialsRead + work.materialsRead,
  };
}

/** Whether `path` is `root` or beneath it. */
function under(path: string, root: string): boolean {
  return root === "/" || path === root || path.startsWith(`${root}/`);
}

/**
 * The viewport's model: a stagehand draw list of the editor's stage, kept
 * up to date from its change log, and the colors of its items.
 */
export class ViewportModel {
  readonly #stagehand: Stagehand;
  #list: DrawList | null = null;
  #stage: Stage | null = null;
  /** The editor's change log entries already applied. */
  #seen = 0;
  #colors = new Map<number, Pick<ViewItem, "color" | "colorSource" | "rangeColors">>();

  constructor(stagehand: Stagehand) {
    this.#stagehand = stagehand;
  }

  /** The draw list, once synced. */
  get list(): DrawList | null {
    return this.#list;
  }

  /**
   * Brings the list up to `editor`'s stage at `time`: a new stage is read
   * whole; otherwise the change records since the last sync are applied in
   * order, then the time, if it changed. Returns what changed.
   */
  sync(editor: Editor, time: number | undefined): ViewportUpdate {
    const stage = editor.stage;
    const diffs: DrawDiff[] = [];
    let reset = false;
    let timeChanged = false;
    let records = 0;
    const touched: string[] = [];
    if (this.#list === null || this.#stage !== stage) {
      this.#list?.dispose();
      this.#colors.clear();
      this.#list = this.#stagehand.drawList(stage, {
        ...(time === undefined ? {} : { time }),
        purposes: PURPOSES,
        tessellation: TESSELLATION,
      });
      this.#stage = stage;
      this.#seen = editor.changeLog.length;
      diffs.push(this.#list.refresh());
      reset = true;
    } else {
      for (const { changes } of editor.changeLog.slice(this.#seen)) {
        diffs.push(this.#list.applyChanges(changes));
        touched.push(...changes.resynced, ...changes.changedInfoOnly);
        touched.push(...changes.resyncedProperties.map((property) => property.slice(0, property.lastIndexOf("."))));
        records++;
      }
      this.#seen = editor.changeLog.length;
      if (time !== this.#list.time) {
        diffs.push(this.#list.setTime(time));
        timeChanged = true;
      }
    }
    const union = (pick: (diff: DrawDiff) => readonly number[]) => new Set(diffs.flatMap(pick));
    const paths = (pick: (diff: DrawDiff) => readonly string[]) => new Set(diffs.flatMap(pick));
    const update = {
      reset,
      records,
      timeChanged,
      added: union((d) => d.added),
      removed: union((d) => d.removed),
      matrices: union((d) => d.matrices),
      visibility: union((d) => d.visibility),
      recolored: new Set<number>(),
      rebound: union((d) => d.rebound),
      buffersAdded: union((d) => d.buffersAdded),
      refilled: union((d) => d.refilled),
      rebuilt: union((d) => d.rebuilt),
      freed: union((d) => d.freed),
      lightsAdded: paths((d) => d.lightsAdded),
      lightsRemoved: paths((d) => d.lightsRemoved),
      lightsChanged: paths((d) => d.lightsChanged),
      cameras: paths((d) => [...d.camerasAdded, ...d.camerasRemoved, ...d.camerasChanged]),
      materialsAdded: paths((d) => d.materialsAdded),
      materialsRemoved: paths((d) => d.materialsRemoved),
      materialsChanged: paths((d) => d.materialsChanged),
      work: diffs.reduce((work, diff) => addWork(work, diff.work), EMPTY_WORK),
    };
    // Colors: read again for new items, and for items whose materials,
    // buffers or prims a change reached (a `displayColor` edit refills a
    // mesh but leaves a shape's tessellation as it is).
    const list = this.#list;
    const recolor = new Set([...update.added, ...union((d) => d.materials), ...update.rebound]);
    for (const item of list.items()) {
      if (update.refilled.has(item.buffer) || update.rebuilt.has(item.buffer)) recolor.add(item.id);
      if (touched.some((path) => under(item.prim, path) || item.instances.some((instance) => under(instance, path)))) {
        recolor.add(item.id);
      }
    }
    // A changed material restyles every draw binding it, itself or by a subset.
    const restyled = new Set([...update.materialsChanged, ...update.materialsAdded]);
    for (const item of list.items()) {
      if ([item.material, ...item.subsetMaterials].some((material) => material !== null && restyled.has(material)) && !update.added.has(item.id)) {
        update.recolored.add(item.id);
      }
    }
    for (const id of update.removed) this.#colors.delete(id);
    for (const id of recolor) {
      const item = list.item(id);
      if (item === null) continue;
      const buffers = list.buffers(item.buffer)!;
      const at = item.instances[0] ?? item.prim;
      this.#colors.set(id, { ...primColor(stage, at, time), rangeColors: rangeColors(stage, at, buffers, time) });
      if (!update.added.has(id)) update.recolored.add(id);
    }
    return update;
  }

  /** The items with their buffers and colors, and the framed bounds. */
  scene(time: number | undefined): ViewportScene {
    const list = this.#list;
    const stage = this.#stage;
    if (list === null || stage === null) return { items: [], hidden: [], bounds: null, lights: [], camera: null, materials: new Map() };
    const items: ViewItem[] = [];
    const hidden = new Set<string>();
    for (const item of list.items()) {
      const buffers = list.buffers(item.buffer)!;
      if (item.hidden) {
        if (item.source.kind === "gprim") hidden.add(item.source.path);
        if (item.source.kind === "pointInstancer") hidden.add(item.source.instancer);
      }
      items.push({
        item,
        buffers,
        kind: buffers.shape?.kind ?? "mesh",
        ...this.#colors.get(item.id)!,
      });
    }
    const bounds = stage.worldBounds(["/"], { ...(time === undefined ? {} : { time }), purposes: [...PURPOSES] }).alignedRanges;
    const empty = bounds[0]! > bounds[3]!;
    const cameras = list.cameras().sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
    return {
      items,
      hidden: [...hidden],
      bounds: empty ? null : Array.from(bounds),
      lights: list.lights(),
      camera: cameras[0] ?? null,
      materials: new Map(list.materials().map((material) => [material.path, material])),
    };
  }

  /** Frees the list's buffers. */
  dispose(): void {
    this.#list?.dispose();
    this.#list = null;
    this.#stage = null;
  }
}

/** An update's counts. */
export function updateSummary(update: ViewportUpdate): UpdateSummary {
  return {
    added: update.added.size,
    removed: update.removed.size,
    moved: update.matrices.size,
    shown: update.visibility.size,
    refilled: update.refilled.size,
    rebuilt: update.rebuilt.size,
    triangulations: update.work.triangulations,
    tessellations: update.work.tessellations,
    itemsRead: update.work.itemsRead,
    itemsUntouched: update.work.itemsUntouched,
    itemsTimeInvariant: update.work.itemsTimeInvariant,
    localTransforms: update.work.localTransforms,
    lightsRead: update.work.lightsRead,
    lights: update.lightsAdded.size + update.lightsRemoved.size + update.lightsChanged.size,
    camerasRead: update.work.camerasRead,
    materialsRead: update.work.materialsRead,
    materials: update.materialsAdded.size + update.materialsRemoved.size + update.materialsChanged.size,
  };
}

/** The viewport's contents as the acceptance script checks them: no pixels. */
export function viewportSummary(scene: ViewportScene): ViewportSummary {
  let triangles = 0;
  let instances = 0;
  const drawn: string[] = [];
  const meshes: string[] = [];
  for (const { item, buffers } of scene.items) {
    if (item.hidden) continue;
    const placements = item.matrices.length / 16;
    triangles += (buffers.mesh.indices.length / 3) * placements;
    const report = buffers.mesh.report;
    if (report !== null) {
      const ranges = buffers.mesh.ranges.map((range) => range.count / 3).join("+");
      meshes.push(
        `${item.prim}: ${report.triangles} triangles, ${report.vertices} vertices, ` +
          `${report.concaveFaces} concave (${report.clippedFaces} clipped), ranges ${ranges}`,
      );
    }
    if (item.source.kind === "pointInstancer") {
      instances += placements;
      drawn.push(`${item.source.instancer} ×${placements}`);
    } else {
      drawn.push(...item.instances);
    }
  }
  return {
    drawn: drawn.sort(),
    hidden: [...scene.hidden].sort(),
    triangles,
    instances,
    bounds: scene.bounds === null ? null : scene.bounds.map((value) => Math.round(value * 1000) / 1000),
    meshes: meshes.sort(),
    lights: scene.lights.map((light) => `${light.path} ${light.kind}${light.hidden ? " hidden" : ""}`).sort(),
    camera: scene.camera?.path ?? null,
    materials: [...scene.materials.values()].map(materialLine).sort(),
  };
}

/** A number to 3 decimals, as the summaries write them. */
const round = (value: number) => Math.round(value * 1000) / 1000;

/** One input's source, as {@link materialLine} writes it: a value, `primvar name`, or `texture n.channel`. */
function sourceText(value: MaterialValue<number | readonly number[]>): string {
  switch (value.kind) {
    case "constant":
      return typeof value.value === "number" ? String(round(value.value)) : `(${value.value.map(round).join(", ")})`;
    case "primvar":
      return `primvar ${value.name}`;
    case "texture":
      return `texture ${value.texture}.${value.channel}`;
  }
}

/** A material record in one line: its surface's inputs, its textures and alpha, and its notes. */
export function materialLine(material: MaterialItem): string {
  const inputs = (["diffuseColor", "emissiveColor", "metallic", "roughness", "opacity", "normal", "occlusion"] as const).map(
    (name) => `${name} ${sourceText(material[name])}`,
  );
  const textures = material.textures.map((texture, index) => {
    const coordinates = texture.coordinates;
    const transform = coordinates.transform === null ? "" : ` rotated ${round(coordinates.transform.rotation)} scaled (${coordinates.transform.scale.map(round).join(", ")})`;
    return (
      `texture ${index} ${texture.file.identifier ?? texture.file.assetPath}${texture.file.udim ? " (udim)" : ""} ` +
      `${texture.colorSpace} ${texture.wrapS}/${texture.wrapT} scale (${texture.scale.map(round).join(", ")}) ` +
      `st ${coordinates.primvar ?? "constant"}${transform}`
    );
  });
  const alpha = material.alpha.kind === "mask" ? `mask ${round(material.alpha.threshold)}` : material.alpha.kind;
  const notes = material.notes.map((note) => `${note.kind}${note.input === null ? "" : ` ${note.input}`}`);
  return [`${material.path}: ${material.surface ?? "no surface"}`, ...inputs, ...textures, `alpha ${alpha}`, ...notes].join("; ");
}
