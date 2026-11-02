// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// GPU picking's bookkeeping, apart from three.js so it can be tested in
// Node: the ids a pick render writes, and the order its pixels are read in.
//
// A pick renders the scene once into a small RGBA8 target around the
// clicked point (`viewport3d.ts`), every draw in one shader that writes a
// 32-bit pick id per fragment: its object's first id plus the instance's
// index (`instanceIndex`), the low byte in red. Each drawn object is given
// a run of ids, one per instance it draws, numbered from 1 (0 is the
// cleared background), so ids are dense: 2^32 - 1 of them, whatever the
// split between objects and instances, where a fixed split (say 12 bits of
// instance) would cap one or the other. A pick id is found again by binary
// search over the runs' first ids.

/** The largest pick id: four bytes. */
export const MAX_PICK_ID = 0xffffffff;

/** An object a pick id names, and the instance within it. */
export interface PickHit {
  /** What the object was added under ({@link PickIds.add}): the viewport's item id. */
  readonly owner: number;
  /** The instance's index within the object (0 for an object drawn once). */
  readonly instance: number;
}

/**
 * The pick ids of one pick render: a run per object, in the order added.
 * Kept and cleared between picks, so picking allocates nothing per object.
 */
export class PickIds {
  readonly #firsts: number[] = [];
  readonly #owners: number[] = [];
  #next = 1;

  /** Forgets every run: ids start from 1 again. */
  clear(): void {
    this.#firsts.length = 0;
    this.#owners.length = 0;
    this.#next = 1;
  }

  /**
   * Gives `owner` a run of `count` ids (at least one) and returns its
   * first; `null` once ids run out (that object is not pickable in this
   * render).
   */
  add(owner: number, count: number): number | null {
    const first = this.#next;
    const length = Math.max(1, count);
    if (first + length - 1 > MAX_PICK_ID) return null;
    this.#firsts.push(first);
    this.#owners.push(owner);
    this.#next = first + length;
    return first;
  }

  /** The object and instance `id` names; `null` for 0 (nothing drawn) or an id no run holds. */
  find(id: number): PickHit | null {
    if (id < 1 || id >= this.#next) return null;
    let low = 0;
    let high = this.#firsts.length - 1;
    while (low < high) {
      const middle = (low + high + 1) >>> 1;
      if (this.#firsts[middle]! <= id) low = middle;
      else high = middle - 1;
    }
    return { owner: this.#owners[low]!, instance: id - this.#firsts[low]! };
  }
}

/** Writes `id` as four bytes, low byte first (red), as the pick shader does. */
export function packPickId(id: number, out: Uint8Array, at: number): void {
  out[at] = id & 0xff;
  out[at + 1] = (id >>> 8) & 0xff;
  out[at + 2] = (id >>> 16) & 0xff;
  out[at + 3] = (id >>> 24) & 0xff;
}

/** The pick id in the four bytes at `at` (red first). */
export function unpackPickId(pixels: Uint8Array, at: number): number {
  return (pixels[at]! | (pixels[at + 1]! << 8) | (pixels[at + 2]! << 16) | (pixels[at + 3]! << 24)) >>> 0;
}

/**
 * The pixels of a `size` × `size` pick target (odd), nearest the center
 * first, ties in row order: indices into its pixels. The center decides
 * for surfaces; the rest let a line or point a few pixels off be picked.
 */
export function pickOrder(size: number): Int32Array {
  const center = (size - 1) / 2;
  const pixels = Array.from({ length: size * size }, (_, index) => index);
  const distance = (index: number) => (Math.floor(index / size) - center) ** 2 + ((index % size) - center) ** 2;
  return Int32Array.from(pixels.sort((a, b) => distance(a) - distance(b) || a - b));
}

/**
 * Where pixel `pixel` of a `size` × `size` pick target ({@link pickOrder}
 * numbers rows from the bottom, as OpenGL reads them) is in the pixels
 * WebGPU reads back, whose rows run from the top.
 */
export function readIndex(pixel: number, size: number): number {
  return (size - 1 - Math.floor(pixel / size)) * size + (pixel % size);
}

/**
 * The 4×4 matrix (row by row) that narrows clip space to the `width` ×
 * `height` pixels about the point `x`, `y` (in pixels from the top left)
 * of a `viewWidth` × `viewHeight` view: multiplied before a projection
 * matrix, the window fills the pick target. As `PerspectiveCamera.setViewOffset`
 * does, for any projection (a stage camera's is given, not computed).
 */
export function pickWindow(x: number, y: number, width: number, height: number, viewWidth: number, viewHeight: number): number[] {
  const ndcX = (x / viewWidth) * 2 - 1;
  const ndcY = 1 - (y / viewHeight) * 2;
  const halfX = width / viewWidth;
  const halfY = height / viewHeight;
  return [1 / halfX, 0, 0, -ndcX / halfX, 0, 1 / halfY, 0, -ndcY / halfY, 0, 0, 1, 0, 0, 0, 0, 1];
}
