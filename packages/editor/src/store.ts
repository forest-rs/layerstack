// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's state outside React, as the panels see it: one object to
// read and to act through. What it does is in `store/`, a concern a
// module: opening a scene (`opening.ts`), the time and playback
// (`playback.ts`), the selection (`selection.ts`), how the viewport shows
// the scene (`view.ts`), the draws and the view they are applied to
// (`drawing.ts`), and what is reported (`notices.ts`). This file holds the
// session, wires those together, and runs the actions on the stage: each
// one runs, brings the draws up to the stage's change records, and bumps
// the signals of what it changed (`store/signals.ts`), which the panels
// listen to (`app/context.ts`). Nothing here knows about the DOM except
// the viewport it is handed.
import type { Edit, EditReceipt, EditTarget, Layer, Layerstack, LoadOutcome, LoadPolicy, ValueInput } from "@forest-rs/layerstack";
import type { BudgetReport, DrawCost, Stagehand } from "@forest-rs/layerstack_stagehand";
import type { AssetFocus, BudgetChoice, DrawScopeChoice } from "./model/assetView.ts";
import type { ViewportScene, ViewportUpdate } from "./model/draws.ts";
import { indexOfId, type InstancerColumns, toggled } from "./model/instancer.ts";
import type { Session } from "./model/session.ts";
import type { SceneSource } from "./model/sources.ts";
import { Drawing } from "./store/drawing.ts";
import { type FrameNote, FrameNotes, type Notice, Notices } from "./store/notices.ts";
import { type BusyPhase, OpenController, type PayloadChoice } from "./store/opening.ts";
import { Playback, type TimeRange, timeRange } from "./store/playback.ts";
import { cornersBox, instanceCorners, type InstanceSelection, Selection } from "./store/selection.ts";
import { type Channel, Signals } from "./store/signals.ts";
import { type EditorStatus, moduleTrap } from "./store/status.ts";
import { ViewSettings } from "./store/view.ts";
import type { LightSettings } from "./viewport/defaultLights.ts";
import { isBox, type UpAxis } from "./viewport/framing.ts";
import type { ToneMapChoice } from "./viewport/toneMap.ts";
import type { Viewport } from "./viewport/viewport3d.ts";

export { FRAME_NOTE_MS, type FrameNote, type Notice } from "./store/notices.ts";
export { PAYLOAD_BUDGET, type PayloadChoice } from "./store/opening.ts";
export type { TimeRange } from "./store/playback.ts";
export { type EditorStatus, moduleTrap } from "./store/status.ts";

/** The editor: one session at a time, and the state the panels share. */
export class EditorStore {
  readonly layerstack: Layerstack;
  readonly stagehand: Stagehand;
  /** What changed, by concern: what the panels listen to. */
  readonly signals = new Signals();
  /** Where the editor is with its scene. */
  status: EditorStatus = { kind: "empty" };
  /** The open scene's session; `null` before the first opens. */
  session: Session | null = null;
  /** Where the open scene's bytes come from. */
  source: SceneSource | null = null;
  /** Why the open scene's payloads were left unloaded; `null` when they were not, or were chosen so. */
  payloadNote: string | null = null;
  readonly #opener: OpenController;
  readonly #playback: Playback;
  readonly #selection = new Selection();
  readonly #view: ViewSettings;
  readonly #drawing: Drawing;
  readonly #notices: Notices;
  readonly #frameNotes: FrameNotes;
  #frame = 0;
  /** The channels {@link EditorStore.#bumpSoon} bumps at the next frame. */
  readonly #soon = new Set<Channel>();

  constructor(layerstack: Layerstack, stagehand: Stagehand) {
    this.layerstack = layerstack;
    this.stagehand = stagehand;
    const signals = this.signals;
    this.#notices = new Notices(signals);
    this.#frameNotes = new FrameNotes(signals);
    this.#playback = new Playback(
      () => this.#syncTime(),
      () => signals.bump("time"),
    );
    this.#drawing = new Drawing(stagehand, {
      request: () => ({
        session: this.session,
        time: this.time,
        drawScope: this.drawScope,
        budgetChoice: this.budgetChoice,
        selection: this.selection,
        property: this.property,
      }),
      resync: () => this.#sync(),
      changed: (channels, soon) => (soon ? this.#bumpSoon(...channels) : signals.bump(...channels)),
    });
    const viewport = () => this.viewport;
    this.#view = new ViewSettings(signals, {
      get viewport() {
        return viewport();
      },
      redraw: () => this.#sync("view"),
    });
    const hasSession = () => this.session !== null;
    this.#opener = new OpenController(layerstack, signals, {
      get hasSession() {
        return hasSession();
      },
      setStatus: (status) => this.#setStatus(status),
      commit: (session, source, note) => this.#commit(session, source, note),
      lose: (error, label) => this.lose(error, label),
    });
  }

  // What the panels read, each from the concern that owns it.

  /** The selected prim's path. */
  get selection(): string | null {
    return this.#selection.prim;
  }

  /**
   * The point instancer instance selected (in its inspector view, or
   * picked in the viewport): its instancer, which is the selection, and
   * its index in the instancer's arrays; `null` for none.
   */
  get instance(): InstanceSelection | null {
    return this.#selection.instance;
  }

  /** The property whose value the explain panel shows, by path. */
  get property(): string | null {
    return this.#selection.property;
  }

  /**
   * The time everything is read at: a time code, or `undefined` for the
   * default time (a stage without a time range).
   */
  get time(): number | undefined {
    return this.#playback.time;
  }

  /** The stage's time range; `null` when it authors none. */
  get range(): TimeRange | null {
    return this.#playback.range;
  }

  /** Whether time is playing. */
  get playing(): boolean {
    return this.#playback.playing;
  }

  /** The stage camera the viewport looks through, by path; `null` for the free camera. */
  get through(): string | null {
    return this.#view.through;
  }

  /** The stage's up axis (its `upAxis` metadata, `Y` when it has none): what the viewport and the previews orbit about. */
  get upAxis(): UpAxis {
    return this.#view.upAxis;
  }

  /** The default lights the viewport adds to the stage's, as usdview's View > Lights menu sets them: the camera light on. */
  get lightSettings(): LightSettings {
    return this.#view.lightSettings;
  }

  /** How the viewport and the previews are shown on the display: usdview's sRGB by default, or AgX; remembered for this viewer. */
  get toneMap(): ToneMapChoice {
    return this.#view.toneMap;
  }

  /**
   * What the viewport draws: the whole scene (as a scene opens), the
   * selection alone (the asset view), or nothing; each under the draw
   * budget ({@link EditorStore.budgetChoice}).
   */
  get drawScope(): DrawScopeChoice {
    return this.#view.drawScope;
  }

  /**
   * The draw budget this viewer chose, remembered across visits: what
   * does not fit is drawn as boxes or skipped, the selection admitted
   * first.
   */
  get budgetChoice(): BudgetChoice {
    return this.#view.budgetChoice;
  }

  /** The session being opened, while its assets are fetched. */
  get opening(): Session | null {
    return this.#opener.opening;
  }

  /** How the next scene opens its payloads ({@link PayloadChoice}). */
  get payloads(): PayloadChoice {
    return this.#opener.payloads;
  }

  /** What the open, or the payload load, is doing, and since when; `null` when nothing is opening or it has not said yet. */
  get openingPhase(): BusyPhase | null {
    return this.#opener.phase;
  }

  /** The draws, as last synced. */
  get scene(): ViewportScene {
    return this.#drawing.scene;
  }

  /** The last sync that changed something, for the stats. */
  get lastUpdate(): ViewportUpdate | null {
    return this.#drawing.lastUpdate;
  }

  /** What the draws hold, measured after the last sync; `null` with no scene. */
  get cost(): DrawCost | null {
    return this.#drawing.cost;
  }

  /** Where the draws stand against the budget, after the last sync; `null` without a budget or a scene. */
  get budget(): BudgetReport | null {
    return this.#drawing.budget;
  }

  /** What the asset view makes of the selection, as last synced. */
  get focus(): AssetFocus {
    return this.#drawing.focus;
  }

  /** The three.js view, once attached. */
  get viewport(): Viewport | null {
    return this.#drawing.viewport;
  }

  /** What actions reported, newest last. */
  get notices(): readonly Notice[] {
    return this.#notices.list;
  }

  /**
   * Why the last request to frame (the F key, the frame command, a
   * double-click on an instance) framed nothing, for a few seconds;
   * `null` otherwise, and once one frames.
   */
  get frameNote(): FrameNote | null {
    return this.#frameNotes.note;
  }

  // The viewport.

  /** Hands the store the three.js view (or `null` when it goes), which it keeps up to date. */
  attachViewport(viewport: Viewport | null): void {
    if (viewport !== null) this.#view.apply(viewport);
    this.signals.bump(...this.#drawing.attach(viewport));
  }

  // Opening.

  /** The source's root, opened and resolved: everything it depends on is fetched first. */
  open(source: SceneSource): Promise<void> {
    this.#drawing.forgetResident();
    return this.#opener.open(source);
  }

  /**
   * Stops waiting for the scene being opened: the stage keeps the scene it
   * had. Work already holding the thread finishes, and its result is
   * dropped.
   */
  cancelOpen(): void {
    if (this.status.kind !== "opening") return;
    this.#opener.cancel();
  }

  /** Sets how the next scene opens its payloads. */
  setPayloads(choice: PayloadChoice): void {
    this.#opener.setPayloads(choice);
  }

  /** Takes the session an open made as the scene: the one it had is freed, and everything starts over on the new one. */
  #commit(session: Session, source: SceneSource, note: string | null): void {
    this.payloadNote = note;
    this.session?.free();
    this.session = session;
    this.source = source;
    this.#selection.clear();
    this.viewport?.setMarker(null);
    this.#view.reset(upAxis(session));
    this.#playback.reset(timeRange(session.stage));
    this.status = { kind: "ready" };
    this.#drawing.sync(false);
    // Another scene: everything a panel reads may be another thing.
    this.signals.bumpAll();
  }

  #setStatus(status: EditorStatus): void {
    this.status = status;
    this.signals.bump("status");
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
    this.#playback.stop();
    this.status = { kind: "lost", label: label ?? this.source?.label ?? null, outOfMemory, message };
    this.signals.bump("status", "time");
    return true;
  }

  // The selection.

  /**
   * Selects the prim at `path` (`null` for none); the explain panel keeps
   * its property while it is the prim's. Drawing the selection, the asset
   * view draws it instead.
   */
  select(path: string | null): void {
    const instance = this.instance;
    if (!this.#selection.select(path)) return;
    if (instance !== null && this.instance === null) this.viewport?.setMarker(null);
    // Drawing the selection, or under a budget (which admits the selection first), the draws change.
    if (this.drawScope === "selection" || this.budgetChoice !== "unlimited") {
      this.#sync("selection");
      return;
    }
    this.#drawing.restyle();
    this.signals.bump("selection");
  }

  /** The columns of the point instancer at `path` at the current time, read again only when a change reaches it or the time moves. */
  instancerColumns(path: string): InstancerColumns | null {
    return this.session === null ? null : this.#selection.columns(this.session, path, this.time);
  }

  /**
   * Selects instance `index` of the point instancer at `instancer` (and
   * the instancer), outlining it in the viewport while the whole scene is
   * drawn, and with `frame` framing it; `null` clears it.
   */
  selectInstance(instancer: string, index: number | null, frame = false): void {
    if (this.selection !== instancer) this.select(instancer);
    this.#selection.instance = index === null ? null : { instancer, index };
    const corners = index === null ? null : this.#instanceCorners(instancer, index);
    this.viewport?.setMarker(corners);
    if (frame) this.#frameNotes.show(corners !== null && this.#frameBox(cornersBox(corners)), `instance ${index} of ${instancer}`);
    this.signals.bump("selection");
  }

  /** Selects the instance with `id` of the point instancer at `instancer` (a pick's), by its index. */
  selectInstanceById(instancer: string, id: bigint): void {
    const columns = this.instancerColumns(instancer);
    const index = columns === null ? -1 : indexOfId(columns, id);
    this.selectInstance(instancer, index < 0 ? null : index);
  }

  /** The corners of instance `index`'s box in the space the viewport draws in ({@link instanceCorners}). */
  #instanceCorners(instancer: string, index: number): Float64Array | null {
    const stage = this.session?.stage;
    if (stage === undefined) return null;
    return instanceCorners(stage, instancer, this.instancerColumns(instancer), index, this.time, { drawScope: this.drawScope, focus: this.focus });
  }

  /** Shows `path`'s value and opinions in the explain panel (and, drawing the selection, an image it names). */
  explain(path: string | null): void {
    this.#selection.property = path;
    if (this.drawScope === "selection") this.#sync("selection");
    else this.signals.bump("selection");
  }

  // How the viewport shows the scene.

  /** Draws the whole scene, the selection alone, or nothing, from now on. */
  setDrawScope(choice: DrawScopeChoice): void {
    this.#view.setDrawScope(choice);
  }

  /** Draws under the budget `choice` from now on, and remembers it for this viewer. */
  setBudgetChoice(choice: BudgetChoice): void {
    this.#view.setBudgetChoice(choice);
  }

  /** Looks through the stage camera at `path`, or the free camera with `null`. */
  lookThrough(path: string | null): void {
    this.#view.lookThrough(path);
  }

  /** Switches the viewport's default lights (usdview's View > Lights menu). */
  setLightSettings(settings: LightSettings): void {
    this.#view.setLightSettings(settings);
  }

  /** Shows the viewport and the previews sRGB-encoded as usdview does, or through AgX, and remembers the choice. */
  setToneMap(choice: ToneMapChoice): void {
    this.#view.setToneMap(choice);
  }

  /**
   * What drawing the whole scene would cost in full, estimated from counts
   * (nothing built), at the current time; made when first asked for and
   * again after an edit. `null` with no scene, or when it cannot be made.
   */
  sceneEstimate(): DrawCost | null {
    return this.#drawing.estimate();
  }

  /**
   * Frames the free camera on the selection (its chosen instance, for a
   * point instancer with one), or the whole stage; drawing the selection
   * alone, on what is drawn. It frames again what is in view already.
   * Returns whether it framed anything: not for a selection without
   * bounds (a material, a camera, an empty prim), which leaves the view
   * as it was and says so ({@link EditorStore.frameNote}).
   */
  frameSelection(): boolean {
    const stage = this.session?.stage;
    if (stage === undefined || this.viewport === null) return false;
    const instance = this.instance;
    if (instance !== null && instance.instancer === this.selection) {
      const corners = this.#instanceCorners(instance.instancer, instance.index);
      if (corners !== null) return this.#frameNotes.show(this.#frameBox(cornersBox(corners)), null);
    }
    if (this.drawScope !== "scene") return this.#frameNotes.show(this.scene.bounds !== null && this.#frameBox(this.scene.bounds), "what is drawn");
    const path = this.selection ?? "/";
    const bounds = Array.from(
      stage
        .worldBounds([path], {
          purposes: ["default", "proxy"],
          ...(this.time === undefined ? {} : { time: this.time }),
        })
        .alignedRanges.subarray(0, 6),
    );
    // A camera is not framed at its position: nothing is drawn there, so
    // the view would show nothing; the note points at looking through it.
    const camera = this.scene.cameras.some((each) => each.path === path);
    return this.#frameNotes.show(this.#frameBox(bounds), path === "/" ? "the stage" : path, camera ? "look through it instead" : null);
  }

  /**
   * Draws the prim at `path` alone (the asset view) and frames it: also
   * when it is drawn alone already, so asking again brings it back into
   * view.
   */
  drawAlone(path: string): void {
    this.select(path);
    this.setDrawScope("selection");
    this.frameSelection();
  }

  /** Frames `box` with the free camera; `false`, the view as it was, when it is not a box. */
  #frameBox(box: readonly number[]): boolean {
    const viewport = this.viewport;
    if (viewport === null || !isBox(box)) return false;
    this.lookThrough(null);
    return viewport.frame(box);
  }

  // The time.

  /** Reads everything at `time` (clamped to the range): the draws move to it, the panels read it. */
  setTime(time: number | undefined): void {
    this.#playback.setTime(time);
  }

  /** Plays the range from the current time at `timeCodesPerSecond`, looping at its end. */
  play(): void {
    this.#playback.play();
  }

  /** Stops playing, on the nearest whole time code: an edit then authors its sample on a frame. */
  pause(): void {
    this.#playback.pause();
  }

  /** Moves time by `frames` whole time codes, pausing. */
  step(frames: number): void {
    this.#playback.step(frames);
  }

  // Actions on the stage.

  /** Where edits go: a layer of the stage's stack or an edit target; `undefined` for the stage's edit layer. */
  setTarget(target: Layer | EditTarget | undefined): void {
    if (this.session === null) return;
    this.session.target = target;
    this.signals.bump("target");
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

  /**
   * Hides instance `id` of the point instancer at `instancer` (adds it to
   * `invisibleIds`, at the current time when that attribute is animated,
   * `UsdGeomPointInstancer::InvisibleIds`) or shows it again, as one edit
   * where edits go. Nothing when it already is.
   */
  setInstanceVisible(instancer: string, id: bigint, visible: boolean): void {
    const stage = this.session?.stage;
    const columns = this.instancerColumns(instancer);
    if (stage === undefined || columns === null || columns.invisibleIds.includes(id) !== visible) return;
    const attribute = stage.attribute(`${instancer}.invisibleIds`);
    const at = attribute?.mightBeTimeVarying() === true ? (this.time ?? null) : null;
    const value = { type: "int64[]", value: BigInt64Array.from(toggled(columns.invisibleIds, id)) } as const;
    this.apply(`${visible ? "show" : "hide"} instance ${id} of ${instancer}`, [
      { type: "setValue", path: `${instancer}.invisibleIds`, value, ...(at === null ? {} : { time: at }) },
    ]);
  }

  /**
   * Deactivates instance `id` of the point instancer at `instancer` (adds
   * it to the `inactiveIds` list op metadata, `UsdGeomPointInstancer::
   * DeactivateId`) or activates it again, as one edit where edits go.
   * Nothing when it already is.
   */
  setInstanceActive(instancer: string, id: bigint, active: boolean): void {
    const columns = this.instancerColumns(instancer);
    if (columns === null || columns.inactiveIds.includes(id) !== active) return;
    const value = { type: "int64[]", value: BigInt64Array.from(toggled(columns.inactiveIds, id)) } as const;
    this.apply(`${active ? "activate" : "deactivate"} instance ${id} of ${instancer}`, [{ type: "setMetadata", path: instancer, key: "inactiveIds", value }]);
  }

  /** Selects `variant` of `set` on `prim`, then fetches what it needs. */
  async selectVariant(prim: string, set: string, variant: string): Promise<void> {
    await this.#actAsync(`select ${set}=${variant}`, (session) => session.selectVariant(prim, set, variant));
  }

  /** Undoes the newest action, then fetches what that brings back. */
  async undo(): Promise<void> {
    await this.#actAsync("undo", (session) => session.undo());
  }

  /** Redoes the most recently undone action, then fetches what it needs. */
  async redo(): Promise<void> {
    await this.#actAsync("redo", (session) => session.redo());
  }

  /** Mutes or unmutes a layer, by identifier for a muted layer the stage released. */
  async setMuted(layer: Layer | string, muted: boolean): Promise<void> {
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

  /**
   * Unloads the payloads at and beneath `path`: their prims leave the
   * stage and the draws, and the layers nothing uses any more are
   * released, so their memory comes back; loading them again fetches them.
   */
  async unload(path: string): Promise<void> {
    await this.#loading(`unload ${path}`, path, async (session) => {
      const outcome = await session.loadPayloads([], [path]);
      session.releaseUnusedLayers();
      return outcome;
    });
  }

  /** Runs a payload load or unload with the overlay up, then brings the draws up to it. */
  async #loading(action: string, label: string, run: (session: Session) => Promise<LoadOutcome>): Promise<void> {
    const session = this.session;
    if (session === null || this.status.kind !== "ready") return;
    this.#opener.begin();
    this.#setStatus({ kind: "loading", label });
    try {
      // A path the stage refuses (an inactive prim) is skipped, as in
      // OpenUSD; the notice says which and why.
      for (const { path, code } of (await run(session)).refused) {
        this.#notices.add(action, `${path} was not ${action.startsWith("load") ? "loaded" : "unloaded"}: ${code.replaceAll("_", " ")}`, `load.${code}`);
      }
    } catch (error) {
      if (this.lose(error)) return;
      this.#notices.report(action, error);
    }
    if (this.session !== session) return;
    this.#opener.end();
    this.status = { kind: "ready" };
    this.#sync("status", "loop");
  }

  /** Forgets a notice. */
  dismiss(id: number): void {
    this.#notices.dismiss(id);
  }

  /** Runs an action; whether it succeeded (a failure is a notice). */
  #act(action: string, run: (session: Session) => EditReceipt | void): boolean {
    const session = this.session;
    if (session === null) return false;
    let done = true;
    try {
      run(session);
    } catch (error) {
      this.#notices.report(action, error);
      done = false;
    }
    this.#sync("history");
    return done;
  }

  async #actAsync(action: string, run: (session: Session) => Promise<unknown>): Promise<void> {
    const session = this.session;
    if (session === null) return;
    try {
      await run(session);
    } catch (error) {
      this.#notices.report(action, error);
    }
    if (this.session === session) this.#sync("history", "loop");
  }

  // Keeping the draws and the panels up to date.

  /**
   * Brings the draws up to the stage and tells the panels: `draws`,
   * `composition` when the stage reported a change since the last sync,
   * and `also`.
   */
  #sync(...also: readonly Channel[]): void {
    const changed = this.#drawing.sync(false);
    this.#outline();
    this.signals.bump(...also, ...changed);
  }

  /**
   * Brings the draws to the time, which moved: `time`, and `draws` only
   * when the move did more than place draws elsewhere, so playing a scene
   * of rigid motion renders what shows the time and no more.
   */
  #syncTime(): void {
    const changed = this.#drawing.sync(true);
    this.#outline();
    this.signals.bump("time", ...changed);
  }

  /** The selected instance's outline follows edits, time and the draw scope. */
  #outline(): void {
    const instance = this.instance;
    if (instance !== null && this.session !== null) this.viewport?.setMarker(this.#instanceCorners(instance.instancer, instance.index));
  }

  /** Bumps `channels` once per frame at most. */
  #bumpSoon(...channels: readonly Channel[]): void {
    for (const channel of channels) this.#soon.add(channel);
    if (this.#frame !== 0) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      const due = [...this.#soon];
      this.#soon.clear();
      this.signals.bump(...due);
    });
  }
}

/** The stage's `upAxis`: `Z` when it says so, else `Y`. */
function upAxis(session: Session): UpAxis {
  const up = session.stage.metadata("upAxis");
  return up?.kind === "scalar" && up.value === "Z" ? "Z" : "Y";
}
