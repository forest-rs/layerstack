// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where a free camera stands to frame a box, and the clipping range it
// keeps as it moves: the math the viewport and the inspector's previews
// share, without three.js, so it is tested in Node.
//
// Framing never produces a camera it cannot move from: a box that is not
// one (empty, inverted, NaN or infinite) frames nothing (`null`), a box of
// no size (a point, a camera's or an empty prim's) is framed at a fallback
// radius instead of a vanishing one, and the clipping range always reaches
// the scene, so orbiting or zooming out from something small still shows
// what is around it.

/** A stage's up axis (`UsdGeomGetStageUpAxis`): `Y` or `Z`. */
export type UpAxis = "Y" | "Z";

/** Three numbers: a point or a direction. */
export type Vec3 = readonly [number, number, number];

/** A box, `[minX, minY, minZ, maxX, maxY, maxZ]`. */
export type Box = readonly number[];

/** Where a camera frames a box from. */
export interface Framing {
  /** The box's center: what the camera looks at and orbits about. */
  readonly center: Vec3;
  /** The radius framed: the box's bounding sphere's, or the fallback for a box of no size. */
  readonly radius: number;
  /** From the camera to the center. */
  readonly distance: number;
  /** Where the camera stands. */
  readonly position: Vec3;
  readonly near: number;
  readonly far: number;
}

/** The most `far / near` a clipping range spans: past it, depth precision fails. */
export const MAX_DEPTH_RATIO = 1e6;

/** Whether `bounds` is a box: six finite numbers, each minimum at most its maximum. */
export function isBox(bounds: Box | null | undefined): boolean {
  if (bounds == null || bounds.length < 6) return false;
  for (let axis = 0; axis < 3; axis++) {
    const min = bounds[axis]!;
    const max = bounds[axis + 3]!;
    if (!Number.isFinite(min) || !Number.isFinite(max) || min > max) return false;
  }
  return true;
}

/** A box's center and its bounding sphere's radius; `null` when it is not a box ({@link isBox}). */
export function sphereOf(bounds: Box | null | undefined): { readonly center: Vec3; readonly radius: number } | null {
  if (!isBox(bounds)) return null;
  const [x0, y0, z0, x1, y1, z1] = bounds as [number, number, number, number, number, number];
  const radius = Math.hypot(x1 - x0, y1 - y0, z1 - z0) / 2;
  const center: Vec3 = [(x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2];
  return Number.isFinite(radius) && center.every(Number.isFinite) ? { center, radius } : null;
}

/**
 * The direction from what the free camera frames to the camera: in
 * front, to the right and above, about the stage's up axis (the same view
 * of a Y-up stage and of the Z-up stage it is rotated to). Unit length.
 */
export function viewDirection(up: UpAxis): Vec3 {
  return normalize(up === "Y" ? [0.55, 0.45, 0.7] : [0.55, -0.7, 0.45]);
}

/** The stage's up axis as a direction. */
export function upVector(up: UpAxis): Vec3 {
  return up === "Y" ? [0, 1, 0] : [0, 0, 1];
}

/**
 * The clipping range for a camera `distance` from what it orbits, with
 * the farthest point of the scene `reach` from it (0 for none): near a
 * hundredth of the distance, far past both the orbited point and the
 * whole scene, the ratio kept within {@link MAX_DEPTH_RATIO} (far is
 * brought in, never near pushed out past what is framed). `null` when
 * `distance` is not a positive finite number.
 */
export function clipRange(distance: number, reach = 0): { readonly near: number; readonly far: number } | null {
  if (!(distance > 0) || !Number.isFinite(distance)) return null;
  const near = distance / 100;
  const wanted = Math.max(distance * 10, Number.isFinite(reach) ? reach * 1.05 : 0);
  return { near, far: Math.min(wanted, near * MAX_DEPTH_RATIO) };
}

/** What {@link frameBox} frames with. */
export interface FrameOptions {
  /** The camera's vertical field of view, in degrees. */
  readonly fov: number;
  /** Width over height; the narrower of the two angles fits the box (1 when absent). */
  readonly aspect?: number;
  /** The direction from the box to the camera ({@link viewDirection}); normalized here. */
  readonly direction: Vec3;
  /** The radius a box of no size is framed at (1 when absent): a point is framed in its surroundings, not magnified to nothing. */
  readonly fallbackRadius?: number;
  /** The scene the camera stands in, so the far plane reaches all of it; `null` or absent for the box alone. */
  readonly scene?: Box | null;
}

/**
 * Where a camera frames `bounds` whole, with a margin: at its center,
 * `options.direction` from it, far enough that its bounding sphere fits
 * the narrower field of view. The same box frames the same way each time,
 * whatever the camera did before. `null` when `bounds` is not a box
 * ({@link isBox}) or the result would not be finite.
 */
export function frameBox(bounds: Box | null | undefined, options: FrameOptions): Framing | null {
  const sphere = sphereOf(bounds);
  if (sphere === null) return null;
  const { center } = sphere;
  // A box of no size, relative to where it is: framed at the fallback.
  const scale = Math.max(1, Math.abs(center[0]), Math.abs(center[1]), Math.abs(center[2]));
  const fallback = options.fallbackRadius !== undefined && options.fallbackRadius > 0 && Number.isFinite(options.fallbackRadius) ? options.fallbackRadius : 1;
  const radius = sphere.radius > scale * 1e-9 ? sphere.radius : fallback;
  const vertical = (Math.min(Math.max(options.fov, 1), 179) * Math.PI) / 360;
  const aspect = options.aspect !== undefined && options.aspect > 0 && Number.isFinite(options.aspect) ? options.aspect : 1;
  const half = Math.min(vertical, Math.atan(Math.tan(vertical) * aspect));
  const distance = (1.1 * radius) / Math.tan(half);
  const direction = normalize(options.direction);
  const position: Vec3 = [center[0] + direction[0] * distance, center[1] + direction[1] * distance, center[2] + direction[2] * distance];
  const range = clipRange(distance, Math.max(distance + radius, reachFrom(position, options.scene)));
  if (range === null || !position.every(Number.isFinite)) return null;
  return { center, radius, distance, position, near: range.near, far: range.far };
}

/** From `point` to the farthest corner of `scene`; 0 when it is not a box. */
export function reachFrom(point: Vec3, scene: Box | null | undefined): number {
  if (!isBox(scene)) return 0;
  const box = scene!;
  let farthest = 0;
  for (let corner = 0; corner < 8; corner++) {
    const x = box[corner & 1 ? 3 : 0]! - point[0];
    const y = box[corner & 2 ? 4 : 1]! - point[1];
    const z = box[corner & 4 ? 5 : 2]! - point[2];
    farthest = Math.max(farthest, Math.hypot(x, y, z));
  }
  return farthest;
}

/** `v` at unit length; `[0, 0, 1]` for a zero or non-finite one. */
export function normalize(v: Vec3): Vec3 {
  const length = Math.hypot(v[0], v[1], v[2]);
  return length > 0 && Number.isFinite(length) ? [v[0] / length, v[1] / length, v[2] / length] : [0, 0, 1];
}
