// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the store tells its listeners, by concern: a revision per channel,
// bumped when the state the channel covers changes, and listeners per
// channel. A panel listens to the channels it reads (`useRevision`,
// `useStoreSelector` in `../app/context.ts`), so playing the timeline,
// which bumps `time` sixty times a second, renders what shows the time and
// nothing else. No React here: a test listens the same way.

/**
 * The store's concerns, each with its own revision:
 *
 * - `status`: where the editor is with its scene (`status`, `opening`,
 *   `openingPhase`, `source`, `payloads`, `payloadNote`, and `running`:
 *   the action waiting on its assets).
 * - `composition`: the stage. Bumped when the session is another one and
 *   whenever the stage reports a change (an edit, an undo, a variant, a
 *   mute, a payload); `range`, `upAxis` and `renamed` follow it. A prim's
 *   own revision (`usePrimRevision`) says whether a change reached it.
 * - `selection`: `selection`, `instance`, `property`.
 * - `time`: `time` and `playing`.
 * - `draws`: `scene`, `cost`, `budget`, `focus` and `lastUpdate`, when a
 *   sync changed more than where things are: a time change that only
 *   moves draws bumps `time` alone.
 * - `view`: how the viewport shows the scene (`drawScope`, `budgetChoice`,
 *   `through`, `lightSettings`, `toneMap`, `frameNote`, `viewport`) and
 *   which tabs are in front (`sceneTab`, `bottomTab`).
 * - `history`: the session's undo and redo stacks, and which layers are
 *   dirty (every edit, undo and redo bumps it, and so does a save).
 * - `target`: where edits go (`session.target`).
 * - `notices`: `notices`.
 * - `loop`: the asset loop's logs and counts (`session.loops`,
 *   `session.loopLog`, `session.fetched`), of the session open or opening.
 */
export const CHANNELS = ["status", "composition", "selection", "time", "draws", "view", "history", "target", "notices", "loop"] as const;

/** One of the store's concerns ({@link CHANNELS}). */
export type Channel = (typeof CHANNELS)[number];

/** A revision and listeners per {@link Channel}. */
export class Signals {
  readonly #revisions = new Map<Channel, number>(CHANNELS.map((channel) => [channel, 0]));
  readonly #listeners = new Map<Channel, Set<() => void>>(CHANNELS.map((channel) => [channel, new Set()]));

  /** The revision of `channel`: how many times it was bumped. */
  revision(channel: Channel): number {
    return this.#revisions.get(channel)!;
  }

  /** The sum of the revisions of `channels`: it changes when any of them is bumped. */
  sum(channels: readonly Channel[]): number {
    let sum = 0;
    for (const channel of channels) sum += this.#revisions.get(channel)!;
    return sum;
  }

  /** Every channel's revision, by name: what a test compares before and after. */
  snapshot(): Readonly<Record<Channel, number>> {
    return Object.fromEntries(this.#revisions) as Record<Channel, number>;
  }

  /**
   * Bumps each of `channels` once and tells their listeners, each once
   * however many of its channels were bumped.
   */
  bump(...channels: readonly Channel[]): void {
    const told = new Set<() => void>();
    for (const channel of new Set(channels)) {
      this.#revisions.set(channel, this.#revisions.get(channel)! + 1);
      for (const listener of this.#listeners.get(channel)!) told.add(listener);
    }
    for (const listener of told) listener();
  }

  /** Bumps every channel: the scene is another one. */
  bumpAll(): void {
    this.bump(...CHANNELS);
  }

  /** Calls `listener` after each bump of any of `channels`, until the returned function is called. */
  subscribe(channels: readonly Channel[], listener: () => void): () => void {
    for (const channel of channels) this.#listeners.get(channel)!.add(listener);
    return () => {
      for (const channel of channels) this.#listeners.get(channel)!.delete(listener);
    };
  }

  /** How many listeners `channel` has. */
  listeners(channel: Channel): number {
    return this.#listeners.get(channel)!.size;
  }
}
