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
// which `../viewport/viewport3d.ts` turns into three.js materials), and the lights'
// evaluated links (which draws each light illuminates and shadows). After each
// action the model hands the list the session's change records, and the
// list answers with diffs: what to add, remove, move, show or hide, refill
// or rebuild. Nothing here draws: `../viewport/viewport3d.ts` applies the
// diffs to three.js.
import type { Stage } from "@forest-rs/layerstack";
import type {
  CameraItem,
  DrawBuffers,
  DrawDiff,
  DrawItem,
  DrawList,
  DrawWork,
  LightItem,
  LinkKind,
  MaterialItem,
  MaterialValue,
  Stagehand,
} from "@forest-rs/layerstack_stagehand";
import type { Session } from "./session.ts";
import { SkinningModel } from "./skinning.ts";
import type { SkinDraw, SkinSummary, SkinWork } from "./skinning.ts";
import { releaseValue } from "./values.ts";

/** The purposes the viewport draws and frames, as usdview shows them by default. */
export const PURPOSES = ["default", "proxy"] as const;

/** Segments around and rings from pole to pole of curved implicit shapes. */
const TESSELLATION = { segments: 24, rings: 12 } as const;

/**
 * How curves are sampled and swept: 8 samples per segment, tubes of 6
 * sides (ribbons where normals are authored). Every curve is swept; the
 * viewport draws the thin ones as lines instead ({@link curveDrawnAs}).
 */
const CURVES = { style: "swept", samples: 8, sides: 6 } as const;

/** The width under which a curve is drawn as line segments, not as its tube. */
export const LINE_WIDTH = 0.05;

/**
 * Whether the viewport draws a curves buffer as `lines` (every width under
 * {@link LINE_WIDTH}) or as its swept `tubes`.
 */
export function curveDrawnAs(buffers: DrawBuffers): "lines" | "tubes" {
  const widths = buffers.curves?.widths ?? new Float32Array();
  return widths.every((width) => width < LINE_WIDTH) ? "lines" : "tubes";
}

/** One draw: a stagehand item, its buffers and its colors. */
export interface ViewItem {
  readonly item: DrawItem;
  readonly buffers: DrawBuffers;
  /**
   * `mesh` for a `Mesh`; `lines` or `tubes` for curves ({@link curveDrawnAs});
   * `points` for a `Points`; else the implicit shape's type (`Sphere`, `Cube`, ...).
   */
  readonly kind: string;
  /** Linear RGB. */
  readonly color: readonly [number, number, number];
  /** Where the color comes from. */
  readonly colorSource: "displayColor" | "material" | "fallback";
  /** Each range's color: its subset's bound material's `diffuseColor`, else `null` for the item's. */
  readonly rangeColors: readonly (readonly [number, number, number] | null)[];
  /** How a skeleton deforms it at the time ({@link SkinningModel}); `null` when nothing does. */
  readonly skin: SkinDraw | null;
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
  /** The stage's cameras, by path. */
  readonly cameras: readonly CameraItem[];
  /** The materials the draws bind, by path. */
  readonly materials: ReadonlyMap<string, MaterialItem>;
  /** The skinned draws in brief. */
  readonly skinning: SkinSummary;
  /**
   * The lights each draw is not lit by, by item id: the lights whose
   * `lightLink` has a category that leaves the draw out. A native
   * prototype's draw is decided by its first instance (three.js gives one
   * material to all of an `InstancedMesh`'s instances).
   */
  readonly unlit: ReadonlyMap<number, ReadonlySet<string>>;
  /** Each light link that does not link everything, by light key and kind, sorted. */
  readonly linking: readonly LinkLine[];
}

/** A light link that does not link everything: the targets it links of all the draw list's targets. */
export interface LinkLine {
  readonly light: string;
  readonly kind: LinkKind;
  /** The linked targets' paths, sorted. */
  readonly linked: readonly string[];
  /** Every target's count: a draw per instance proxy, one for a point instancer. */
  readonly of: number;
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
  /** Light links whose linked draws changed, summed over the records. */
  readonly links: number;
  /** The work done, summed. */
  readonly work: DrawWork;
  /** Items whose skinning was bound again, begun or ended: their objects made again. */
  readonly reskinned: ReadonlySet<number>;
  /** Skinned items posed at the time: their joint palettes (or CPU-deformed points) to upload. */
  readonly skinned: ReadonlySet<number>;
  /** What skinning did. */
  readonly skinWork: SkinWork;
}

/** The viewport's contents in brief, independent of pixels. */
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
  /** The stage's cameras, sorted. */
  readonly cameras: readonly string[];
  /** Each material, `path: input source, ...` for its non-default inputs, its textures and alpha, sorted. */
  readonly materials: readonly string[];
  /** Each drawn curves prim's stagehand report, in brief, sorted: evaluation, segments, samples, how it is drawn. */
  readonly curves: readonly string[];
  /** Each drawn point set's report, in brief, sorted: points, shape, ids. */
  readonly points: readonly string[];
  /** Each light link that does not link everything: `<light> <kind> links <n> of <m> draws: <paths>`, sorted. */
  readonly links: readonly string[];
}

/** The counts of an update, as the stats show them. */
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
  /** Curves refilled by their kept plans, summed. */
  readonly curvesRefilled: number;
  /** Curves laid out, summed. */
  readonly curvesLaidOut: number;
  /** Point sets refilled, summed. */
  readonly pointsRefilled: number;
  /** Point sets read anew, summed. */
  readonly pointsRebuilt: number;
  /** Light links whose membership was computed, summed. */
  readonly linksEvaluated: number;
  /** Draws classified against a light-link category, summed. */
  readonly linksClassified: number;
  /** Light links whose linked draws changed. */
  readonly links: number;
}

const EMPTY_WORK: DrawWork = {
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
  curvesRefilled: 0,
  curvesLaidOut: 0,
  pointsRefilled: 0,
  pointsRebuilt: 0,
  linksEvaluated: 0,
  linksClassified: 0,
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
    instancersRederived: into.instancersRederived + work.instancersRederived,
    matricesComputed: into.matricesComputed + work.matricesComputed,
    localTransforms: into.localTransforms + work.localTransforms,
    buffersReused: into.buffersReused + work.buffersReused,
    refills: into.refills + work.refills,
    triangulations: into.triangulations + work.triangulations,
    tessellations: into.tessellations + work.tessellations,
    camerasRead: into.camerasRead + work.camerasRead,
    lightsRead: into.lightsRead + work.lightsRead,
    materialsRead: into.materialsRead + work.materialsRead,
    curvesRefilled: into.curvesRefilled + work.curvesRefilled,
    curvesLaidOut: into.curvesLaidOut + work.curvesLaidOut,
    pointsRefilled: into.pointsRefilled + work.pointsRefilled,
    pointsRebuilt: into.pointsRebuilt + work.pointsRebuilt,
    linksEvaluated: into.linksEvaluated + work.linksEvaluated,
    linksClassified: into.linksClassified + work.linksClassified,
  };
}

/** Whether `path` is `root` or beneath it. */
function under(path: string, root: string): boolean {
  return root === "/" || path === root || path.startsWith(`${root}/`);
}

/**
 * The viewport's model: a stagehand draw list of the session's stage, kept
 * up to date from its change log, and the colors of its items.
 */
export class ViewportModel {
  readonly #stagehand: Stagehand;
  #list: DrawList | null = null;
  #stage: Stage | null = null;
  /** The session's change log entries already applied. */
  #seen = 0;
  #colors = new Map<number, Pick<ViewItem, "color" | "colorSource" | "rangeColors">>();
  readonly #skins: SkinningModel;

  constructor(stagehand: Stagehand) {
    this.#stagehand = stagehand;
    this.#skins = new SkinningModel(stagehand);
  }

  /** The draw list, once synced. */
  get list(): DrawList | null {
    return this.#list;
  }

  /**
   * Brings the list up to `session`'s stage at `time`: a new stage is read
   * whole; otherwise the change records since the last sync are applied in
   * order, then the time, if it changed. Returns what changed.
   */
  sync(session: Session, time: number | undefined): ViewportUpdate {
    const stage = session.stage;
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
        curves: CURVES,
      });
      this.#stage = stage;
      this.#seen = session.changeLog.length;
      diffs.push(this.#list.refresh());
      reset = true;
    } else {
      for (const { changes } of session.changeLog.slice(this.#seen)) {
        diffs.push(this.#list.applyChanges(changes));
        // `/` changed info-only is the stage's metadata, which no color reads.
        touched.push(...changes.resynced, ...changes.changedInfoOnly.filter((path) => path !== "/"));
        touched.push(...changes.resyncedProperties.map((property) => property.slice(0, property.lastIndexOf("."))));
        records++;
      }
      this.#seen = session.changeLog.length;
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
      links: diffs.reduce((count, diff) => count + diff.linksChanged.length, 0),
      work: diffs.reduce((work, diff) => addWork(work, diff.work), EMPTY_WORK),
    };
    // Skinning: bound where items, buffers or the prims they skin with
    // changed; posed when the time did.
    const skins = this.#skins.sync(
      stage,
      this.#list,
      {
        reset,
        added: update.added,
        removed: update.removed,
        rebuffered: new Set([...update.buffersAdded, ...update.refilled, ...update.rebuilt]),
        touched,
        timeChanged,
      },
      time,
    );
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
    return { ...update, reskinned: skins.rebound, skinned: skins.posed, skinWork: skins.work };
  }

  /** The items with their buffers and colors, and the framed bounds. */
  scene(time: number | undefined): ViewportScene {
    const list = this.#list;
    const stage = this.#stage;
    if (list === null || stage === null) {
      return { items: [], hidden: [], bounds: null, lights: [], cameras: [], materials: new Map(), skinning: this.#skins.summary(), unlit: new Map(), linking: [] };
    }
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
        kind: buffers.curves !== null ? curveDrawnAs(buffers) : buffers.points !== null ? "points" : (buffers.shape?.kind ?? "mesh"),
        ...this.#colors.get(item.id)!,
        skin: this.#skins.draw(item.id),
      });
    }
    const bounds = stage.worldBounds(["/"], { ...(time === undefined ? {} : { time }), purposes: [...PURPOSES] }).alignedRanges;
    const empty = bounds[0]! > bounds[3]!;
    const cameras = list.cameras().sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
    // Light links: only those with a category leave draws out; the rest
    // (the `includeRoot` fallback) link everything.
    const lights = list.lights();
    const targets = list.items().flatMap((item) => item.linkPaths.map((path) => ({ item: item.id, path })));
    const unlit = new Map<number, Set<string>>();
    const linking: LinkLine[] = [];
    for (const light of [...lights].sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0))) {
      for (const [kind, link] of [
        ["light", light.lightLink],
        ["shadow", light.shadowLink],
      ] as const) {
        if (link.category === null) continue;
        const linked = targets.filter((target) => list.isLinked(light.key, kind, target));
        linking.push({ light: light.key, kind, linked: linked.map((target) => target.path).sort(), of: targets.length });
        if (kind !== "light") continue;
        for (const item of list.items()) {
          const first = item.linkPaths[0];
          if (first !== undefined && !list.isLinked(light.key, kind, { item: item.id, path: first })) {
            const set = unlit.get(item.id) ?? new Set<string>();
            set.add(light.key);
            unlit.set(item.id, set);
          }
        }
      }
    }
    return {
      items,
      hidden: [...hidden],
      bounds: empty ? null : Array.from(bounds),
      lights,
      cameras,
      materials: new Map(list.materials().map((material) => [material.path, material])),
      skinning: this.#skins.summary(),
      unlit,
      linking,
    };
  }

  /** Frees the list's buffers. */
  dispose(): void {
    this.#skins.dispose();
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
    curvesRefilled: update.work.curvesRefilled,
    curvesLaidOut: update.work.curvesLaidOut,
    pointsRefilled: update.work.pointsRefilled,
    pointsRebuilt: update.work.pointsRebuilt,
    linksEvaluated: update.work.linksEvaluated,
    linksClassified: update.work.linksClassified,
    links: update.links,
  };
}

/** The viewport's contents in brief, as the stats show them: no pixels. */
export function viewportSummary(scene: ViewportScene): ViewportSummary {
  let triangles = 0;
  let instances = 0;
  const drawn: string[] = [];
  const meshes: string[] = [];
  const curves: string[] = [];
  const points: string[] = [];
  for (const { item, buffers, kind } of scene.items) {
    if (item.hidden) continue;
    const placements = item.matrices.length / 16;
    // Curves drawn as lines draw no triangles; points are sprites.
    if (kind !== "lines") triangles += (buffers.mesh.indices.length / 3) * placements;
    if (buffers.curves !== null) {
      const report = buffers.curves.report;
      const evaluation =
        report.evaluation.kind === "basis"
          ? `${report.evaluation.type} ${report.evaluation.basis ?? ""} ${report.evaluation.wrap}`.replace("  ", " ")
          : report.evaluation.kind;
      const drawn = kind === "lines" ? `${report.lines} lines` : `${report.triangles} triangles (${report.geometry.kind})`;
      curves.push(`${item.prim}: ${evaluation}, segments ${report.segments.join("+")}, ${report.vertices} samples, ${drawn}`);
    }
    if (buffers.points !== null) {
      const report = buffers.points.report;
      points.push(`${item.prim}: ${report.points} points, ${report.shape}, ids ${report.channels.ids.state}`);
    }
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
    cameras: scene.cameras.map((camera) => camera.path),
    materials: [...scene.materials.values()].map(materialLine).sort(),
    curves: curves.sort(),
    points: points.sort(),
    links: scene.linking.map((line) => `${line.light} ${line.kind} links ${line.linked.length} of ${line.of} draws: ${line.linked.join(", ")}`),
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
