// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which of the viewport's draws three.js records into render bundles, and
// when a bundle is recorded again, apart from three.js so it can be tested
// in Node.
//
// A WebGPU render bundle is a recorded list of draw commands the GPU
// replays: three.js records a `BundleGroup`'s children once and, until the
// group's `needsUpdate` is set, replays the recording without looking at
// them again. Not their matrices, materials, geometry, visibility or
// instance counts: so every change to a bundled draw must record its
// bundle again, and that is this module's one rule (`touch`). Outside a
// bundle, three.js compares each draw's matrix, material, geometry and
// lights against what it last drew, every frame, which is most of a
// frame's CPU time once there are a thousand draws or more.
//
// A draw is drawn directly, outside any bundle, while it is new or has new
// materials (`drawDirectly`): its pipelines compile off the main thread
// (`Renderer.compileAsync`, which leaves bundles' contents alone), and a
// draw whose pipeline is not ready yet is skipped, not waited for. Once the
// compile that met it is done, it joins a bundle (`settle`) if the viewport
// finds it eligible. A draw that changes as time plays is kept direct for
// good (`keepDirect`): recording its bundle every frame would cost more
// than drawing it directly.
//
// Bundles hold at most `capacity` draws each, so one draw's change records
// that many again, not the whole scene. A bundle left empty stays, to be
// filled again.

/** What draws directly: the scene. */
export interface Holder<T> {
  add(object: T): unknown;
  remove(object: T): unknown;
}

/** A bundle: three.js's `BundleGroup`, recorded again once `needsUpdate` is set. */
export interface Bundle<T> extends Holder<T> {
  needsUpdate: boolean;
}

/** A bundle and how many draws it holds. */
interface Slot<T> {
  readonly bundle: Bundle<T>;
  size: number;
}

/** What {@link DrawBundles} holds: bundles, draws in them, and draws drawn directly. */
export interface BundleCounts {
  readonly bundles: number;
  readonly bundled: number;
  readonly direct: number;
  /** Draws drawn directly until their compile is done. */
  readonly pending: number;
  /** Draws kept direct because they change as time plays. */
  readonly dynamic: number;
}

/**
 * The viewport's draws, each in a bundle or drawn directly (see the
 * header). `direct` is where a direct draw goes (the scene); `make` makes
 * a new bundle and puts it where it is drawn (the scene too).
 */
export class DrawBundles<T> {
  readonly #direct: Holder<T>;
  readonly #make: () => Bundle<T>;
  readonly #capacity: number;
  readonly #slots: Slot<T>[] = [];
  /** Each draw's bundle, or `null` when it is drawn directly. */
  readonly #where = new Map<T, Slot<T> | null>();
  /** Direct draws waiting for their compile. */
  readonly #pending = new Set<T>();
  /** Direct draws that stay direct. */
  readonly #dynamic = new Set<T>();

  constructor(direct: Holder<T>, make: () => Bundle<T>, capacity: number) {
    if (!(capacity >= 1)) throw new RangeError(`a bundle holds at least one draw, not ${capacity}`);
    this.#direct = direct;
    this.#make = make;
    this.#capacity = capacity;
  }

  /**
   * Draws `object` directly until the next compile that meets it is done
   * ({@link compiling}, {@link settle}): a new draw, or one with new
   * materials. Out of its bundle if it was in one, which is recorded again.
   */
  drawDirectly(object: T): void {
    this.#unbundle(object);
    if (!this.#dynamic.has(object)) this.#pending.add(object);
  }

  /** Draws `object` directly for as long as it is drawn: it changes as time plays. */
  keepDirect(object: T): void {
    this.#unbundle(object);
    this.#pending.delete(object);
    this.#dynamic.add(object);
  }

  /** Whether `object` is kept direct ({@link keepDirect}). */
  isDynamic(object: T): boolean {
    return this.#dynamic.has(object);
  }

  /** Whether `object` is in a bundle. */
  isBundled(object: T): boolean {
    return (this.#where.get(object) ?? null) !== null;
  }

  /**
   * The direct draws a compile starting now meets, taken from those
   * waiting: hand them to {@link settle} once it is done.
   */
  compiling(): T[] {
    const met = [...this.#pending];
    this.#pending.clear();
    return met;
  }

  /**
   * Puts each of `met` (from {@link compiling}) that is still drawn
   * directly, not waiting for a later compile, not kept direct, and
   * `eligible`, into a bundle with room, which is recorded again.
   */
  settle(met: readonly T[], eligible: (object: T) => boolean): void {
    for (const object of met) {
      if (this.#where.get(object) !== null || this.#pending.has(object) || this.#dynamic.has(object) || !eligible(object)) continue;
      this.#direct.remove(object);
      const slot = this.#room();
      slot.bundle.add(object);
      slot.bundle.needsUpdate = true;
      slot.size += 1;
      this.#where.set(object, slot);
    }
  }

  /**
   * Records again the bundle holding `object`: its matrix, visibility,
   * instance count, geometry or lights changed. Nothing for a direct draw,
   * which three.js compares every frame.
   */
  touch(object: T): void {
    const slot = this.#where.get(object);
    if (slot != null) slot.bundle.needsUpdate = true;
  }

  /** Records every bundle again: the scene's lights changed, and with them every lit shader. */
  touchAll(): void {
    for (const slot of this.#slots) if (slot.size > 0) slot.bundle.needsUpdate = true;
  }

  /** Lets `object` go, from its bundle (recorded again) or from the direct draws. */
  remove(object: T): void {
    const slot = this.#where.get(object);
    if (slot === undefined) return;
    if (slot === null) this.#direct.remove(object);
    else this.#leave(object, slot);
    this.#where.delete(object);
    this.#pending.delete(object);
    this.#dynamic.delete(object);
  }

  /** How many draws are where. */
  get counts(): BundleCounts {
    let bundled = 0;
    for (const slot of this.#slots) bundled += slot.size;
    return { bundles: this.#slots.length, bundled, direct: this.#where.size - bundled, pending: this.#pending.size, dynamic: this.#dynamic.size };
  }

  /** `object` drawn directly: taken out of its bundle, or added if new. */
  #unbundle(object: T): void {
    const slot = this.#where.get(object);
    if (slot === null) return;
    if (slot !== undefined) this.#leave(object, slot);
    this.#direct.add(object);
    this.#where.set(object, null);
  }

  #leave(object: T, slot: Slot<T>): void {
    slot.bundle.remove(object);
    slot.bundle.needsUpdate = true;
    slot.size -= 1;
  }

  /** The first bundle with room, or a new one. */
  #room(): Slot<T> {
    for (const slot of this.#slots) if (slot.size < this.#capacity) return slot;
    const slot = { bundle: this.#make(), size: 0 };
    this.#slots.push(slot);
    return slot;
  }
}
