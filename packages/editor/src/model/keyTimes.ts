// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where the keys are: the times the selected prim's attributes have time
// samples, drawn as ticks on the timeline, jumped between, and said in
// words where a property is explained. The reads of the stage are done
// once (`selectionKeys`), by whoever holds the result for as long as the
// selection and the composition stay as they are; the rest is arithmetic
// on the times, in Node-testable functions with no DOM and no React.
import type { BracketingSamples, Stage } from "@forest-rs/layerstack";

/** The key times of a selection, ascending, within the timeline's range. */
export interface KeyTimes {
  /** The union of the selected prim's attributes' sample times (`unionedTimeSamples`). */
  readonly prim: readonly number[];
  /** The explained property's own sample times; empty when none is explained. */
  readonly property: readonly number[];
  /** Both, merged and without repeats: what the jump buttons walk. */
  readonly all: readonly number[];
}

/** No keys: nothing is selected. */
export const NO_KEYS: KeyTimes = { prim: [], property: [], all: [] };

/** `a` and `b` (both ascending) merged, ascending, each time once. */
export function mergeTimes(a: readonly number[], b: readonly number[]): number[] {
  const out: number[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length || j < b.length) {
    const next = j >= b.length || (i < a.length && a[i]! <= b[j]!) ? a[i++]! : b[j++]!;
    if (out.length === 0 || out[out.length - 1] !== next) out.push(next);
  }
  return out;
}

/**
 * The keys of the prim at `prim` (the union of its attributes' time
 * samples) and of the attribute at `property`, within `range`: one read of
 * the stage each, so a caller holds the result instead of asking again.
 * A path the stage does not know has no keys.
 */
export function selectionKeys(stage: Stage, prim: string | null, property: string | null, range: { readonly start: number; readonly end: number }): KeyTimes {
  const interval = { min: range.start, max: range.end };
  let primTimes: number[] = [];
  if (prim !== null) {
    const names = stage.prim(prim)?.attributeNames() ?? [];
    if (names.length > 0) primTimes = stage.unionedTimeSamples(names.map((name) => `${prim}.${name}`), interval);
  }
  const propertyTimes = property === null ? [] : (stage.attribute(property)?.timeSamplesInInterval(interval) ?? []);
  return { prim: primTimes, property: propertyTimes, all: mergeTimes(primTimes, propertyTimes) };
}

/** How near a time must be to a key to be on it: below any frame a person can tell apart. */
const ON_KEY = 1e-6;

/**
 * The key after `time` (`direction` 1) or before it (-1) in `times`
 * (ascending); `null` when there is none. A time on a key moves to the
 * next one past it, so jumping always moves.
 */
export function nextKey(times: readonly number[], time: number, direction: 1 | -1): number | null {
  if (direction === 1) {
    for (const key of times) if (key > time + ON_KEY) return key;
    return null;
  }
  for (let i = times.length - 1; i >= 0; i -= 1) if (times[i]! < time - ON_KEY) return times[i]!;
  return null;
}

/** A tick as drawn: the key's time, and the whole pixel it sits at along the track. */
export interface Tick {
  readonly time: number;
  readonly x: number;
}

/**
 * The ticks for `times` (ascending) along a track `width` pixels wide
 * covering `start` to `end`: each at its pixel, at most one per pixel (the
 * first key there is drawn, and clicking it moves to that key), the first
 * and the last key always drawn. Times outside the range are not drawn.
 */
export function thinTicks(times: readonly number[], start: number, end: number, width: number): Tick[] {
  const span = end - start;
  const inside = times.filter((time) => time >= start && time <= end);
  if (inside.length === 0 || !(width >= 1)) return [];
  const pixels = Math.floor(width);
  const at = (time: number) => (span > 0 ? Math.min(pixels - 1, Math.round(((time - start) / span) * (pixels - 1))) : 0);
  const last = inside.length - 1;
  const out: Tick[] = [];
  inside.forEach((time, index) => {
    const x = at(time);
    if (out.length > 0 && out[out.length - 1]!.x === x) {
      // The last key replaces what shares its pixel, unless that is the first key.
      if (index === last && out.length > 1) out[out.length - 1] = { time, x };
      return;
    }
    out.push({ time, x });
  });
  return out;
}

/** The types a time sample is never interpolated between: USD holds the sample below the time. */
const HELD_TYPES = /^(bool|u?int(64)?|uchar|token|string|asset|opaque|pathExpression)(\[\])?$/;

/** Whether USD holds a value of `typeName` between samples whatever the stage's interpolation (`interpolationType`). */
export function heldBetweenSamples(typeName: string): boolean {
  return HELD_TYPES.test(typeName);
}

/**
 * What the bracketing samples say of the time: it is on a sample, before
 * the first or after the last (the value is that sample's), or between two.
 */
export type BracketNote =
  | { readonly kind: "on"; readonly time: number }
  | { readonly kind: "before"; readonly time: number }
  | { readonly kind: "after"; readonly time: number }
  | { readonly kind: "between"; readonly lower: number; readonly upper: number; readonly held: boolean }
  | { readonly kind: "none" };

/**
 * The note for `bracket` at `time`. The stage does not report whether it
 * interpolates linearly or holds, so `held` is true only where the
 * attribute's type is held whatever it does ({@link heldBetweenSamples});
 * otherwise the note says only the two times.
 */
export function bracketNote(bracket: BracketingSamples, time: number, typeName: string): BracketNote {
  if (bracket.kind !== "samples") return { kind: "none" };
  const { lower, upper } = bracket;
  if (lower === upper) {
    if (Math.abs(lower - time) <= ON_KEY) return { kind: "on", time: lower };
    return time < lower ? { kind: "before", time: lower } : { kind: "after", time: lower };
  }
  return { kind: "between", lower, upper, held: heldBetweenSamples(typeName) };
}
