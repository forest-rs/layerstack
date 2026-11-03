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

/** The stage's up and the free camera's view direction, in some space. */
export interface ViewOrientation {
  /** Up: the camera's up and what it orbits about. Unit length. */
  readonly up: Vec3;
  /** From what is framed toward the camera ({@link viewDirection}). Unit length. */
  readonly direction: Vec3;
}

/**
 * The viewport's orientation for a stage with `up` as its up axis
 * ({@link upVector}, {@link viewDirection}) taken into a prim's own space,
 * where `world` (16 numbers, row by row, row vectors: `Stage.worldTransforms`)
 * takes it to world space: what a preview of the prim's untransformed
 * geometry orbits about and looks from, so it stands as it does in the
 * scene. A direction `d` in world space is `d * L⁻¹` in the prim's, `L`
 * the matrix's upper 3×3. A singular or non-finite matrix leaves the
 * world's orientation.
 */
export function localView(up: UpAxis, world: ArrayLike<number> | null): ViewOrientation {
  const inWorld: ViewOrientation = { up: upVector(up), direction: viewDirection(up) };
  if (world === null || world.length < 16) return inWorld;
  // The upper 3×3, row by row, and its inverse by cofactors.
  const [a, b, c, d, e, f, g, h, i] = [world[0]!, world[1]!, world[2]!, world[4]!, world[5]!, world[6]!, world[8]!, world[9]!, world[10]!];
  const determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
  if (!Number.isFinite(determinant) || Math.abs(determinant) < 1e-12) return inWorld;
  const inverse = [
    (e * i - f * h) / determinant,
    (c * h - b * i) / determinant,
    (b * f - c * e) / determinant,
    (f * g - d * i) / determinant,
    (a * i - c * g) / determinant,
    (c * d - a * f) / determinant,
    (d * h - e * g) / determinant,
    (b * g - a * h) / determinant,
    (a * e - b * d) / determinant,
  ];
  // A row vector times the inverse.
  const local = (v: Vec3): Vec3 =>
    normalize([
      v[0] * inverse[0]! + v[1] * inverse[3]! + v[2] * inverse[6]!,
      v[0] * inverse[1]! + v[1] * inverse[4]! + v[2] * inverse[7]!,
      v[0] * inverse[2]! + v[1] * inverse[5]! + v[2] * inverse[8]!,
    ]);
  return { up: local(inWorld.up), direction: local(inWorld.direction) };
}

/** Where the free camera takes over from a stage camera ({@link orbitFrom}). */
export interface Takeover {
  /** What the free camera orbits about: along the stage camera's view, {@link Takeover.distance} ahead. */
  readonly target: Vec3;
  readonly distance: number;
  readonly near: number;
  readonly far: number;
}

/**
 * What the free camera orbits about when it takes over from a stage
 * camera at `position` looking along `forward`: a point on its view axis,
 * at its `focusDistance` when one is authored (> 0), else as far along as
 * the scene's center is (its projection on the axis), else, with the
 * center behind it, half the scene's radius (1 with no scene). The
 * clipping range is {@link clipRange}'s from there. `null` when the
 * position or direction is not finite.
 */
export function orbitFrom(position: Vec3, forward: Vec3, focusDistance: number, scene: Box | null | undefined): Takeover | null {
  if (!position.every(Number.isFinite) || !forward.every(Number.isFinite) || Math.hypot(...forward) === 0) return null;
  const axis = normalize(forward);
  const sphere = sphereOf(scene);
  let distance = 0;
  if (focusDistance > 0 && Number.isFinite(focusDistance)) distance = focusDistance;
  else if (sphere !== null) {
    const ahead = (sphere.center[0] - position[0]) * axis[0] + (sphere.center[1] - position[1]) * axis[1] + (sphere.center[2] - position[2]) * axis[2];
    distance = ahead > sphere.radius * 1e-3 ? ahead : sphere.radius / 2;
  }
  if (!(distance > 0)) distance = 1;
  const range = clipRange(distance, reachFrom(position, scene));
  if (range === null) return null;
  const target: Vec3 = [position[0] + axis[0] * distance, position[1] + axis[1] * distance, position[2] + axis[2] * distance];
  return { target, distance, near: range.near, far: range.far };
}

/**
 * A perspective camera's vertical field of view in degrees, from its
 * window at depth 1 (`[left, bottom, right, top]`, `GfFrustum`'s, as
 * `Camera.window` gives it): the angle between its bottom and top edges.
 * `null` for a window without height.
 */
export function verticalFov(window: ArrayLike<number>): number | null {
  const bottom = window[1]!;
  const top = window[3]!;
  const degrees = ((Math.atan(top) - Math.atan(bottom)) * 180) / Math.PI;
  return degrees > 0 && degrees < 180 ? degrees : null;
}
