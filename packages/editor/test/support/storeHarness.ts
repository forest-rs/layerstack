// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the store's acceptance tests stand an `EditorStore` on, in Node: the
// sample scene read from `public/scene`, with fetches a test can hold and
// watch; a viewport that records what it was told instead of drawing; and
// frames and a clock the test moves by hand, so an open's phases and
// playback's ticks happen when the test says.
import { readFile, stat } from "node:fs/promises";
import { loadLayerstack } from "@forest-rs/layerstack";
import { loadStagehand } from "@forest-rs/layerstack_stagehand";
import type { ViewportScene, ViewportUpdate } from "../../src/model/draws.ts";
import type { SceneSource } from "../../src/model/sources.ts";
import { EditorStore } from "../../src/store.ts";
import type { Viewport } from "../../src/viewport/viewport3d.ts";

/** The sample scene's directory. */
const SCENE = new URL("../../public/scene/", import.meta.url);

/**
 * The page's frames and clock, moved by hand: `requestAnimationFrame`
 * callbacks wait until {@link Frames.run}, and `performance.now()` reads
 * {@link Frames.now}.
 */
export class Frames {
  /** The time `performance.now()` reads, in milliseconds. */
  now = 0;
  readonly #waiting = new Map<number, FrameRequestCallback>();
  #next = 1;

  /** Replaces the globals for the rest of the process (a test file is its own). */
  install(): void {
    globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const id = this.#next++;
      this.#waiting.set(id, callback);
      return id;
    }) as typeof requestAnimationFrame;
    globalThis.cancelAnimationFrame = ((id: number) => {
      this.#waiting.delete(id);
    }) as typeof cancelAnimationFrame;
    Object.defineProperty(performance, "now", { configurable: true, value: () => this.now });
  }

  /** The frames requested and not yet run. */
  get waiting(): number {
    return this.#waiting.size;
  }

  /** Moves the clock by `ms` and runs the frames requested so far (not those they request). */
  run(ms = 0): void {
    this.now += ms;
    const callbacks = [...this.#waiting.values()];
    this.#waiting.clear();
    for (const callback of callbacks) callback(this.now);
  }
}

/** A source whose fetches a test can watch and hold. */
export interface TestSource extends SceneSource {
  /** Every identifier asked for, in order. */
  readonly asked: string[];
  /** The abort signals the fetches were given, by identifier (the last one's); absent when none was passed. */
  readonly signals: Map<string, AbortSignal>;
  /** Holds every fetch of `identifier` from now on until the returned function is called. */
  hold(identifier: string): () => void;
  /** How many times `identifier` was asked for. */
  askedFor(identifier: string): number;
  /** Makes every fetch of `identifier` from now on throw `error`. */
  fail(identifier: string, error: Error): void;
}

/**
 * The sample scene from disk, rooted at `root`, with `extra` layers served
 * from memory ahead of the disk's (a root with payloads, which the sample
 * has none of).
 */
export function sampleSource(root = "scene.usda", extra: Readonly<Record<string, string>> = {}): TestSource {
  const held = new Map<string, Promise<void>>();
  const failing = new Map<string, Error>();
  const asked: string[] = [];
  const signals = new Map<string, AbortSignal>();
  const bytes = async (identifier: string): Promise<Uint8Array | null> => {
    const text = extra[identifier];
    if (text !== undefined) return new TextEncoder().encode(text);
    try {
      return new Uint8Array(await readFile(new URL(identifier, SCENE)));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw error;
    }
  };
  // The store's fetcher is given the signal once it threads one through.
  const fetch = async (identifier: string, signal?: AbortSignal): Promise<Uint8Array | null> => {
    asked.push(identifier);
    if (signal !== undefined) signals.set(identifier, signal);
    await held.get(identifier);
    const error = failing.get(identifier);
    if (error !== undefined) throw error;
    return bytes(identifier);
  };
  return {
    label: root,
    root,
    fetch,
    exists: async (identifier) => (await bytes(identifier)) !== null,
    size: async (identifier) => {
      const text = extra[identifier];
      if (text !== undefined) return text.length;
      return stat(new URL(identifier, SCENE)).then(
        (found) => found.size,
        () => null,
      );
    },
    // As the sample's host lists them (its layers.json), with the layers served from memory.
    layers: async () => [...(JSON.parse(await readFile(new URL("layers.json", SCENE), "utf8")) as string[]), ...Object.keys(extra)].sort(),
    asked,
    signals,
    hold(identifier) {
      let release = () => {};
      held.set(identifier, new Promise<void>((resolve) => (release = resolve)));
      return () => {
        held.delete(identifier);
        release();
      };
    },
    askedFor(identifier) {
      return asked.filter((each) => each === identifier).length;
    },
    fail(identifier, error) {
      failing.set(identifier, error);
    },
  };
}

/** What a {@link RecordingViewport} was told. */
export interface ViewportRecord {
  /** Each `update`: the items drawn, what the sync said changed, and the prim highlighted. */
  readonly updates: { readonly scene: ViewportScene; readonly update: ViewportUpdate; readonly selected: string | null }[];
  /** Each box framed. */
  readonly framed: (readonly number[])[];
  /** Each marker set: an instance's corners, or `null`. */
  readonly markers: (ArrayLike<number> | null)[];
  /** Each up axis set. */
  readonly upAxes: string[];
  /** Each camera looked through. */
  readonly through: (string | null)[];
}

/** A viewport that draws nothing and records what the store tells it. */
export interface RecordingViewport {
  readonly viewport: Viewport;
  readonly record: ViewportRecord;
}

/** A recording viewport, to hand to `EditorStore.attachViewport`. */
export function recordingViewport(): RecordingViewport {
  const record: ViewportRecord = { updates: [], framed: [], markers: [], upAxes: [], through: [] };
  const stub = {
    onUploaded: null,
    onResident: null,
    onReleased: null,
    onContextRestored: null,
    lookThrough(path: string | null) {
      record.through.push(path);
    },
    setLightSettings() {},
    setToneMap() {},
    setUpAxis(up: string) {
      record.upAxes.push(up);
    },
    setMarker(corners: ArrayLike<number> | null) {
      record.markers.push(corners);
    },
    update(scene: ViewportScene, update: ViewportUpdate, selected: string | null) {
      record.updates.push({ scene, update, selected });
    },
    frame(box: readonly number[]) {
      record.framed.push(box);
      return true;
    },
  };
  return { viewport: stub as unknown as Viewport, record };
}

/** A store over the modules, its viewport's record, and the frames it waits on. */
export interface Harness {
  readonly store: EditorStore;
  readonly frames: Frames;
  readonly view: ViewportRecord;
  /**
   * Runs frames and lets the event loop turn until `work` settles (an open
   * waits two frames a phase, and on its fetches), then returns its
   * result. Throws if it has not settled after {@link WAIT_MS}.
   */
  settle<T>(work: Promise<T>): Promise<T>;
  /**
   * Lets the event loop turn `turns` times, running the frames requested:
   * for showing that nothing more happens. Never for waiting until
   * something has: how many turns a file's read takes is the machine's
   * to say ({@link Harness.until}).
   */
  turn(turns?: number): Promise<void>;
  /**
   * Runs frames and lets the event loop turn until `reached()` (work a
   * test holds no promise of got where the test needs it: a held fetch
   * was asked for). Throws, naming `what`, if it has not after
   * {@link WAIT_MS}.
   */
  until(reached: () => boolean, what: string): Promise<void>;
  /** Opens `source` and waits for it; the store is then `ready`, or the test fails. */
  open(source: SceneSource): Promise<void>;
}

/**
 * How long a wait may take before the test fails, by the wall clock: a
 * bound on real time, not on turns of the event loop, because how many
 * turns pass while a file is read differs from one machine to the next.
 */
const WAIT_MS = 30_000;

/** One event-loop turn: every settled promise's reactions and a round of I/O callbacks. */
const immediate = () => new Promise<void>((resolve) => setImmediate(resolve));

/** A store with a recording viewport attached and hand-moved frames installed. */
export async function harness(): Promise<Harness> {
  const frames = new Frames();
  frames.install();
  const [layerstack, stagehand] = await Promise.all([loadLayerstack(), loadStagehand()]);
  const store = new EditorStore(layerstack, stagehand);
  const { viewport, record } = recordingViewport();
  store.attachViewport(viewport);
  const turn = async (turns = 4) => {
    for (let at = 0; at < turns; at++) {
      frames.run();
      await immediate();
    }
  };
  const until = async (reached: () => boolean, what: string): Promise<void> => {
    // The wall clock, which the frames do not move: a turn is as long as the machine makes it.
    const deadline = Date.now() + WAIT_MS;
    while (!reached()) {
      if (Date.now() > deadline) throw new Error(`${what} had not happened after ${WAIT_MS / 1000} s`);
      await turn(1);
    }
  };
  const settle = async <T>(work: Promise<T>): Promise<T> => {
    let settled = false;
    const mark = () => {
      settled = true;
    };
    work.then(mark, mark);
    await until(() => settled, "the work settling");
    return work;
  };
  return {
    store,
    frames,
    view: record,
    settle,
    turn,
    until,
    async open(source) {
      await settle(store.open(source));
      if (store.status.kind !== "ready") throw new Error(`the open ended ${JSON.stringify(store.status)}`);
    },
  };
}

/** A module's trap as the layerstack wrapper reports it: what `moduleTrap` recognises. */
export function trap(): Error {
  return Object.assign(new Error("unreachable executed"), { code: "module.trapped" });
}

/**
 * A root layer over the sample's assets with two payloads, for the tests
 * of loading and unloading: the robot and a beacon, each behind one.
 */
export const PAYLOAD_ROOT = `#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{
    def Xform "Robot" (
        prepend payload = @./assets/robot.usda@
    )
    {
    }

    def Xform "Beacon" (
        prepend payload = @./assets/beacon.usda@
    )
    {
        double3 xformOp:translate = (4, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
`;

/**
 * A root that references a layer that references the sample's robot: an
 * open that takes two rounds of the asset loop, for the tests of stopping
 * between them.
 */
export const NESTED_LAYERS: Readonly<Record<string, string>> = {
  "nested.usda": `#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World" (
    prepend references = @./middle.usda@
)
{
}
`,
  "middle.usda": `#usda 1.0
(
    defaultPrim = "Middle"
)

def Xform "Middle"
{
    def Xform "Robot" (
        prepend references = @./assets/robot.usda@
    )
    {
    }
}
`,
};
