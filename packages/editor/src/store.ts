// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's state outside React: the open session, what is selected,
// where edits go, and the draw list the viewport is kept from. Every
// action runs here, then brings the draw list up to date from the stage's
// change records and bumps `revision`; components subscribe to it
// (`useEditor`) and read the stage directly, which is cheap and always
// current. Nothing here knows about the DOM except the viewport it is
// handed.
import { useSyncExternalStore } from "react";
import type { Edit, EditReceipt, EditTarget, Layer, Layerstack, ValueInput } from "@forest-rs/layerstack";
import type { Stagehand } from "@forest-rs/layerstack_stagehand";
import { ViewportModel, type ViewportScene, type ViewportUpdate } from "./model/draws.ts";
import { Session } from "./model/session.ts";
import type { SceneSource } from "./model/sources.ts";
import type { Viewport } from "./viewport/viewport3d.ts";

/** Where the editor is with its scene. */
export type EditorStatus =
  | { readonly kind: "empty" }
  | { readonly kind: "opening"; readonly label: string }
  | { readonly kind: "ready" }
  | { readonly kind: "failed"; readonly label: string; readonly message: string };

/** The stage's frames, as its metadata gives them. */
export interface TimeRange {
  /** `startTimeCode` and `endTimeCode`. */
  readonly start: number;
  readonly end: number;
  /** `timeCodesPerSecond`: how fast time codes pass when playing. */
  readonly timeCodesPerSecond: number;
}

/** Something an action reported that the person should see: a refused edit, a failed fetch. */
export interface Notice {
  readonly id: number;
  readonly action: string;
  readonly message: string;
  /** The error's code, for a `LayerstackError` (`edit.invalid`, `value.kind`). */
  readonly code: string | null;
}

const EMPTY_SCENE: ViewportScene = {
  items: [],
  hidden: [],
  bounds: null,
  lights: [],
  cameras: [],
  materials: new Map(),
  unlit: new Map(),
  linking: [],
};

/** The editor: one session at a time, and the state the panels share. */
export class EditorStore {
  readonly layerstack: Layerstack;
  readonly stagehand: Stagehand;
  /** Bumped after every change the panels may show. */
  revision = 0;
  status: EditorStatus = { kind: "empty" };
  session: Session | null = null;
  source: SceneSource | null = null;
  /** The selected prim's path. */
  selection: string | null = null;
  /** The property whose value the explain panel shows, by path. */
  property: string | null = null;
  /**
   * The time everything is read at: a time code, or `undefined` for the
   * default time (a stage without a time range).
   */
  time: number | undefined = undefined;
  /** The stage's time range; `null` when it authors none. */
  range: TimeRange | null = null;
  /** Whether time is playing. */
  playing = false;
  /** The stage camera the viewport looks through, by path; `null` for the free camera. */
  through: string | null = null;
  /** The session being opened, while its assets are fetched. */
  opening: Session | null = null;
  /** The draws, as last synced. */
  scene: ViewportScene = EMPTY_SCENE;
  /** The last sync that changed something, for the stats. */
  lastUpdate: ViewportUpdate | null = null;
  /** What actions reported, newest last. */
  readonly notices: Notice[] = [];
  readonly #draws: ViewportModel;
  readonly #listeners = new Set<() => void>();
  #viewport: Viewport | null = null;
  #notice = 0;
  #opening = 0;
  #frame = 0;
  #playFrame = 0;

  constructor(layerstack: Layerstack, stagehand: Stagehand) {
    this.layerstack = layerstack;
    this.stagehand = stagehand;
    this.#draws = new ViewportModel(stagehand);
  }

  /** The three.js view, once attached. */
  get viewport(): Viewport | null {
    return this.#viewport;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  /** Hands the store the three.js view (or `null` when it goes), which it keeps up to date. */
  attachViewport(viewport: Viewport | null): void {
    this.#viewport = viewport;
    if (viewport === null) return;
    viewport.lookThrough(this.through);
    this.#orient();
    const update = this.session === null ? null : this.#draws.sync(this.session, this.time);
    if (update !== null) {
      this.scene = this.#draws.scene(this.time);
      viewport.update(this.scene, { ...update, reset: true }, this.selection);
    }
  }

  /** The source's root, opened and resolved: everything it depends on is fetched first. */
  async open(source: SceneSource): Promise<void> {
    const opening = ++this.#opening;
    this.status = { kind: "opening", label: source.label };
    this.#bump();
    try {
      const session = await Session.open(this.layerstack, source.root, source.fetch, {
        progress: (pending) => {
          if (opening === this.#opening) this.opening = pending;
          this.#bumpSoon();
        },
      });
      if (opening !== this.#opening) {
        session.free();
        return;
      }
      this.opening = null;
      this.session?.free();
      this.session = session;
      this.source = source;
      this.selection = null;
      this.property = null;
      this.through = null;
      this.#viewport?.lookThrough(null);
      this.#orient();
      this.pause();
      this.range = timeRange(session);
      this.time = this.range?.start;
      this.status = { kind: "ready" };
      this.#sync();
    } catch (error) {
      if (opening !== this.#opening) return;
      this.opening = null;
      this.status = { kind: "failed", label: source.label, message: error instanceof Error ? error.message : String(error) };
      this.#bump();
    }
  }

  /** Selects the prim at `path` (`null` for none); the explain panel keeps its property while it is the prim's. */
  select(path: string | null): void {
    if (path === this.selection) return;
    this.selection = path;
    if (this.property !== null && (path === null || !this.property.startsWith(`${path}.`))) this.property = null;
    this.#viewport?.update(this.scene, this.#noUpdate(), path);
    this.#bump();
  }

  /** Shows `path`'s value and opinions in the explain panel. */
  explain(path: string | null): void {
    this.property = path;
    this.#bump();
  }

  /** Looks through the stage camera at `path`, or the free camera with `null`. */
  lookThrough(path: string | null): void {
    this.through = path;
    this.#viewport?.lookThrough(path);
    this.#bump();
  }

  /** Frames the free camera on the selection, or the whole stage. */
  frameSelection(): void {
    const stage = this.session?.stage;
    if (stage === undefined || this.#viewport === null) return;
    const bounds = stage.worldBounds([this.selection ?? "/"], {
      purposes: ["default", "proxy"],
      ...(this.time === undefined ? {} : { time: this.time }),
    }).alignedRanges;
    if (bounds[0]! > bounds[3]!) return;
    this.lookThrough(null);
    this.#viewport.frame(Array.from(bounds));
  }

  /** Reads everything at `time` (clamped to the range): the draws move to it, the panels read it. */
  setTime(time: number | undefined): void {
    const range = this.range;
    const clamped = time === undefined || range === null ? time : Math.min(range.end, Math.max(range.start, time));
    if (clamped === this.time) return;
    this.time = clamped;
    this.#sync();
  }

  /** Plays the range from the current time at `timeCodesPerSecond`, looping at its end. */
  play(): void {
    const range = this.range;
    if (range === null || this.playing) return;
    this.playing = true;
    let last = performance.now();
    const tick = (now: number) => {
      if (!this.playing) return;
      const advanced = (this.time ?? range.start) + ((now - last) / 1000) * range.timeCodesPerSecond;
      last = now;
      this.time = advanced > range.end ? range.start : advanced;
      this.#sync();
      this.#playFrame = requestAnimationFrame(tick);
    };
    this.#playFrame = requestAnimationFrame(tick);
    this.#bump();
  }

  /** Stops playing, on the nearest whole time code: an edit then authors its sample on a frame. */
  pause(): void {
    if (!this.playing) return;
    this.playing = false;
    cancelAnimationFrame(this.#playFrame);
    if (this.time !== undefined) this.time = Math.round(this.time);
    this.#sync();
  }

  /** Moves time by `frames` whole time codes, pausing. */
  step(frames: number): void {
    this.pause();
    if (this.range === null) return;
    this.setTime(Math.round(this.time ?? this.range.start) + frames);
  }

  /** Where edits go: a layer of the stage's stack or an edit target; `undefined` for the stage's edit layer. */
  setTarget(target: Layer | EditTarget | undefined): void {
    if (this.session === null) return;
    this.session.target = target;
    this.#bump();
  }

  /** Applies `edits` as one undoable action. */
  apply(label: string, edits: readonly Edit[]): void {
    this.#act(label, (session) => session.apply(label, edits));
  }

  /** Authors `value` for the attribute at `path`: its default, or its sample at `time`. */
  setValue(path: string, value: ValueInput, time: number | null = null): void {
    this.#act(time === null ? `set ${path}` : `set ${path} at ${time}`, (session) => session.setValue(path, value, time));
  }

  /** Selects `variant` of `set` on `prim`, then fetches what it needs. */
  async selectVariant(prim: string, set: string, variant: string): Promise<void> {
    await this.#actAsync(`select ${set}=${variant}`, (session) => session.selectVariant(prim, set, variant));
  }

  async undo(): Promise<void> {
    await this.#actAsync("undo", (session) => session.undo());
  }

  async redo(): Promise<void> {
    await this.#actAsync("redo", (session) => session.redo());
  }

  async setMuted(layer: Layer, muted: boolean): Promise<void> {
    await this.#actAsync(muted ? "mute" : "unmute", (session) => session.setMuted(layer, muted));
  }

  /** Forgets a notice. */
  dismiss(id: number): void {
    const index = this.notices.findIndex((notice) => notice.id === id);
    if (index >= 0) this.notices.splice(index, 1);
    this.#bump();
  }

  #act(action: string, run: (session: Session) => EditReceipt | void): void {
    const session = this.session;
    if (session === null) return;
    try {
      run(session);
    } catch (error) {
      this.#report(action, error);
    }
    this.#sync();
  }

  async #actAsync(action: string, run: (session: Session) => Promise<unknown>): Promise<void> {
    const session = this.session;
    if (session === null) return;
    try {
      await run(session);
    } catch (error) {
      this.#report(action, error);
    }
    if (this.session === session) this.#sync();
  }

  /** Orients the view to the stage's `upAxis`. */
  #orient(): void {
    const stage = this.session?.stage;
    if (stage === undefined || this.#viewport === null) return;
    const up = stage.metadata("upAxis");
    this.#viewport.setUpAxis(up?.kind === "scalar" && up.value === "Z" ? "Z" : "Y");
  }

  #report(action: string, error: unknown): void {
    const code = typeof error === "object" && error !== null && "code" in error ? String((error as { code: unknown }).code) : null;
    this.notices.push({ id: ++this.#notice, action, message: error instanceof Error ? error.message : String(error), code });
  }

  /** Brings the draw list up to the stage, applies what changed to the view, and bumps the revision. */
  #sync(): void {
    if (this.session !== null) {
      const update = this.#draws.sync(this.session, this.time);
      this.scene = this.#draws.scene(this.time);
      if (update.reset || update.records > 0 || update.timeChanged) this.lastUpdate = update;
      this.#viewport?.update(this.scene, update, this.selection);
    }
    this.#bump();
  }

  /** An update that changes nothing: a restyle for a new selection. */
  #noUpdate(): ViewportUpdate {
    const none = new Set<never>();
    return {
      reset: false,
      records: 0,
      timeChanged: false,
      added: none,
      removed: none,
      matrices: none,
      visibility: none,
      recolored: none,
      rebound: none,
      buffersAdded: none,
      refilled: none,
      rebuilt: none,
      freed: none,
      lightsAdded: none,
      lightsRemoved: none,
      lightsChanged: none,
      cameras: none,
      materialsAdded: none,
      materialsRemoved: none,
      materialsChanged: none,
      links: 0,
      work: this.lastUpdate?.work ?? ({} as ViewportUpdate["work"]),
    };
  }

  /** Bumps once per frame at most: the asset loop reports every fetch. */
  #bumpSoon(): void {
    if (this.#frame !== 0) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      this.#bump();
    });
  }

  #bump(): void {
    this.revision += 1;
    for (const listener of this.#listeners) listener();
  }
}

/** The stage's time range from its metadata, `null` when it authors no span (`endTimeCode` not after `startTimeCode`). */
function timeRange(session: Session): TimeRange | null {
  const number = (key: string, fallback: number) => {
    const value = session.stage.metadata(key);
    return value?.kind === "scalar" && typeof value.value === "number" ? value.value : fallback;
  };
  const start = number("startTimeCode", 0);
  const end = number("endTimeCode", 0);
  if (!(end > start)) return null;
  return { start, end, timeCodesPerSecond: number("timeCodesPerSecond", 24) };
}

/** Re-renders the component after every change to `store`; read the store and its stage directly. */
export function useEditor(store: EditorStore): number {
  return useSyncExternalStore(store.subscribe, () => store.revision);
}
