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
import type { Edit, EditReceipt, EditTarget, Layer, Layerstack, LoadOutcome, LoadPolicy, ValueInput } from "@forest-rs/layerstack";
import type { DrawCost, DrawScope, Stagehand } from "@forest-rs/layerstack_stagehand";
import { type AssetFocus, assetFocus, type DrawScopeChoice, overLimit, scopeFor } from "./model/assetView.ts";
import { ViewportModel, type ViewportScene, type ViewportUpdate } from "./model/draws.ts";
import { type LoadBudget, type OpenPhase, OverBudget, Session } from "./model/session.ts";
import type { SceneSource } from "./model/sources.ts";
import type { Viewport } from "./viewport/viewport3d.ts";

/** Where the editor is with its scene. */
export type EditorStatus =
  | { readonly kind: "empty" }
  | { readonly kind: "opening"; readonly label: string }
  /** Loading payloads of the open scene (`label` names them), and fetching what they want. */
  | { readonly kind: "loading"; readonly label: string }
  | { readonly kind: "ready" }
  | { readonly kind: "failed"; readonly label: string; readonly message: string }
  /**
   * A Wasm module trapped: out of memory (`module.out_of_memory`, or a raw
   * trap the stagehand module reports) or a panic. Its instance is dead
   * for the rest of the page, so the editor stops calling into it; only a
   * reload brings it back. `label` is the scene being opened, if any.
   */
  | { readonly kind: "lost"; readonly label: string | null; readonly outOfMemory: boolean; readonly message: string };

/**
 * Whether `error` (or an error it was caused by) is a Wasm module's trap,
 * after which the module cannot be used again: a `LayerstackError` with a
 * `module.trapped` or `module.out_of_memory` code, or a raw
 * `WebAssembly.RuntimeError`.
 */
export function moduleTrap(error: unknown): { readonly outOfMemory: boolean; readonly message: string; readonly raw: boolean } | null {
  for (let cause = error, depth = 0; cause instanceof Error && depth < 4; cause = cause.cause, depth += 1) {
    const code = (cause as { code?: unknown }).code;
    if (code === "module.out_of_memory" || code === "module.trapped") {
      return { outOfMemory: code === "module.out_of_memory", message: cause.message, raw: false };
    }
    if (cause instanceof WebAssembly.RuntimeError) return { outOfMemory: false, message: cause.message, raw: true };
  }
  return null;
}

/**
 * How a scene opens its payloads: all of them, none (each prim with
 * payloads then loads from the prim tree), or `auto`: none, then all of
 * them unless reading them would pass {@link PAYLOAD_BUDGET}.
 */
export type PayloadChoice = "auto" | "all" | "none";

/**
 * The layer bytes an automatic open reads for payloads before it leaves
 * them unloaded: 1 GiB. Composing takes more memory than the layers' bytes
 * (a large public scene's 3.1 GB of layers composes to 5.4 GB natively),
 * and the module has 4 GiB for the stage and its draws, so a scene whose
 * payloads read more than this would not fit with them loaded.
 */
export const PAYLOAD_BUDGET = 1024 ** 3;

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
  skinning: { gpu: 0, cpu: 0, shaped: 0, shapedVarying: 0, reducedBlocks: 0, droppedWeight: 0, failed: [] },
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
  /** How the next scene opens its payloads ({@link PayloadChoice}). */
  payloads: PayloadChoice = "auto";
  /** Why the open scene's payloads were left unloaded; `null` when they were not, or were chosen so. */
  payloadNote: string | null = null;
  /** What the open is doing, and since when (`performance.now()`); `null` when nothing is opening. */
  openingPhase: { readonly phase: OpenPhase; readonly since: number; readonly started: number } | null = null;
  /** The draws, as last synced. */
  scene: ViewportScene = EMPTY_SCENE;
  /** The last sync that changed something, for the stats. */
  lastUpdate: ViewportUpdate | null = null;
  /** What the draws hold, measured after the last sync; `null` with no scene. */
  cost: DrawCost | null = null;
  /**
   * What the viewport draws: the whole scene, the selection alone (the
   * asset view, {@link assetFocus}), or nothing. A scene over
   * {@link DRAW_LIMIT} opens drawing the selection; {@link scopeNote} says
   * why.
   */
  drawScope: DrawScopeChoice = "scene";
  /** Why the scene opened without drawing it whole; `null` when it did. */
  scopeNote: string | null = null;
  /** What drawing the whole scene would cost, estimated when it opened. */
  sceneEstimate: DrawCost | null = null;
  /** What the asset view makes of the selection, as last synced. */
  focus: AssetFocus = { kind: "none" };
  /** Why the asset view does not draw the selection (over {@link DRAW_LIMIT}); `null` when it does, or when the selection draws nothing anyway. */
  focusNote: string | null = null;
  /** The asset view's last decision, so a sync does not estimate the same selection again: its key, scope and note. */
  #focusScope: { key: string; scope: DrawScope; note: string | null } | null = null;
  /** What actions reported, newest last. */
  readonly notices: Notice[] = [];
  readonly #draws: ViewportModel;
  readonly #listeners = new Set<() => void>();
  #viewport: Viewport | null = null;
  #notice = 0;
  #opening = 0;
  /** When the open, or the payload load, the overlay shows began. */
  #busySince = 0;
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
    viewport.onImages = () => this.#bumpSoon();
    viewport.lookThrough(this.through);
    this.#orient();
    const update = this.session === null ? null : this.#draws.sync(this.session, this.time, this.#scope(this.session));
    if (update !== null) {
      this.scene = this.#draws.scene(this.time);
      this.cost = this.#draws.cost();
      viewport.update(this.scene, { ...update, reset: true }, this.#highlighted());
    }
  }

  /** The source's root, opened and resolved: everything it depends on is fetched first. */
  async open(source: SceneSource): Promise<void> {
    const opening = ++this.#opening;
    this.status = { kind: "opening", label: source.label };
    this.#bump();
    try {
      this.#busySince = performance.now();
      const choice = this.payloads;
      const session = await Session.open(this.layerstack, source.root, source.fetch, {
        open: { load: choice === "all" ? "all" : "none" },
        progress: (pending) => {
          if (opening === this.#opening) this.opening = pending;
          this.#bumpSoon();
        },
        // Each phase is shown before its work holds the thread: two frames
        // let the page render and paint it.
        phase: async (phase) => {
          if (opening !== this.#opening) return;
          this.openingPhase = { phase, since: performance.now(), started: this.#busySince };
          this.#bump();
          await nextFrame();
          await nextFrame();
        },
      });
      if (opening !== this.#opening) {
        session.free();
        return;
      }
      const note = choice === "auto" ? await this.#loadWithin(session, source) : null;
      if (opening !== this.#opening) {
        session.free();
        return;
      }
      this.payloadNote = note;
      this.opening = null;
      this.openingPhase = null;
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
      this.#decideScope(session);
      this.#sync();
    } catch (error) {
      if (opening !== this.#opening) return;
      this.opening = null;
      this.openingPhase = null;
      if (this.lose(error, source.label)) return;
      this.status = { kind: "failed", label: source.label, message: error instanceof Error ? error.message : String(error) };
      this.#bump();
    }
  }

  /**
   * Stops the editor if `error` is a module's trap ({@link moduleTrap}):
   * the status becomes `lost`, playback stops, and nothing calls into the
   * modules again (the frame shows why instead of the panels). Returns
   * whether it was one.
   */
  lose(error: unknown, label: string | null = null): boolean {
    const trap = moduleTrap(error);
    if (trap === null) return false;
    if (this.status.kind === "lost") return true;
    // The layerstack wrapper translates its module's traps; a raw one is
    // the stagehand module's, which says why itself.
    const outOfMemory = trap.raw ? this.stagehand.trapCause() === "outOfMemory" : trap.outOfMemory;
    const message = trap.raw ? `the draw module trapped: ${trap.message}` : trap.message;
    // The stack says which module trapped, and where.
    console.error(error);
    cancelAnimationFrame(this.#playFrame);
    this.playing = false;
    this.status = { kind: "lost", label: label ?? this.source?.label ?? null, outOfMemory, message };
    this.#bump();
    return true;
  }

  /**
   * Stops waiting for the scene being opened: the stage keeps the scene it
   * had. Work already holding the thread finishes, and its result is
   * dropped.
   */
  cancelOpen(): void {
    if (this.status.kind !== "opening") return;
    this.#opening += 1;
    this.opening = null;
    this.openingPhase = null;
    this.status = this.session === null ? { kind: "empty" } : { kind: "ready" };
    this.#bump();
  }

  /**
   * Selects the prim at `path` (`null` for none); the explain panel keeps
   * its property while it is the prim's. Drawing the selection, the asset
   * view draws it instead.
   */
  select(path: string | null): void {
    if (path === this.selection) return;
    this.selection = path;
    if (this.property !== null && (path === null || !this.property.startsWith(`${path}.`))) this.property = null;
    if (this.drawScope === "selection") {
      this.#sync();
      return;
    }
    this.#viewport?.update(this.scene, this.#noUpdate(), this.#highlighted());
    this.#bump();
  }

  /** Shows `path`'s value and opinions in the explain panel (and, drawing the selection, an image it names). */
  explain(path: string | null): void {
    this.property = path;
    if (this.drawScope === "selection") this.#sync();
    else this.#bump();
  }

  /** Draws the whole scene, the selection alone, or nothing, from now on. */
  setDrawScope(choice: DrawScopeChoice): void {
    if (choice === this.drawScope) return;
    this.drawScope = choice;
    this.scopeNote = null;
    this.#sync();
  }

  /**
   * Chooses what a newly opened scene draws: the whole scene, unless its
   * estimated draws are over {@link DRAW_LIMIT}; then the selection, and
   * {@link scopeNote} says why.
   */
  #decideScope(session: Session): void {
    this.#focusScope = null;
    try {
      this.sceneEstimate = this.#draws.estimate(session.stage, this.time, { roots: ["/"] });
    } catch (error) {
      if (moduleTrap(error) !== null) throw error;
      this.sceneEstimate = null;
    }
    const over = this.sceneEstimate === null ? null : overLimit(this.sceneEstimate.total);
    this.drawScope = over === null ? "scene" : "selection";
    this.scopeNote = over === null ? null : `This scene is not drawn whole: ${over}. Select a prim to draw it alone, or choose Scene.`;
  }

  /**
   * The draw list's scope: {@link scopeFor} the choice and the asset
   * view's focus, except that a selection over {@link DRAW_LIMIT} is not
   * drawn ({@link focusNote} says why). Decided once per selection,
   * property and recomposition.
   */
  #scope(session: Session): DrawScope {
    const stage = session.stage;
    this.focus = assetFocus(stage, this.selection, this.property, this.time);
    if (this.drawScope !== "selection") {
      this.focusNote = null;
      return scopeFor(this.drawScope, this.focus);
    }
    const scope = scopeFor("selection", this.focus);
    const key = `${JSON.stringify(scope)} ${session.changeLog.length}`;
    if (this.#focusScope?.key !== key) {
      let note: string | null = null;
      if (scope.roots.length > 0) {
        const over = overLimit(this.#draws.estimate(stage, this.time, scope).total);
        if (over !== null) note = `${scope.roots[0]} is not drawn: ${over}. Select a part of it.`;
      }
      this.#focusScope = { key, scope: note === null ? scope : { roots: [] }, note };
    }
    this.focusNote = this.#focusScope.note;
    return this.#focusScope.scope;
  }

  /** The prim the viewport highlights: the selection, or, drawing it alone, only a face subset of it. */
  #highlighted(): string | null {
    if (this.drawScope !== "selection") return this.selection;
    return this.focus.kind === "geometry" ? this.focus.subset : null;
  }

  /** Looks through the stage camera at `path`, or the free camera with `null`. */
  lookThrough(path: string | null): void {
    this.through = path;
    this.#viewport?.lookThrough(path);
    this.#bump();
  }

  /** Frames the free camera on the selection, or the whole stage; drawing the selection alone, on what is drawn. */
  frameSelection(): void {
    const stage = this.session?.stage;
    if (stage === undefined || this.#viewport === null) return;
    if (this.drawScope !== "scene") {
      if (this.scene.bounds === null) return;
      this.lookThrough(null);
      this.#viewport.frame(this.scene.bounds);
      return;
    }
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

  /**
   * Defines a child of `parent` named `name`, of `typeName` (typeless with
   * `null`), and selects it.
   */
  definePrim(parent: string, name: string, typeName: string | null): void {
    const path = parent === "/" ? `/${name}` : `${parent}/${name}`;
    const label = `define ${typeName ?? "typeless"} ${path}`;
    const edit = typeName === null ? { type: "definePrim" as const, path } : { type: "definePrim" as const, path, typeName };
    if (this.#act(label, (session) => session.apply(label, [edit]))) this.select(path);
  }

  /** Renames or reparents the prim at `path` to `to`, repairing the paths that name it, and keeps it selected. */
  move(path: string, to: string): void {
    if (this.#act(`move ${path}`, (session) => session.move(path, to)) && this.selection === path) this.select(to);
  }

  /** Removes the prim at `path` from the edit target's layer stack. */
  delete(path: string): void {
    if (this.#act(`delete ${path}`, (session) => session.delete(path)) && this.selection !== null && (this.selection === path || this.selection.startsWith(`${path}/`))) {
      this.select(null);
    }
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

  /**
   * Loads the payloads at `path` (and beneath it, with `"withDescendants"`)
   * and fetches what they want, the prim tree showing them loaded once
   * they are; the overlay shows the rounds as an open's.
   */
  async load(path: string, policy: LoadPolicy = "withDescendants"): Promise<void> {
    await this.#loading(`load ${path}`, path, (session) => session.loadPayloads([path], [], policy));
  }

  /** Unloads the payloads at and beneath `path`: their prims leave the stage and the draws. */
  async unload(path: string): Promise<void> {
    await this.#loading(`unload ${path}`, path, (session) => session.loadPayloads([], [path]));
  }

  /** Sets how the next scene opens its payloads. */
  setPayloads(choice: PayloadChoice): void {
    this.payloads = choice;
    this.#bump();
  }

  /**
   * Opens with payloads unloaded, then loads them all unless that would
   * read more than {@link PAYLOAD_BUDGET} of layers (weighed before each
   * round is fetched, from the source's sizes); then they are unloaded
   * again, and the note says why. Returns that note, or `null`.
   */
  async #loadWithin(session: Session, source: SceneSource): Promise<string | null> {
    if (session.stage.findLoadable().length === 0) return null;
    const budget: LoadBudget = { bytes: PAYLOAD_BUDGET, size: (identifier) => source.size(identifier) };
    try {
      await session.loadPayloads(["/"], [], "withDescendants", budget);
      return null;
    } catch (error) {
      if (!(error instanceof OverBudget)) throw error;
      session.stage.setLoadRules("none");
      return `Payloads are unloaded: loading them all would read ${formatGiB(error.bytes)} of layers, over the ${formatGiB(PAYLOAD_BUDGET)} an automatic open reads. Load prims from the prim tree.`;
    }
  }

  /** Runs a payload load or unload with the overlay up, then brings the draws up to it. */
  async #loading(action: string, label: string, run: (session: Session) => Promise<LoadOutcome>): Promise<void> {
    const session = this.session;
    if (session === null || this.status.kind !== "ready") return;
    this.status = { kind: "loading", label };
    this.#busySince = performance.now();
    this.openingPhase = null;
    this.#bump();
    try {
      // A path the stage refuses (an inactive prim) is skipped, as in
      // OpenUSD; the notice says which and why.
      for (const { path, code } of (await run(session)).refused) {
        this.notices.push({ id: ++this.#notice, action, message: `${path} was not ${action.startsWith("load") ? "loaded" : "unloaded"}: ${code.replaceAll("_", " ")}`, code: `load.${code}` });
      }
    } catch (error) {
      if (this.lose(error)) return;
      this.#report(action, error);
    }
    if (this.session !== session) return;
    this.opening = null;
    this.openingPhase = null;
    this.status = { kind: "ready" };
    this.#sync();
  }

  /** Forgets a notice. */
  dismiss(id: number): void {
    const index = this.notices.findIndex((notice) => notice.id === id);
    if (index >= 0) this.notices.splice(index, 1);
    this.#bump();
  }

  /** Runs an action; whether it succeeded (a failure is a notice). */
  #act(action: string, run: (session: Session) => EditReceipt | void): boolean {
    const session = this.session;
    if (session === null) return false;
    let done = true;
    try {
      run(session);
    } catch (error) {
      this.#report(action, error);
      done = false;
    }
    this.#sync();
    return done;
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
      const update = this.#draws.sync(this.session, this.time, this.#scope(this.session));
      this.scene = this.#draws.scene(this.time);
      this.cost = this.#draws.cost();
      if (update.reset || update.records > 0 || update.timeChanged || update.added.size + update.removed.size > 0) this.lastUpdate = update;
      this.#viewport?.update(this.scene, update, this.#highlighted());
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
      reskinned: none,
      skinned: none,
      skinWork: this.lastUpdate?.skinWork ?? { gpuDraws: 0, joints: 0, cpuDraws: 0, cpuPoints: 0, shapedDraws: 0, shapedPoints: 0, gathered: 0 },
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

/** Resolves after the next frame is rendered. */
function nextFrame(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
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

/** `bytes` in GiB, to one decimal. */
function formatGiB(bytes: number): string {
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}
