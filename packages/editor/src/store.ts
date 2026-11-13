// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's state outside React, as the panels see it: one object to
// read and to act through. What it does is in `store/`, a concern a
// module: opening a scene (`opening.ts`), the time and playback
// (`playback.ts`), the selection (`selection.ts`), how the viewport shows
// the scene (`view.ts`), the draws and the view they are applied to
// (`drawing.ts`), and what is reported (`notices.ts`). This file holds the
// session, wires those together, and runs the actions on the stage, one
// at a time in the order asked (`actions.ts`): each one runs, brings the
// draws up to the stage's change records, and bumps the signals of what
// it changed (`store/signals.ts`), which the panels listen to
// (`app/context.ts`). Nothing here knows about the DOM except the
// viewport it is handed.
//
// To add an action: a method here that calls `#edit` (it only edits the
// stage) or `#fetching` (it may want assets after) with what to do to the
// session; state that is not the stage's goes in the module of its
// concern, with a getter here and a bump of its channel.
import type {
  Edit,
  EditReceipt,
  EditTarget,
  FlattenResult,
  HistoryStep,
  Layer,
  LayerFormat,
  Layerstack,
  LoadOutcome,
  LoadPolicy,
  PackageResult,
  SaveResult,
  ValueInput,
} from "@forest-rs/layerstack";
import type { BudgetReport, DrawCost, Stagehand } from "@forest-rs/layerstack_stagehand";
import type { AssetFocus, BudgetChoice, DrawScopeChoice } from "./model/assetView.ts";
import { type NamespaceAction, namespaceRefusal, type NewArc, newArcLabel, removeArcLabel } from "./model/composition.ts";
import type { ViewportScene, ViewportUpdate } from "./model/draws.ts";
import { planSave, type SaveEntry } from "./model/files.ts";
import { indexOfId, type InstancerColumns, toggled } from "./model/instancer.ts";
import { type AuthoredArc, type LayerSpecs, type LayerSummary, readLayerSpecs, summarizeBytes, summaryOf } from "./model/layerSpecs.ts";
import type { LayerTextRead } from "./model/layerText.ts";
import { type Shadow, shadowOf, targetOpinion } from "./model/propertyOps.ts";
import { releaseValue } from "./model/values.ts";
import { addShaderEdits, connectEdits, connectLabel, disconnectEdits, disconnectLabel } from "./model/networkEdits.ts";
import type { HealthReport } from "./model/health.ts";
import { IDENTITY, type LayerOffset, movedSublayers, offsetUsda } from "./model/layerStack.ts";
import type { PathMove, Session } from "./model/session.ts";
import { type FieldSource, type MetadataPlacement, metadataLayer } from "./model/stageMetadata.ts";
import { StageReports } from "./store/reports.ts";
import type { SceneSource } from "./model/sources.ts";
import { ActionQueue } from "./store/actions.ts";
import { Drawing } from "./store/drawing.ts";
import { type EditLayerChoice, FileState } from "./store/files.ts";
import { type FrameNote, FrameNotes, type Notice, Notices } from "./store/notices.ts";
import { type BusyPhase, OpenController, type PayloadChoice } from "./store/opening.ts";
import { Playback, type TimeRange, timeRange } from "./store/playback.ts";
import { cornersBox, instanceCorners, type InstanceSelection, Selection } from "./store/selection.ts";
import { type Channel, Signals } from "./store/signals.ts";
import { type EditorStatus, moduleTrap } from "./store/status.ts";
import { type BottomTab, type SceneTab, ViewSettings } from "./store/view.ts";
import type { LightSettings } from "./viewport/defaultLights.ts";
import { isBox, type UpAxis } from "./viewport/framing.ts";
import { nextKey, selectionKeys } from "./model/keyTimes.ts";
import { rememberLoop } from "./model/playbackLoop.ts";
import type { ToneMapChoice } from "./viewport/toneMap.ts";
import type { Viewport } from "./viewport/viewport3d.ts";

export type { EditLayerChoice } from "./store/files.ts";
export { FRAME_NOTE_MS, type FrameNote, type Notice } from "./store/notices.ts";
export { PAYLOAD_BUDGET, type PayloadChoice } from "./store/opening.ts";
export type { TimeRange } from "./store/playback.ts";
export { type EditorStatus, moduleTrap } from "./store/status.ts";

/** A composition edit made here ({@link EditorStore.compositionEdit}): what the inspector offers to show. */
export interface CompositionEdit {
  /** The prim it edited. */
  readonly prim: string;
  /** The action, as History lists it. */
  readonly label: string;
  /** The layer it wrote. */
  readonly layer: Layer;
  /** The step it made: the edit is offered while this is the newest. */
  readonly step: HistoryStep;
  /** The variant it added and selected, when it did: offered to edit inside. */
  readonly variant?: { readonly set: string; readonly variant: string };
}

/**
 * An action that may wait on its assets: its refusal, known at once when
 * it ran now (`undefined` when the stage took it, or when it waits its
 * turn: a refusal is then a notice alone), and its end.
 */
export interface Authoring {
  readonly refusal: string | undefined;
  readonly done: Promise<void>;
}

/** The most bytes of a layer the stage has not read that are read for its default and root prims alone. */
export const SUMMARY_BYTES = 16 * 1024 ** 2;

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
  /**
   * The last move of a prim the store followed (a rename, or one undone
   * or redone): another object each time, for what keeps paths of its
   * own (the prim tree's open rows) to follow it too.
   */
  renamed: PathMove | null = null;
  readonly #actions: ActionQueue;
  readonly #opener: OpenController;
  readonly #playback: Playback;
  readonly #selection = new Selection();
  readonly #view: ViewSettings;
  readonly #drawing: Drawing;
  readonly #notices: Notices;
  readonly #frameNotes: FrameNotes;
  readonly #files: FileState;
  readonly #reports = new StageReports();
  /** The session whose metadata {@link EditorStore.#readStage} last read, and the change records it had then. */
  #read: { session: Session | null; changes: number } = { session: null, changes: 0 };
  /** The last value edit that does not show ({@link EditorStore.shadow}). */
  #shadow: Shadow | null = null;
  /** The last composition edit made here ({@link EditorStore.compositionEdit}). */
  #compositionEdit: CompositionEdit | null = null;
  /** What each layer authors about composition, read once per change of the stage ({@link EditorStore.layerSpecs}). */
  #specs: { session: Session | null; stamp: string; read: Map<Layer, LayerSpecs | null> } = { session: null, stamp: "", read: new Map() };
  #frame = 0;
  /** The channels {@link EditorStore.#bumpSoon} bumps at the next frame. */
  readonly #soon = new Set<Channel>();

  constructor(layerstack: Layerstack, stagehand: Stagehand) {
    this.layerstack = layerstack;
    this.stagehand = stagehand;
    const signals = this.signals;
    this.#actions = new ActionQueue(() => signals.bump("status"));
    this.#notices = new Notices(signals);
    this.#frameNotes = new FrameNotes(signals);
    this.#files = new FileState(signals);
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
      settle: () => this.#actions.cancel(),
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

  /**
   * The action waiting on its assets (`undo`, `select finish=chrome`,
   * `load /World`), by its label; `null` when none is. Actions asked for
   * meanwhile run after it, in order.
   */
  get running(): string | null {
    return this.#actions.running;
  }

  /** The scene browser's tab in front (prims, layers, cameras). */
  get sceneTab(): SceneTab {
    return this.#view.sceneTab;
  }

  /** The bottom panel's tab in front. */
  get bottomTab(): BottomTab {
    return this.#view.bottomTab;
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
  async open(source: SceneSource): Promise<void> {
    // A lost module opens nothing.
    if (this.status.kind === "lost") return;
    await this.#opener.open(source);
  }

  /**
   * Stops what the overlay shows. An open: its fetches are aborted and
   * what it made is freed, and the editor keeps the scene it had. A
   * payload load: its fetches are aborted, the payloads go back to how
   * they were loaded before it, and the layers it read are released.
   * Work already holding the thread finishes first.
   */
  cancel(): void {
    if (this.status.kind === "opening") this.#opener.cancel();
    else if (this.status.kind === "loading") this.#actions.cancelRunning();
  }

  /** Sets how the next scene opens its payloads. */
  setPayloads(choice: PayloadChoice): void {
    this.#opener.setPayloads(choice);
  }

  /** Where the next scene's edits go: its root, or a session layer ({@link EditLayerChoice}). */
  get editLayerChoice(): EditLayerChoice {
    return this.#opener.editLayer;
  }

  /** Sets where the next scene's edits go; a package root is edited in a session layer whatever is set. */
  setEditLayerChoice(choice: EditLayerChoice): void {
    this.#opener.setEditLayer(choice);
  }

  /** Takes the session an open made as the scene: the one it had is freed, and everything starts over on the new one. */
  #commit(session: Session, source: SceneSource, note: string | null): void {
    this.payloadNote = note;
    this.session?.free();
    // The cluster sets the viewport holds are the freed scene's.
    this.#drawing.forgetResident();
    this.renamed = null;
    this.session = session;
    this.source = source;
    this.#selection.clear();
    this.viewport?.setMarker(null);
    this.#view.reset(upAxis(session));
    this.#playback.reset(timeRange(session.stage));
    this.#read = { session, changes: session.changeLog.length };
    this.status = { kind: "ready" };
    this.#drawing.sync(false);
    // Another scene: everything a panel reads may be another thing.
    this.signals.bumpAll();
  }

  #setStatus(status: EditorStatus): void {
    // Lost is for good: nothing that was in flight says otherwise.
    if (this.status.kind === "lost") return;
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
    // Whatever was in flight stops where it waits, and calls into no module again.
    this.#opener.abort();
    void this.#actions.cancel();
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

  /**
   * Shows `path`'s value and opinions in the explain panel (and, drawing
   * the selection, an image it names), bringing that panel's tab forward
   * unless `reveal` is `false` (asked from another tab of the same panel,
   * which would be hidden by it).
   */
  explain(path: string | null, reveal = true): void {
    this.#selection.property = path;
    if (path !== null && reveal) this.#view.showBottomTab("explain");
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

  /** Brings `tab` of the scene browser (prims, layers, cameras) to the front. */
  showSceneTab(tab: SceneTab): void {
    this.#view.showSceneTab(tab);
  }

  /** Brings `tab` of the bottom panel to the front. */
  showBottomTab(tab: BottomTab): void {
    this.#view.showBottomTab(tab);
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

  /** Whether playing loops at the end of the range (the default) or stops there; remembered for this viewer. */
  get loop(): boolean {
    return this.#playback.loop;
  }

  /** Makes playing loop at the end of the range, or stop there, from now on, and remembers it for this viewer. */
  setLoop(loop: boolean): void {
    if (loop === this.#playback.loop) return;
    this.#playback.loop = loop;
    rememberLoop(loop);
    this.signals.bump("view");
  }

  /** Pauses and reads everything at `time` (clamped to the range): a click on a key, a typed time code. */
  seek(time: number): void {
    this.pause();
    this.setTime(time);
  }

  /**
   * Pauses and moves to the next key after the current time (`direction`
   * 1) or the previous one (-1), of the selected prim's attributes and of
   * the explained property; returns its time, or `null` (the time stays)
   * when there is none that way.
   */
  jumpToKey(direction: 1 | -1): number | null {
    const stage = this.session?.stage;
    const range = this.range;
    if (stage === undefined || range === null) return null;
    const target = nextKey(selectionKeys(stage, this.selection, this.property, range).all, this.time ?? range.start, direction);
    if (target !== null) this.seek(target);
    return target;
  }

  /** Plays the range from the current time at `timeCodesPerSecond`, then loops or stops at its end ({@link EditorStore.loop}). */
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
    this.#shadow = null;
    this.signals.bump("target");
  }

  /**
   * Applies `edits` as one undoable action. Returns why the stage refused
   * them (the notice's message), or `undefined` when it did not, or when
   * the action waits its turn behind one that is fetching: a refusal is
   * then a notice alone.
   */
  apply(label: string, edits: readonly Edit[]): string | undefined {
    return this.#edit(label, (session) => session.apply(label, edits));
  }

  /**
   * Authors `value` for the attribute at `path`: its default, or its
   * sample at `time`. Returns why the stage refused it, as
   * {@link EditorStore.apply} does.
   */
  setValue(path: string, value: ValueInput, time: number | null = null): string | undefined {
    return this.#edit(
      time === null ? `set ${path}` : `set ${path} at ${time}`,
      (session) => {
        this.#shadow = null;
        return session.setValue(path, value, time);
      },
      () => this.#shadowed(path, time),
    );
  }

  // Property operations, each one undoable action where edits go.

  /**
   * The last value edit that does not show ({@link Shadow}): the edit
   * target holds it, but a stronger opinion still gives the value, or the
   * stage does not compose the target. Read once the edit is made; `null`
   * after an edit that shows, an undo or redo, or a change of where edits
   * go. Heard on `history` and `target`.
   */
  get shadow(): Shadow | null {
    return this.#shadow;
  }

  /**
   * Clears the attribute at `path` where edits go: its default (a value or
   * a block) and, when the samples the stage reads are the target's, those
   * samples, so its value falls back to weaker opinions or the schema's
   * fallback. Returns why it was refused (the target holds none of them),
   * as {@link EditorStore.apply} does.
   */
  clearValue(path: string): string | undefined {
    return this.#edit(`clear ${path}`, (session) => {
      const attribute = session.stage.attribute(path);
      const samples = attribute === null ? [] : targetOpinion(session.stage, attribute, session.target).samples;
      return session.clearValue(path, samples);
    });
  }

  /** Deletes the attribute at `path`'s sample at `time` where edits go. Returns why it was refused (no key there). */
  deleteKey(path: string, time: number): string | undefined {
    return this.#edit(`delete key ${path} at ${time}`, (session) => session.clearSample(path, time));
  }

  /**
   * Authors the attribute at `path`'s value at `time` as a sample there,
   * where edits go: a key of the value as it is read now. Returns why it
   * was refused (no value to key, a `uniform` attribute).
   */
  key(path: string, time: number): string | undefined {
    return this.#edit(
      `key ${path} at ${time}`,
      (session) => {
        this.#shadow = null;
        const value = session.stage.attribute(path)?.get(time) ?? null;
        if (value === null) throw new Error(`${path} has no value at ${time} to key`);
        try {
          return session.apply(`key ${path} at ${time}`, [{ type: "setValue", path, value, time }]);
        } finally {
          releaseValue(value);
        }
      },
      () => this.#shadowed(path, time),
    );
  }

  /** Blocks the attribute at `path` where edits go (`UsdAttribute::Block`). Returns why it was refused. */
  blockValue(path: string): string | undefined {
    return this.#edit(
      `block ${path}`,
      (session) => {
        this.#shadow = null;
        return session.blockValue(path);
      },
      () => this.#shadowed(path, null),
    );
  }

  /** Removes the property at `path` from where edits go. Returns why it was refused (the target has no opinion of it). */
  removeProperty(path: string): string | undefined {
    return this.#edit(`remove ${path}`, (session) => session.removeProperty(path));
  }

  /**
   * Creates the attribute at `path` where edits go, of `typeName` and
   * `variability`, with `value` as its default when given, and makes it
   * the property explained, so the inspector marks it. Returns why it was
   * refused.
   */
  createAttribute(path: string, typeName: string, variability: "varying" | "uniform", value?: ValueInput): string | undefined {
    return this.#edit(
      `create ${path}`,
      (session) => session.createAttribute(path, typeName, variability, value),
      () => {
        this.#selection.property = path;
        return true;
      },
    );
  }

  /**
   * Authors `targets` as the relationship at `path`'s explicit targets
   * where edits go, creating a `custom` relationship when nothing declares
   * it, and makes it the property explained. Returns why it was refused.
   */
  setTargets(path: string, targets: readonly string[]): string | undefined {
    return this.#edit(
      `set targets of ${path}`,
      (session) => session.setTargets(path, targets),
      () => {
        if (this.#selection.property === path) return false;
        this.#selection.property = path;
        return true;
      },
    );
  }

  /** Reads whether the edit of `path` at `time` just made shows ({@link EditorStore.shadow}); the selection is unchanged. */
  #shadowed(path: string, time: number | null): boolean {
    const session = this.session;
    const attribute = session?.stage.attribute(path) ?? null;
    this.#shadow = session === null || attribute === null ? null : shadowOf(session.stage, attribute, session.target, time);
    return false;
  }

  /**
   * Defines a child of `parent` named `name`, of `typeName` (typeless with
   * `null`), and selects it. Returns why the stage refused it, as
   * {@link EditorStore.apply} does.
   */
  definePrim(parent: string, name: string, typeName: string | null): string | undefined {
    const path = parent === "/" ? `/${name}` : `${parent}/${name}`;
    const label = `define ${typeName ?? "typeless"} ${path}`;
    const edit = typeName === null ? { type: "definePrim" as const, path } : { type: "definePrim" as const, path, typeName };
    return this.#edit(
      label,
      (session) => {
        refuseNamespace(session, "define", path);
        return session.apply(label, [edit]);
      },
      () => this.#selection.select(path),
    );
  }

  /**
   * Renames or reparents the prim at `path` to `to`, repairing the paths
   * that name it; the selection follows when it is the prim or beneath
   * it. Returns why the stage refused it, as {@link EditorStore.apply}
   * does.
   */
  move(path: string, to: string): string | undefined {
    return this.#edit(
      `move ${path}`,
      (session) => {
        refuseNamespace(session, "rename", path);
        return session.move(path, to);
      },
      () => this.#follow({ from: path, to }),
    );
  }

  /**
   * Removes the prim at `path` from the edit target's layer stack; a
   * selection at or beneath it is cleared. Returns why the stage refused
   * it, as {@link EditorStore.apply} does.
   */
  delete(path: string): string | undefined {
    return this.#edit(`delete ${path}`, (session) => {
      refuseNamespace(session, "delete", path);
      return session.delete(path);
    });
  }

  // The shading network (`model/networkEdits.ts` says what each authors).

  /**
   * Connects the shading input or output at `destination` to read
   * `source` (`UsdShadeConnectableAPI::ConnectToSource`, replacing what
   * it read), creating either when only its shader's definition has it.
   * Returns why the stage refused it, as {@link EditorStore.apply} does.
   */
  connect(destination: string, source: string): string | undefined {
    const label = connectLabel(destination, source);
    return this.#edit(label, (session) => session.applyInTurn(label, connectEdits(session.stage, destination, source)));
  }

  /**
   * Stops `destination` reading `source`, or (`null`) every source it
   * reads (`DisconnectSource`). Returns why the stage refused it, as
   * {@link EditorStore.apply} does.
   */
  disconnect(destination: string, source: string | null): string | undefined {
    const label = disconnectLabel(destination, source);
    return this.#edit(label, (session) => session.apply(label, disconnectEdits(destination, source)));
  }

  /**
   * Authors `value` for the shading input at `path`, creating the input
   * with the value's type when the prim has none yet
   * (`UsdShadeConnectableAPI::CreateInput(...).Set`): its default, or its
   * sample at `time`. Returns why the stage refused it, as
   * {@link EditorStore.apply} does.
   */
  setShaderInput(path: string, value: ValueInput, time: number | null = null): string | undefined {
    const label = time === null ? `set ${path}` : `set ${path} at ${time}`;
    const edit: Edit = time === null ? { type: "setShaderInput", path, value } : { type: "setShaderInput", path, value, time };
    return this.#edit(label, (session) => session.apply(label, [edit]));
  }

  /**
   * Defines a `Shader` of the id `id` at `path`, with `position` as its
   * `ui:nodegraph:node:pos` when given, as one action. Returns why the
   * stage refused it, as {@link EditorStore.apply} does.
   */
  addShader(path: string, id: string, position: readonly [number, number] | null = null): string | undefined {
    const label = `add ${id} ${path}`;
    return this.#edit(label, (session) => session.apply(label, addShaderEdits(path, id, position)));
  }

  // The layer stack (`model/layerStack.ts`): each edit is to one layer's
  // `subLayers`, whatever layer other edits go to, one undoable action
  // that then runs the asset loop for what the stack names now.

  /**
   * Adds `asset` (as `parent` authors it: `./lib.usda`) to `parent`'s
   * sublayers at `index` (0 the strongest), read through `offset`, then
   * fetches it. Resolves why the stage refused it, as
   * {@link EditorStore.apply} returns it; a missing asset is not refused,
   * the Health tab reports it.
   */
  insertSublayer(parent: Layer, asset: string, index: number, offset: LayerOffset = IDENTITY): Promise<string | undefined> {
    const label = `add sublayer @${asset}@${offsetUsda(offset)} to ${layerLabel(parent)}`;
    return this.#fetchingEdit(label, (session, signal) =>
      session.editSublayers(label, parent, [{ type: "insertSublayer", asset, index, layerOffset: { offset: offset.offset, scale: offset.scale } }], signal),
    );
  }

  /** Removes `parent`'s sublayer at `index`, with its offset. Resolves why the stage refused it. */
  removeSublayer(parent: Layer, index: number): Promise<string | undefined> {
    const asset = this.session?.stage.sublayers(parent)[index]?.asset ?? `#${index}`;
    const label = `remove sublayer @${asset}@ from ${layerLabel(parent)}`;
    return this.#fetchingEdit(label, (session, signal) => session.editSublayers(label, parent, [{ type: "removeSublayer", index }], signal));
  }

  /**
   * Moves `parent`'s sublayer at `index` by `by` places (negative:
   * stronger), authoring its sublayers reordered (`setSublayers`).
   * Resolves why the stage refused it, or why it cannot move that far.
   */
  moveSublayer(parent: Layer, index: number, by: number): Promise<string | undefined> {
    const sublayers = this.session?.stage.sublayers(parent) ?? [];
    const asset = sublayers[index]?.asset ?? `#${index}`;
    const label = `make @${asset}@ ${by < 0 ? "stronger" : "weaker"} in ${layerLabel(parent)}`;
    return this.#fetchingEdit(label, async (session, signal) => {
      const moved = movedSublayers(session.stage.sublayers(parent), index, by);
      if (moved === null) throw new Error(`@${asset}@ is the ${by < 0 ? "strongest" : "weakest"} sublayer of ${layerLabel(parent)} already`);
      return session.editSublayers(label, parent, [{ type: "setSublayers", sublayers: moved.map(({ asset, layerOffset }) => ({ asset, layerOffset: { ...layerOffset } })) }], signal);
    });
  }

  /** Authors `offset` for `parent`'s sublayer at `index` (`SdfLayer::SetSubLayerOffset`). Resolves why the stage refused it. */
  setSublayerOffset(parent: Layer, index: number, offset: LayerOffset): Promise<string | undefined> {
    const asset = this.session?.stage.sublayers(parent)[index]?.asset ?? `#${index}`;
    const label = `set the offset of @${asset}@ in ${layerLabel(parent)}`;
    return this.#fetchingEdit(label, (session, signal) =>
      session.editSublayers(label, parent, [{ type: "setSublayerOffset", index, layerOffset: { offset: offset.offset, scale: offset.scale } }], signal),
    );
  }

  /**
   * Reads `layer` again from the source, or (`null`) every layer the
   * stage holds that has no unsaved edits and is not muted
   * ({@link Session.reload}): files changed on disk come in, and only what
   * changed recomposes. A layer with unsaved edits is not read again: one
   * asked for alone is refused, and among all it is left out and a notice
   * says so. Not an edit: nothing goes on the undo stack.
   */
  async reload(layer: Layer | null): Promise<ReloadReport> {
    const label = layer === null ? "reload from source" : `reload ${layerLabel(layer)}`;
    let report: ReloadReport = { reloaded: [], skipped: [], refusal: null };
    const refusal = await this.#fetchingEdit(label, async (session, signal) => {
      const stage = session.stage;
      const asked = layer === null ? stage.layers().filter((each) => !each.anonymous && !stage.isMuted(each)) : [layer];
      const skipped = asked.filter((each) => session.isEdited(each));
      if (layer !== null && skipped.length > 0) throw new Error(`${layerLabel(layer)} has unsaved edits: save or undo them before reading it again from the source`);
      const reading = asked.filter((each) => !skipped.includes(each));
      const result = reading.length === 0 ? null : await session.reload(reading, signal);
      report = { reloaded: result?.reloaded ?? [], skipped, refusal: null };
      if (skipped.length > 0) {
        this.#notices.add(label, `not read again, having unsaved edits: ${skipped.map(layerLabel).join(", ")}`, "io.dirty_reload");
      }
      for (const failure of result?.failures ?? []) this.#notices.add(label, `${failure.assetPath} was not read again: ${failure.message}`, failure.code);
    });
    return refusal === undefined ? report : { ...report, refusal };
  }

  // The stage's metadata (`model/stageMetadata.ts`), read from the root
  // and session layers alone: authored in one of them.

  /** Which layer each stage metadata field the Stage tab shows is read from; read again after a change, not per call. */
  stageMetadataSources(): ReadonlyMap<string, FieldSource> {
    return this.session === null ? new Map() : this.#reports.metadataSources(this.session);
  }

  /**
   * The layer a stage metadata edit goes to ({@link metadataLayer}): where
   * edits go when that is the root or the session layer, else the root,
   * with a note saying why. `null` with no scene.
   */
  metadataPlacement(): MetadataPlacement<Layer> | null {
    const session = this.session;
    if (session === null) return null;
    const stage = session.stage;
    const target = session.target;
    const layer = target === undefined ? stage.editLayer : "kind" in target ? (stage.hasLocalLayer(target.layer) ? target.layer : null) : target;
    return metadataLayer(layer, stage.root, session.sessionLayer, layerLabel);
  }

  /**
   * Authors the stage metadata field `key` (`upAxis`, `metersPerUnit`,
   * `startTimeCode`, ...) as `value` where {@link EditorStore.metadataPlacement}
   * says. The viewport follows `upAxis`, the timeline the range and rate.
   * Returns why the stage refused it.
   */
  setStageMetadata(key: string, value: ValueInput): string | undefined {
    const layer = this.metadataPlacement()?.layer;
    if (layer === undefined) return undefined;
    const label = `set ${key} in ${layerLabel(layer)}`;
    return this.#edit(label, (session) => session.applyIn(label, layer, [{ type: "setLayerMetadata", key, value }]));
  }

  /**
   * Clears the stage metadata field `key` from `layer` (the root or the
   * session layer, whichever holds the opinion), so the stage reads the
   * other's, or the field's fallback. Returns why it was refused (the
   * layer authors none).
   */
  clearStageMetadata(key: string, layer: Layer): string | undefined {
    const label = `clear ${key} in ${layerLabel(layer)}`;
    return this.#edit(label, (session) => session.clearLayerMetadata(label, layer, key));
  }

  /**
   * The stage's health ({@link HealthReport}): its composition errors by
   * prim, the assets missing or not supplied, and the paths the last move
   * or delete left unrepaired. Read once per change of the stage (or of
   * the undo stack), however often it is asked; `null` with no scene.
   * Heard on `composition` and `history`.
   */
  get health(): HealthReport<Layer> | null {
    const session = this.session;
    if (session === null || this.status.kind === "lost") return null;
    try {
      return this.#reports.health(session);
    } catch (error) {
      if (!this.lose(error)) throw error;
      return null;
    }
  }

  /**
   * Hides instance `id` of the point instancer at `instancer` (adds it to
   * `invisibleIds`, at the current time when that attribute is animated,
   * `UsdGeomPointInstancer::InvisibleIds`) or shows it again, as one edit
   * where edits go. Nothing when it already is.
   */
  setInstanceVisible(instancer: string, id: bigint, visible: boolean): string | undefined {
    const stage = this.session?.stage;
    const columns = this.instancerColumns(instancer);
    if (stage === undefined || columns === null || columns.invisibleIds.includes(id) !== visible) return undefined;
    const attribute = stage.attribute(`${instancer}.invisibleIds`);
    const at = attribute?.mightBeTimeVarying() === true ? (this.time ?? null) : null;
    const value = { type: "int64[]", value: BigInt64Array.from(toggled(columns.invisibleIds, id)) } as const;
    return this.apply(`${visible ? "show" : "hide"} instance ${id} of ${instancer}`, [
      { type: "setValue", path: `${instancer}.invisibleIds`, value, ...(at === null ? {} : { time: at }) },
    ]);
  }

  /**
   * Deactivates instance `id` of the point instancer at `instancer` (adds
   * it to the `inactiveIds` list op metadata, `UsdGeomPointInstancer::
   * DeactivateId`) or activates it again, as one edit where edits go.
   * Nothing when it already is.
   */
  setInstanceActive(instancer: string, id: bigint, active: boolean): string | undefined {
    const columns = this.instancerColumns(instancer);
    if (columns === null || columns.inactiveIds.includes(id) !== active) return undefined;
    const value = { type: "int64[]", value: BigInt64Array.from(toggled(columns.inactiveIds, id)) } as const;
    return this.apply(`${active ? "activate" : "deactivate"} instance ${id} of ${instancer}`, [{ type: "setMetadata", path: instancer, key: "inactiveIds", value }]);
  }

  /** Selects `variant` of `set` on `prim`, then fetches what it needs. */
  selectVariant(prim: string, set: string, variant: string): Promise<void> {
    return this.#fetching(`select ${set}=${variant}`, async (session, signal) => {
      await session.selectVariant(prim, set, variant, signal);
      return false;
    });
  }

  // Composition: arcs and variant sets (`model/composition.ts` says what each authors).

  /**
   * The last composition edit made here (an arc or a variant set added or
   * removed) while it is the history's newest step: what the inspector
   * offers to show in its layer's text. `null` once another step is.
   * Heard on `history`.
   */
  get compositionEdit(): CompositionEdit | null {
    const edit = this.#compositionEdit;
    return edit !== null && this.session?.history.nextUndo === edit.step ? edit : null;
  }

  /**
   * Adds `arc` to the prim at `prim` where edits go, then fetches what it
   * names that the stage has not read, with the opening overlay showing
   * the asset loop's rounds; one undoable action. Cancelling the fetch
   * takes the arc back. What it names that is missing is a notice, and
   * the inspector says so beside the arc. The refusal is known at once
   * when the action runs now ({@link Authoring}).
   */
  addArc(prim: string, arc: NewArc): Authoring {
    const label = newArcLabel(prim, arc);
    let refusal: string | undefined;
    const done = this.#actions.run(label, async (signal) => {
      const session = this.session;
      if (session === null || this.status.kind === "lost") return;
      try {
        let step: HistoryStep | null;
        try {
          step = this.#composing(session, prim, () => session.addArc(prim, arc));
        } catch (error) {
          if (this.lose(error)) return;
          refusal = this.#notices.report(label, error).message;
          this.#settled(session, false, "history");
          return;
        }
        if (!(await this.#supplyShown(session, label, `Adding the ${arc.kind}: fetching what it names`, step, signal))) return;
        for (const issue of session.stage.status().compositionErrors) {
          if (issue.prim === prim && issue.code === "composition.unresolved_asset") this.#notices.add(label, issue.message, issue.code);
        }
        this.#settled(session, false, "history", "loop", "status");
      } catch (error) {
        if (!this.lose(error)) throw error;
      }
    });
    return { refusal, done };
  }

  /**
   * Removes `arc`, which the edit target's layer authors on the prim at
   * `prim` (`model/composition.ts`, `findAuthoring`), as one undoable
   * action. Returns why it was refused.
   */
  removeArc(prim: string, arc: AuthoredArc): string | undefined {
    const label = removeArcLabel(prim, arc);
    return this.#edit(label, (session) => this.#composed(session, prim, () => session.removeArc(prim, arc)));
  }

  /**
   * Adds the variant `variant` to the set `set` of the prim at `prim`
   * where edits go, the set with it when it is new (`newSet`), and selects
   * it, as one undoable action. Returns why it was refused.
   */
  addVariant(prim: string, set: string, variant: string, newSet = false): string | undefined {
    const label = newSet ? `add variant set ${set}` : `add variant ${set}=${variant}`;
    return this.#edit(label, (session) => this.#composed(session, prim, () => session.addVariant(prim, set, variant, newSet), { set, variant }));
  }

  /**
   * Removes the variant set `set` of the prim at `prim` from the edit
   * target's layer: its variants, what they author, and the layer's
   * selection; its name stays in `variantSetNames`, as OpenUSD leaves it.
   * Returns why it was refused (the layer holds no such set).
   */
  removeVariantSet(prim: string, set: string): string | undefined {
    const label = `remove variant set ${set}`;
    return this.#edit(label, (session) => this.#composed(session, prim, () => session.removeVariantSet(prim, set)));
  }

  /**
   * Makes edits go inside the variant `variant` of the set `set` of the
   * prim at `prim` (`Prim.variantEditTarget`), in the layer edits go to
   * now (the edit layer when they go through an arc).
   */
  editInsideVariant(prim: string, set: string, variant: string): void {
    const session = this.session;
    const found = session?.stage.prim(prim) ?? null;
    if (session === null || found === null) return;
    const target = session.target;
    const layer = target === undefined ? session.stage.editLayer : "kind" in target ? (target.kind === "layer" ? target.layer : session.stage.editLayer) : target;
    try {
      this.setTarget(found.variantEditTarget(set, variant, layer));
    } catch (error) {
      if (!this.lose(error)) this.#notices.report(`edit inside ${set}=${variant}`, error);
    }
  }

  /**
   * What `layer` authors about composition (`model/layerSpecs.ts`), read
   * from its text once per change of the stage; `null` when its text is
   * too large to read whole, or cannot be written.
   */
  layerSpecs(layer: Layer): LayerSpecs | null {
    const session = this.session;
    if (session === null) return null;
    const stamp = `${this.signals.revision("history")}:${this.signals.revision("composition")}`;
    if (this.#specs.session !== session || this.#specs.stamp !== stamp) this.#specs = { session, stamp, read: new Map() };
    const cached = this.#specs.read.get(layer);
    if (cached !== undefined) return cached;
    const text = this.layerText(layer);
    const specs = typeof text === "string" || text.truncated ? null : readLayerSpecs(text.lines.join("\n"));
    this.#specs.read.set(layer, specs);
    return specs;
  }

  /** The layers an arc can be pointed at: those the source lists and those the stage holds, by identifier, sorted. */
  async arcLayers(): Promise<string[]> {
    const session = this.session;
    if (session === null) return [];
    const held = session.stage.layers().flatMap((layer) => (layer.identifier === null || layer.identifier.includes("[") ? [] : [layer.identifier]));
    const listed = (await this.source?.layers?.().catch(() => [])) ?? [];
    return [...new Set([...listed, ...held])].sort();
  }

  /**
   * The default prim and root prims of the layer `identifier`: from the
   * stage when it holds it, else read from the source (a layer over
   * {@link SUMMARY_BYTES} is not, and the summary says why).
   */
  async layerSummary(identifier: string): Promise<LayerSummary> {
    const session = this.session;
    const source = this.source;
    if (session === null || source === null) return { kind: "unread", reason: "Nothing is open." };
    const held = session.stage.layers().find((layer) => layer.identifier === identifier);
    if (held !== undefined) {
      const specs = this.layerSpecs(held);
      return specs === null ? { kind: "unread", reason: `${identifier} is too large to read for its prims here.` } : summaryOf(specs);
    }
    try {
      const size = await source.size(identifier);
      if (size !== null && size > SUMMARY_BYTES) return { kind: "unread", reason: `${identifier} is ${(size / 1024 ** 2).toFixed(1)} MB: too large to read for its prims alone.` };
      const bytes = await source.fetch(identifier);
      if (bytes === null) return { kind: "unread", reason: `The scene's source has no ${identifier}.` };
      if (bytes.byteLength > SUMMARY_BYTES) return { kind: "unread", reason: `${identifier} is too large to read for its prims alone.` };
      return summarizeBytes(this.layerstack, identifier, bytes);
    } catch (error) {
      if (this.lose(error)) return { kind: "unread", reason: "The module was lost." };
      return { kind: "unread", reason: error instanceof Error ? error.message : String(error) };
    }
  }

  /** Runs a composition edit of `prim` and notes it ({@link EditorStore.compositionEdit}); returns its receipt. */
  #composed(session: Session, prim: string, run: () => EditReceipt, variant?: CompositionEdit["variant"]): EditReceipt {
    let receipt: EditReceipt | null = null;
    this.#composing(session, prim, () => (receipt = run()), variant);
    return receipt!;
  }

  /**
   * Runs a composition edit of `prim`, its layer's text read first when
   * the Text tab never read it (so the tab can mark what the edit
   * changed), and notes it; returns the step it made.
   */
  #composing(session: Session, prim: string, run: () => EditReceipt, variant?: CompositionEdit["variant"]): HistoryStep | null {
    const target = session.target;
    const layer = target === undefined ? session.stage.editLayer : "kind" in target ? target.layer : target;
    if (layer !== null && this.lastLayerText(layer) === null) this.layerText(layer);
    const receipt = run();
    const step = session.history.nextUndo;
    this.#compositionEdit = step === null ? null : { prim, label: step.label, layer: receipt.target.layer, step, ...(variant === undefined ? {} : { variant }) };
    return step;
  }

  /**
   * Fetches what the stage wants after the action `action` (whose step is
   * `step`), with the opening overlay up while it does (`title` says what
   * for) when there is anything to fetch. Cancelled, the action is taken
   * back: its step undone, what was read for it released. Returns whether
   * the fetching ran to its end (a failure is a notice).
   */
  async #supplyShown(session: Session, action: string, title: string, step: HistoryStep | null, signal: AbortSignal): Promise<boolean> {
    const wants = session.stage.unavailable().length > 0;
    if (wants) {
      this.#opener.begin();
      this.#setStatus({ kind: "loading", label: action, title });
    }
    let finished = true;
    try {
      await session.supplyUnavailable(action, undefined, signal);
    } catch (error) {
      if (this.lose(error)) return false;
      finished = false;
      if (!signal.aborted) this.#notices.report(action, error);
    }
    if (signal.aborted && step !== null && session.history.nextUndo === step) {
      // Cancelled: nothing of the action stays, and what was read for it is let go.
      await session.undo();
      session.releaseUnusedLayers();
    }
    if (wants) {
      this.#opener.end();
      if ((this.status as EditorStatus).kind === "loading") this.status = { kind: "ready" };
    }
    if (!finished) this.#settled(session, false, "history", "loop", "status");
    return finished;
  }

  /** Undoes the newest action, then fetches what that brings back; the selection follows a rename undone. */
  undo(): Promise<void> {
    return this.#fetching("undo", async (session, signal) => {
      this.#shadow = null;
      return this.#follow(await session.undo(signal));
    });
  }

  /** Redoes the most recently undone action, then fetches what it needs; the selection follows a rename redone. */
  redo(): Promise<void> {
    return this.#fetching("redo", async (session, signal) => {
      this.#shadow = null;
      return this.#follow(await session.redo(signal));
    });
  }

  /** Mutes or unmutes a layer, by identifier for a muted layer the stage released. */
  setMuted(layer: Layer | string, muted: boolean): Promise<void> {
    return this.#fetching(muted ? "mute" : "unmute", async (session, signal) => {
      await session.setMuted(layer, muted, signal);
      return false;
    });
  }

  /**
   * Loads the payloads at `path` (and beneath it, with `"withDescendants"`)
   * and fetches what they want, the prim tree showing them loaded once
   * they are; the overlay shows the rounds as an open's, and
   * {@link EditorStore.cancel} stops it.
   */
  load(path: string, policy: LoadPolicy = "withDescendants"): Promise<void> {
    return this.#loading(`load ${path}`, path, (session, signal) => session.loadPayloads([path], [], policy, undefined, signal));
  }

  /**
   * Unloads the payloads at and beneath `path`: their prims leave the
   * stage and the draws, and the layers nothing uses any more are
   * released, so their memory comes back; loading them again fetches them.
   */
  unload(path: string): Promise<void> {
    return this.#loading(`unload ${path}`, path, async (session, signal) => {
      const outcome = await session.loadPayloads([], [path], "withDescendants", undefined, signal);
      session.releaseUnusedLayers();
      return outcome;
    });
  }

  /** Runs a payload load or unload in its turn with the overlay up, then brings the draws up to it. */
  #loading(action: string, label: string, run: (session: Session, signal: AbortSignal) => Promise<LoadOutcome>): Promise<void> {
    return this.#actions.run(action, async (signal) => {
      const session = this.session;
      // Behind an open's overlay, or with no scene, there is nothing to load into.
      if (session === null || this.status.kind !== "ready") return;
      const rules = session.stage.loadRules();
      this.#opener.begin();
      this.#setStatus({ kind: "loading", label });
      try {
        // A path the stage refuses (an inactive prim) is skipped, as in
        // OpenUSD; the notice says which and why.
        for (const { path, code } of (await run(session, signal)).refused) {
          this.#notices.add(action, `${path} was not ${action.startsWith("load") ? "loaded" : "unloaded"}: ${code.replaceAll("_", " ")}`, `load.${code}`);
        }
      } catch (error) {
        if (this.lose(error)) return;
        if (!signal.aborted) this.#notices.report(action, error);
      }
      try {
        if (signal.aborted) {
          // Cancelled: the payloads are loaded as they were, and what was read for them is let go.
          session.stage.setLoadRules(rules);
          session.releaseUnusedLayers();
        }
        this.#opener.end();
        // An open that began meanwhile says where the editor is.
        if ((this.status as EditorStatus).kind === "loading") this.status = { kind: "ready" };
        this.#settled(session, false, "status", "loop");
      } catch (error) {
        if (!this.lose(error)) throw error;
      }
    });
  }

  // What leaves the page: saving, exporting, and a layer as text.

  /** The layers with unsaved edits, the session layer's among them; empty with no scene. */
  dirtyLayers(): Layer[] {
    return this.session?.dirtyLayers() ?? [];
  }

  /** Whether `layer` has unsaved edits ({@link Session.isEdited}): what the Layers tab marks "edited". */
  isEdited(layer: Layer): boolean {
    return this.session?.isEdited(layer) ?? false;
  }

  /**
   * Whether anything would be lost by leaving the page: a layer has edits
   * not saved (or, for the session layer, not exported). What the page's
   * `beforeunload` asks.
   */
  get unsaved(): boolean {
    if (this.status.kind === "lost") return false;
    return this.dirtyLayers().length > 0;
  }

  /** The dirty layers and what a save does with each ({@link planSave}), the written ones first. */
  savePlan(): SaveEntry[] {
    const session = this.session;
    return session === null ? [] : planSave(session.stage, session.dirtyLayers(), session.sessionLayer);
  }

  /**
   * Saves the dirty layers (`Stage.save`) in its turn: what it returns is
   * written (the bytes are the caller's to download or write) and those
   * layers are clean; the session layer and muted layers stay dirty. A
   * layer that cannot be written is a notice too. `null` with no scene, or
   * when the save threw (a notice says why).
   */
  async save(): Promise<SaveResult | null> {
    let result: SaveResult | null = null;
    await this.#actions.run("save", () => {
      const session = this.session;
      if (session === null || this.status.kind === "lost") return;
      try {
        result = session.save();
        for (const failure of result.failures) {
          this.#notices.add("save", `${failure.layer.identifier ?? "the session layer"} was not saved: ${failure.message}`, failure.code);
        }
      } catch (error) {
        if (!this.lose(error)) this.#notices.report("save", error);
      }
      this.signals.bump("history");
    });
    return result;
  }

  /**
   * `layer` as `format` (`Stage.exportLayer`): a copy, the layer's state
   * unchanged. `null` when the stage could not write it, with a notice
   * saying why.
   */
  exportLayer(layer: Layer, format: LayerFormat): Uint8Array | null {
    return this.#exporting(`export ${layer.identifier ?? "the session layer"}`, (session) => session.stage.exportLayer(layer, format));
  }

  /**
   * The composed stage as one layer with no arcs (`Stage.exportFlattened`),
   * with whether it is lossless and what was lost. `null` when the stage
   * could not write it, with a notice saying why.
   */
  exportFlattened(format: LayerFormat): FlattenResult | null {
    return this.#exporting("export flattened", (session) => session.stage.exportFlattened(format));
  }

  /**
   * The root and what it depends on as a USDZ package, in its turn: the
   * assets the stage was never asked for are fetched from the source for
   * it ({@link Session.exportPackage}). Its `bytes` are `null` while a
   * dependency is unresolved, and `unresolved` says which; the session
   * layer is `omitted`. `null` with no scene, or when it failed (a notice
   * says why).
   */
  async exportPackage(): Promise<PackageResult | null> {
    let result: PackageResult | null = null;
    await this.#actions.run("package", async (signal) => {
      const session = this.session;
      const source = this.source;
      if (session === null || source === null || this.status.kind === "lost") return;
      try {
        result = (await session.exportPackage((identifier) => source.exists(identifier), signal)).result;
      } catch (error) {
        if (this.lose(error)) return;
        if (!signal.aborted) this.#notices.report("export package", error);
      }
      this.signals.bump("loop");
    });
    return result;
  }

  /** The layer the Text tab shows: the one chosen ({@link EditorStore.showLayerText}) while the stage holds it, else the edit layer. */
  get textLayer(): Layer | null {
    return this.#files.shown(this.session);
  }

  /** Shows `layer` as text in the Text tab, bringing the tab forward. */
  showLayerText(layer: Layer): void {
    const session = this.session;
    if (session === null) return;
    this.#files.show(session, layer);
    this.#view.showBottomTab("text");
  }

  /**
   * `layer` as USDA text, with the lines that changed since it was last
   * read here ({@link LayerTextRead.changes}): read after an edit, the
   * lines the edit authored. The text says why when the layer cannot be
   * written as text.
   */
  layerText(layer: Layer): LayerTextRead | string {
    const session = this.session;
    if (session === null) return "Nothing is open.";
    try {
      return this.#files.texts.read(session.stage, layer);
    } catch (error) {
      if (this.lose(error)) return "The module was lost.";
      return error instanceof Error ? error.message : String(error);
    }
  }

  /** The last text read of `layer` ({@link EditorStore.layerText}) without reading it again; `null` when none was. */
  lastLayerText(layer: Layer): LayerTextRead | null {
    const session = this.session;
    return session === null ? null : this.#files.texts.last(session.stage, layer);
  }

  /** Runs an export that only reads the stage; its failure is a notice, a module's trap loses the module. */
  #exporting<T>(action: string, run: (session: Session) => T): T | null {
    const session = this.session;
    if (session === null || this.status.kind === "lost") return null;
    try {
      return run(session);
    } catch (error) {
      if (!this.lose(error)) this.#notices.report(action, error);
      return null;
    }
  }

  /** Forgets a notice. */
  dismiss(id: number): void {
    this.#notices.dismiss(id);
  }

  /**
   * Runs an edit of the stage in its turn: now, when no action is
   * waiting on its assets, else after those asked for before it. `then`
   * runs after an edit the stage took, and returns whether it changed the
   * selection. A refusal is a notice, and what this returns when the
   * edit ran now; a module's trap loses the module.
   */
  #edit(action: string, run: (session: Session) => EditReceipt, then?: () => boolean): string | undefined {
    let refusal: string | undefined;
    void this.#actions.run(action, () => {
      const session = this.session;
      if (session === null || this.status.kind === "lost") return;
      try {
        let selected = false;
        try {
          run(session);
          selected = then?.() ?? false;
        } catch (error) {
          if (this.lose(error)) return;
          refusal = this.#notices.report(action, error).message;
        }
        this.#settled(session, selected, "history");
      } catch (error) {
        if (!this.lose(error)) throw error;
      }
    });
    return refusal;
  }

  /**
   * Runs an action that may fetch what the stage then wants, in its turn;
   * resolves once it has run. `run` returns whether it changed the
   * selection. A failure is a notice, a module's trap loses the module,
   * and an action aborted (the scene is going) says nothing.
   */
  #fetching(action: string, run: (session: Session, signal: AbortSignal) => Promise<boolean>): Promise<void> {
    return this.#actions.run(action, async (signal) => {
      const session = this.session;
      if (session === null || this.status.kind === "lost") return;
      try {
        let selected = false;
        try {
          selected = await run(session, signal);
        } catch (error) {
          if (this.lose(error)) return;
          if (!signal.aborted) this.#notices.report(action, error);
        }
        this.#settled(session, selected, "history", "loop");
      } catch (error) {
        if (!this.lose(error)) throw error;
      }
    });
  }

  /**
   * Runs an edit that may fetch what the stage then wants, in its turn
   * ({@link EditorStore.#fetching}), and resolves why the stage refused it
   * (or the fetch failed), as {@link EditorStore.apply} returns it; the
   * refusal is a notice too.
   */
  async #fetchingEdit(action: string, run: (session: Session, signal: AbortSignal) => Promise<unknown>): Promise<string | undefined> {
    let refusal: string | undefined;
    await this.#fetching(action, async (session, signal) => {
      try {
        await run(session, signal);
      } catch (error) {
        // A trap loses the module, and an abort says nothing: both are the queue's to handle.
        if (signal.aborted || moduleTrap(error) !== null) throw error;
        refusal = this.#notices.report(action, error).message;
      }
      return false;
    });
    return refusal;
  }

  /**
   * After an action on `session`: the selection lets go of a prim that is
   * no longer there, and the draws and the panels are brought up to the
   * stage (`selected`: the action changed the selection). Nothing when
   * the scene is another one by now.
   */
  #settled(session: Session, selected: boolean, ...also: readonly Channel[]): void {
    if (this.session !== session || this.status.kind === "lost") return;
    // What was read of the stage during the action (a panel rendering as its assets came in) may be of a stage half composed.
    this.#reports.forget();
    const prim = this.selection;
    if (prim !== null && session.stage.prim(prim) === null) {
      this.#selection.select(null);
      selected = true;
    }
    // An instance let go with its instancer takes its outline with it.
    if (selected && this.instance === null) this.viewport?.setMarker(null);
    this.#sync(...also, ...(selected ? (["selection"] as const) : []));
  }

  /**
   * The selection follows `move`, a prim's (`null`: nothing moved), and
   * {@link EditorStore.renamed} says so. Returns whether the selection
   * changed.
   */
  #follow(move: PathMove | null): boolean {
    if (move === null) return false;
    this.renamed = { ...move };
    return this.#selection.follow(move.from, move.to);
  }

  // Keeping the draws and the panels up to date.

  /**
   * Brings the draws up to the stage and tells the panels: `draws`,
   * `composition` when the stage reported a change since the last sync,
   * and `also`.
   */
  #sync(...also: readonly Channel[]): void {
    const moved = this.#readStage();
    const changed = this.#drawing.sync(false);
    this.#outline();
    this.signals.bump(...also, ...changed, ...(moved ? (["time"] as const) : []));
  }

  /**
   * Reads what the store keeps of the stage's metadata, when the stage
   * reported a change since it last did: its time range and its up axis
   * are an edit, or an undo, away from what they were at open. Returns
   * whether the range or the time changed.
   */
  #readStage(): boolean {
    const session = this.session;
    if (session === null || (this.#read.session === session && this.#read.changes === session.changeLog.length)) return false;
    this.#read = { session, changes: session.changeLog.length };
    const up = upAxis(session);
    if (up !== this.upAxis) this.#view.setUpAxis(up);
    return this.#playback.setRange(timeRange(session.stage));
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

/** What {@link EditorStore.reload} did. */
export interface ReloadReport {
  /** The layers read again: those whose source changed. */
  readonly reloaded: readonly Layer[];
  /** The layers left out for their unsaved edits. */
  readonly skipped: readonly Layer[];
  /** Why the reload was refused, or failed; `null` when it ran. */
  readonly refusal: string | null;
}

/** A layer as an action's label names it: its identifier, or the session layer. */
function layerLabel(layer: Layer): string {
  return layer.identifier ?? "the session layer";
}

/** Throws why `action` on `path` cannot go where edits go ({@link namespaceRefusal}), before it is tried. */
function refuseNamespace(session: Session, action: NamespaceAction, path: string): void {
  const why = namespaceRefusal(session.target, action, path);
  if (why !== null) throw new Error(why);
}

/** The stage's `upAxis`: `Z` when it says so, else `Y`. */
function upAxis(session: Session): UpAxis {
  const up = session.stage.metadata("upAxis");
  return up?.kind === "scalar" && up.value === "Z" ? "Z" : "Y";
}
