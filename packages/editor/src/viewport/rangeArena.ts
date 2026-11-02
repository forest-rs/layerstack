// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Ranges of a fixed-size buffer handed out and taken back, apart from
// three.js so it can be tested in Node: the clustered draws (`clusters.ts`)
// keep each mesh's and each draw's records at a range of each of their
// storage buffers, so a mesh or draw that comes or goes writes its own
// ranges and nothing else.
//
// First fit over a free list kept in order and merged as ranges come back;
// a range freed at the end lowers the high-water mark (what a pass over the
// buffer must cover) instead. A request no free range fits fails rather
// than growing: growing a GPU buffer is making it again, which the caller
// does for every buffer at once (and compacts them as it does).

/** A free range: where it starts and how long it is. */
interface Free {
  start: number;
  length: number;
}

/** What an arena holds, for reports: its size, what is taken, the high-water mark, and the free space below it. */
export interface ArenaUse {
  readonly capacity: number;
  readonly used: number;
  readonly end: number;
  /** Free space below the high-water mark: what fragmentation costs. */
  readonly holes: number;
}

/** Ranges of `[0, capacity)`, handed out first fit (see the header). */
export class RangeArena {
  /** The size of the buffer the ranges are of. */
  readonly capacity: number;
  #end = 0;
  #used = 0;
  /** Free ranges below {@link end}, in order, none adjacent to another. */
  readonly #free: Free[] = [];

  constructor(capacity: number) {
    if (!(capacity >= 0) || !Number.isInteger(capacity)) throw new RangeError(`an arena's capacity is a whole number, not ${capacity}`);
    this.capacity = capacity;
  }

  /** One past the last range handed out: what a pass over the buffer must cover. */
  get end(): number {
    return this.#end;
  }

  /** What the ranges handed out hold. */
  get used(): number {
    return this.#used;
  }

  /** What the arena holds, for reports. */
  get use(): ArenaUse {
    return { capacity: this.capacity, used: this.#used, end: this.#end, holes: this.#end - this.#used };
  }

  /** Whether `length` would fit somewhere, now or once the arena is compacted. */
  fitsCompacted(length: number): boolean {
    return this.#used + length <= this.capacity;
  }

  /** The start of a free range of `length` (first fit), or -1 when none fits. A range of nothing starts at 0 and takes nothing. */
  allocate(length: number): number {
    if (length === 0) return 0;
    for (let i = 0; i < this.#free.length; i++) {
      const free = this.#free[i]!;
      if (free.length < length) continue;
      const start = free.start;
      free.start += length;
      free.length -= length;
      if (free.length === 0) this.#free.splice(i, 1);
      this.#used += length;
      return start;
    }
    if (this.#end + length > this.capacity) return -1;
    const start = this.#end;
    this.#end += length;
    this.#used += length;
    return start;
  }

  /** Takes back the range at `start` of `length`, which {@link allocate} handed out. */
  free(start: number, length: number): void {
    if (length === 0) return;
    if (start < 0 || start + length > this.#end) throw new RangeError(`[${start}, ${start + length}) is not a range of this arena`);
    this.#used -= length;
    // Where it goes in the list: after every free range before it.
    let at = 0;
    while (at < this.#free.length && this.#free[at]!.start < start) at++;
    const before = at > 0 ? this.#free[at - 1]! : null;
    const after = at < this.#free.length ? this.#free[at]! : null;
    if ((before !== null && before.start + before.length > start) || (after !== null && start + length > after.start)) throw new RangeError(`[${start}, ${start + length}) is free already`);
    let merged: Free;
    if (before !== null && before.start + before.length === start) {
      before.length += length;
      merged = before;
      at--;
    } else {
      merged = { start, length };
      this.#free.splice(at, 0, merged);
    }
    if (after !== null && merged.start + merged.length === after.start) {
      merged.length += after.length;
      this.#free.splice(at + 1, 1);
    }
    // A free range reaching the end is no range: the end comes down to it.
    if (merged.start + merged.length === this.#end) {
      this.#end = merged.start;
      this.#free.splice(at, 1);
    }
  }
}
