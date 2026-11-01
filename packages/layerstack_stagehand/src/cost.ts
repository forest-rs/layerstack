// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What drawing costs: a draw list's buffers and instances measured, or a
// stage's estimated from counts before anything is built. Both are given
// in the same units, so an estimate and the measurement that follows it
// compare field by field.
import type { Stage } from "@forest-rs/layerstack";
import type { DrawBuffers, ItemSource } from "./draw.js";
import type { CurveOptions } from "./types.js";

/**
 * Bytes a renderer uploads per instance: a 4x4 matrix of `f32`, as
 * three.js's `InstancedMesh` holds it.
 */
export const INSTANCE_GPU_BYTES = 64;

/**
 * What one buffer costs, whoever draws it.
 *
 * - `triangles`: the triangles of its mesh (a mesh's, a shape's, curves'
 *   swept tubes, the unit shape points are drawn as).
 * - `vertices`: its mesh's vertices, plus curves' samples and points'
 *   points.
 * - `cpuBytes`: the bytes of the JavaScript typed arrays it holds, every
 *   one counted once: a mesh's positions, normals, `st`, colors, texture
 *   coordinate sets, indices, triangle faces and vertex points; curves'
 *   samples and segments; points' arrays; a stand-in's lines.
 * - `gpuBytes`: the bytes a renderer uploads to draw it as the editor
 *   does: `f32` positions and normals (3 per vertex), `st` and texture
 *   coordinate sets (2), colors (3), `u32` indices; curves' samples (3
 *   per sample) and segment indices; points' positions, widths and
 *   colors; a stand-in's line points. An estimate of what the GPU holds:
 *   drivers pad and copy.
 */
export interface BufferCost {
  readonly triangles: number;
  readonly vertices: number;
  readonly cpuBytes: number;
  readonly gpuBytes: number;
}

/**
 * What one item costs: its buffer's cost (whole, though another item may
 * share the buffer: equal shapes, a mesh drawn by several native
 * instancing groups) and its instances'.
 */
export interface ItemCost {
  /** The item's id ({@link DrawItem.id}) when measured; `null` in an estimate, which builds no item. */
  readonly item: number | null;
  readonly source: ItemSource;
  /** The prim whose geometry is drawn ({@link DrawItem.prim}). */
  readonly prim: string;
  /** The buffer drawn ({@link DrawBuffers.key}) when measured; `null` in an estimate. */
  readonly buffer: number | null;
  /** The buffer's cost, once per item: not multiplied by its instances. */
  readonly geometry: BufferCost;
  /** Instances drawn: matrices, hidden ones not counted (a hidden item draws none, though its buffer is kept). */
  readonly instances: number;
  /** Triangles drawn: the buffer's times its instances. */
  readonly trianglesDrawn: number;
  /** Bytes of its instance matrices on the GPU: {@link INSTANCE_GPU_BYTES} each. */
  readonly instanceBytes: number;
}

/**
 * Totals over a set of items: each buffer counted once however many items
 * and instances draw it, each instance once.
 */
export interface CostTotals {
  /** Items. */
  readonly items: number;
  /** Distinct buffers. */
  readonly buffers: number;
  /** Instances drawn, over every item. */
  readonly instances: number;
  /** Triangles drawn: each item's buffer's times its instances. */
  readonly trianglesDrawn: number;
  /** Triangles held: each distinct buffer's once. */
  readonly triangles: number;
  /** Vertices held: each distinct buffer's once. */
  readonly vertices: number;
  /**
   * JavaScript bytes held: each distinct buffer's arrays once, and every
   * item's `f64` instance matrices (128 bytes each).
   */
  readonly cpuBytes: number;
  /** Bytes uploaded: each distinct buffer's once, and {@link instanceBytes}. */
  readonly gpuBytes: number;
  /** Of {@link gpuBytes}: the instance matrices, {@link INSTANCE_GPU_BYTES} per instance. */
  readonly instanceBytes: number;
}

/**
 * What a draw list's items cost, or would cost.
 *
 * - `measured` ({@link DrawList.cost}): the buffers as built.
 * - `estimated` ({@link DrawList.estimate}): read from counts, building
 *   nothing. A mesh's triangles are exact for faces without holes
 *   (`faceVertexIndices` less twice the faces: Σ(n − 2)); its vertices are
 *   its points, raised where a drawn primvar (normals, `st`,
 *   `displayColor`) is face-varying: to an indexed primvar's value count
 *   (typical, not a bound), or to the face-vertex count for an unindexed
 *   one (a ceiling: a build merges equal corners); its bytes follow from
 *   those as a build's would. A
 *   shape is tessellated (cheap and exact); curves and points are rough,
 *   from their point counts and the list's curve options (swept tubes
 *   counted as such). A point instancer's instances are its
 *   `protoIndices` naming the prototype, masks not applied.
 */
export interface DrawCost {
  readonly basis: "measured" | "estimated";
  /** The time read; `undefined` for the default time. */
  readonly time: number | undefined;
  readonly total: CostTotals;
  /** Each item's cost, most bytes uploaded first (its buffer's and its instances'), then by prim. */
  readonly items: readonly ItemCost[];
}

/** The cost of `buffers` as they are now (a refill keeps their sizes). */
export function bufferCost(buffers: DrawBuffers): BufferCost {
  const mesh = buffers.mesh;
  let vertices = mesh.positions.length / 3;
  let gpuBytes =
    mesh.positions.byteLength +
    mesh.normals.byteLength +
    (mesh.st?.byteLength ?? 0) +
    (mesh.colors?.byteLength ?? 0) +
    mesh.uvSets.reduce((sum, set) => sum + set.values.byteLength, 0) +
    mesh.indices.byteLength;
  const curves = buffers.curves;
  if (curves !== null) {
    vertices += curves.positions.length / 3;
    gpuBytes += curves.positions.byteLength + curves.lines.byteLength + (curves.colors?.byteLength ?? 0);
  }
  const points = buffers.points;
  if (points !== null) {
    vertices += points.positions.length / 3;
    gpuBytes += points.positions.byteLength + points.widths.byteLength + (points.colors?.byteLength ?? 0);
  }
  if (buffers.standin !== null) gpuBytes += buffers.standin.lines.byteLength;
  return { triangles: mesh.indices.length / 3, vertices, cpuBytes: arrayBytes([mesh, curves, points, buffers.standin]), gpuBytes };
}

/** The bytes of every typed array reachable from `roots` through plain objects and arrays, each counted once. */
function arrayBytes(roots: readonly unknown[]): number {
  const seen = new Set<object>();
  let bytes = 0;
  const visit = (value: unknown, depth: number) => {
    if (value === null || typeof value !== "object" || seen.has(value) || depth > 4) return;
    seen.add(value);
    if (ArrayBuffer.isView(value)) {
      bytes += value.byteLength;
      return;
    }
    for (const child of Object.values(value)) visit(child, depth + 1);
  };
  for (const root of roots) visit(root, 0);
  return bytes;
}

/**
 * A mesh's buffer estimated from its counts, as {@link DrawCost} describes:
 * nothing is triangulated and no point is copied out of the stage.
 */
export function estimateMesh(stage: Stage, prim: string, time: number | undefined, uvSets: number): BufferCost | null {
  const length = (name: string) => attributeLength(stage, `${prim}.${name}`, time);
  const faces = length("faceVertexCounts");
  const corners = length("faceVertexIndices");
  const points = length("points");
  if (faces === null || corners === null || points === null) return null;
  const triangles = Math.max(0, corners - 2 * faces);
  // A face-varying primvar splits points where its corners differ: an
  // indexed one into about as many vertices as it has values (each value
  // usually at one point), an unindexed one at most into every corner.
  const split = (primvar: { readonly interpolation: string; readonly indexed: boolean; readonly path: string } | null): number => {
    if (primvar?.interpolation !== "faceVarying") return points;
    return primvar.indexed ? Math.max(points, attributeLength(stage, primvar.path, time) ?? corners) : corners;
  };
  // A primvar is drawn when it holds a value: `displayColor` its own or inherited.
  const valued = <T extends { readonly path: string }>(primvar: T | null): T | null =>
    primvar !== null && (attributeLength(stage, primvar.path, time) ?? 0) > 0 ? primvar : null;
  const st = valued(stage.primvar(prim, "st"));
  const color = valued(stage.primvar(prim, "displayColor", { inherited: true }));
  // Authored normals: the `normals` primvar, else the `normals` attribute
  // with its `interpolation` metadata.
  const normalsPrimvar = valued(stage.primvar(prim, "normals"));
  const authored = stage.attribute(`${prim}.normals`)?.metadata("interpolation") ?? null;
  const vertices = Math.max(
    split(normalsPrimvar),
    authored?.kind === "scalar" && authored.value === "faceVarying" ? corners : points,
    split(st),
    split(color),
  );
  // As a build fills them: f32 positions and normals, `st` and texture
  // coordinate sets, colors where `displayColor` has a value; u32 indices; and
  // on the CPU a face per triangle and a point per vertex besides.
  const perVertex = 24 + (st === null ? 0 : 8) + 8 * uvSets + (color === null ? 0 : 12);
  const gpuBytes = vertices * perVertex + triangles * 12;
  return { triangles, vertices, cpuBytes: gpuBytes + triangles * 4 + vertices * 4, gpuBytes };
}

/** The length of the array the attribute at `path` holds at `time`, read without copying it out; `null` when it holds none. */
function attributeLength(stage: Stage, path: string, time: number | undefined): number | null {
  const value = stage.attribute(path)?.get(time) ?? null;
  if (value?.kind !== "array") return null;
  const count = value.array.length;
  value.array.release();
  return count;
}

/**
 * A point set's buffer, roughly: one vertex per point, its position, width
 * and color uploaded; no triangles (sprites).
 */
export function estimatePoints(stage: Stage, prim: string, time: number | undefined): BufferCost {
  const points = attributeLength(stage, `${prim}.points`, time) ?? 0;
  return { triangles: 0, vertices: points, cpuBytes: points * 40, gpuBytes: points * 28 };
}

/**
 * Curves' buffer, roughly, as `options` lay them out: about `samples`
 * samples per authored point of a cubic curve (a third as many for a
 * Bézier, one per point for a linear one), each a position and a line
 * index uploaded; swept, a ring of `sides` vertices per sample (two for a
 * ribbon, where normals are authored) and two triangles per ring vertex.
 */
export function estimateCurves(stage: Stage, prim: string, time: number | undefined, options: CurveOptions): BufferCost {
  const points = attributeLength(stage, `${prim}.points`, time) ?? 0;
  const token = (name: string) => {
    const value = stage.attribute(`${prim}.${name}`)?.get(time) ?? null;
    return value?.kind === "scalar" ? String(value.value) : null;
  };
  const samples = options.samples ?? 8;
  const type = token("type");
  const per = type === "linear" ? 1 : token("basis") === "bezier" ? samples / 3 : samples;
  const sampled = Math.ceil(points * per);
  let cost = { triangles: 0, vertices: sampled, cpuBytes: sampled * 28, gpuBytes: sampled * 20 };
  if (options.style === "swept") {
    const ribbon = stage.attribute(`${prim}.normals`) !== null && (attributeLength(stage, `${prim}.normals`, time) ?? 0) > 0;
    const ring = ribbon ? 2 : (options.sides ?? 8);
    const vertices = sampled * ring;
    const triangles = sampled * ring * 2;
    cost = {
      triangles,
      vertices: sampled + vertices,
      cpuBytes: cost.cpuBytes + vertices * 28 + triangles * 16,
      gpuBytes: cost.gpuBytes + vertices * 24 + triangles * 12,
    };
  }
  return cost;
}

/** Totals of `items`, each buffer once by `key` (its key, or what an estimate shares it by). */
export function totals(items: readonly ItemCost[], key: (item: ItemCost) => string, cpuMatrixBytes: number): CostTotals {
  const buffers = new Map<string, BufferCost>();
  let instances = 0;
  let trianglesDrawn = 0;
  for (const item of items) {
    buffers.set(key(item), item.geometry);
    instances += item.instances;
    trianglesDrawn += item.trianglesDrawn;
  }
  let triangles = 0;
  let vertices = 0;
  let cpuBytes = cpuMatrixBytes;
  let gpuBytes = 0;
  for (const cost of buffers.values()) {
    triangles += cost.triangles;
    vertices += cost.vertices;
    cpuBytes += cost.cpuBytes;
    gpuBytes += cost.gpuBytes;
  }
  const instanceBytes = instances * INSTANCE_GPU_BYTES;
  return { items: items.length, buffers: buffers.size, instances, trianglesDrawn, triangles, vertices, cpuBytes, gpuBytes: gpuBytes + instanceBytes, instanceBytes };
}

/** An item's cost from its buffer's and its instance count. */
export function itemCost(item: number | null, source: ItemSource, prim: string, buffer: number | null, geometry: BufferCost, instances: number): ItemCost {
  return { item, source, prim, buffer, geometry, instances, trianglesDrawn: geometry.triangles * instances, instanceBytes: instances * INSTANCE_GPU_BYTES };
}

/** Most bytes uploaded first, then by prim. */
export function byBytes(a: ItemCost, b: ItemCost): number {
  return b.geometry.gpuBytes + b.instanceBytes - (a.geometry.gpuBytes + a.instanceBytes) || (a.prim < b.prim ? -1 : a.prim > b.prim ? 1 : 0);
}
