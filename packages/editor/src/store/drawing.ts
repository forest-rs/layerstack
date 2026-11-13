// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The draws: the stagehand draw list kept up to the stage, what it holds
// and costs, and the three.js view it is applied to. The store says what
// to draw (the session, the time, the scope, the budget, the selection);
// this brings the list to it, hands the view what changed, and says which
// of the panels' concerns did.
import type { BudgetReport, BufferCost, DrawCost, DrawScope, Stagehand } from "@forest-rs/layerstack_stagehand";
import { type AssetFocus, assetFocus, type BudgetChoice, budgetFor, type DrawScopeChoice, scopeFor } from "../model/assetView.ts";
import { ViewportModel, type ViewportScene, type ViewportUpdate } from "../model/draws.ts";
import type { Session } from "../model/session.ts";
import type { Viewport } from "../viewport/viewport3d.ts";
import type { Channel } from "./signals.ts";
import { moduleTrap } from "./status.ts";

/** What is to be drawn: what {@link Drawing.sync} brings the draws to. */
export interface DrawRequest {
  readonly session: Session | null;
  readonly time: number | undefined;
  readonly drawScope: DrawScopeChoice;
  readonly budgetChoice: BudgetChoice;
  /** The selected prim (admitted first under a budget, drawn alone in the asset view), and the explained property (an image it names is shown). */
  readonly selection: string | null;
  readonly property: string | null;
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

/** What {@link Drawing} hears from the viewport and passes on. */
export interface DrawingHost {
  /** What is to be drawn, now. */
  request(): DrawRequest;
  /** The viewport reported something that changes the draws (the cluster sets resident): they are to be synced. */
  resync(): void;
  /** `channels` changed outside a sync; with `soon`, tell the panels once a frame at most. */
  changed(channels: readonly Channel[], soon: boolean): void;
}

/** The draw list, its measurements, and the view it is drawn in. */
export class Drawing {
  /** The draws, as last synced. */
  scene: ViewportScene = EMPTY_SCENE;
  /** The last sync that changed something, for the stats. */
  lastUpdate: ViewportUpdate | null = null;
  /** What the draws hold, measured after the last sync; `null` with no scene. */
  cost: DrawCost | null = null;
  /** Where the draws stand against the budget, after the last sync; `null` without a budget or a scene. */
  budget: BudgetReport | null = null;
  /** What the asset view makes of the selection, as last synced. */
  focus: AssetFocus = { kind: "none" };
  /** The three.js view, once attached. */
  viewport: Viewport | null = null;
  readonly #model: ViewportModel;
  readonly #host: DrawingHost;
  /** The costs of meshes the viewport draws from prebuilt cluster sets, by prim (`Viewport.onResident`); `null` before any. */
  #resident: ReadonlyMap<string, BufferCost> | null = null;
  /** The whole scene's estimate, once asked for, and the session and change it was made at. */
  #estimate: { session: Session; changes: number; time: number | undefined; cost: DrawCost | null } | null = null;
  /** The session the last sync drew and the change records it had seen. */
  #synced: { session: Session | null; changes: number } = { session: null, changes: 0 };

  constructor(stagehand: Stagehand, host: DrawingHost) {
    this.#model = new ViewportModel(stagehand);
    this.#host = host;
  }

  /**
   * Takes the three.js view (or `null` when it goes) and keeps it up to
   * date from now on: its uploads release the list's arrays, its resident
   * cluster sets weigh on the budget, and a restored context is filled
   * again. Returns the channels that changed.
   */
  attach(viewport: Viewport | null): Channel[] {
    this.viewport = viewport;
    if (viewport === null) return ["view"];
    // Uploaded buffers' arrays are dropped; what is held is measured again.
    viewport.onUploaded = (key, revision) => this.#model.release(key, revision);
    // Meshes drawn from prebuilt cluster sets cost the budget their sets, once.
    viewport.onResident = (costs) => {
      this.#resident = costs;
      this.#host.resync();
    };
    viewport.onReleased = () => {
      this.cost = this.#model.cost();
      this.#host.changed(["draws"], true);
    };
    // A lost context lost every upload: the released buffers filled again, the view made again.
    viewport.onContextRestored = () => {
      const { time } = this.#host.request();
      this.#model.restore();
      this.scene = this.#model.scene(time);
      this.cost = this.#model.cost();
      viewport.update(this.scene, { ...this.noUpdate(), reset: true }, this.highlighted());
      this.#host.changed(["draws"], false);
    };
    return ["view", ...this.sync(false, true)];
  }

  /** Forgets the resident cluster sets' costs: they are another scene's, once that scene is the one drawn. */
  forgetResident(): void {
    this.#resident = null;
  }

  /**
   * Brings the draw list up to what is to be drawn and applies what
   * changed to the view. Returns the channels that changed: `draws`
   * (with `timeOnly`, only when the time's move did more than place draws
   * elsewhere, {@link changesDraws}) and `composition` when the stage
   * reported a change since the last sync or is another stage. `reset`
   * has the view drop what it drew and take the scene whole (a view newly
   * attached).
   */
  sync(timeOnly: boolean, reset = false): Channel[] {
    const request = this.#host.request();
    const { session, time } = request;
    const changed: Channel[] = [];
    if (session !== null) {
      const promoted = request.selection === null || request.selection === "/" ? [] : [request.selection];
      const update = this.#model.sync(session, time, this.#scope(session, request), budgetFor(request.budgetChoice), promoted, this.#resident);
      this.scene = this.#model.scene(time);
      this.cost = this.#model.cost();
      this.budget = this.#model.budgetReport();
      if (update.reset || update.records > 0 || update.timeChanged || update.added.size + update.removed.size > 0) this.lastUpdate = update;
      this.viewport?.update(this.scene, reset ? { ...update, reset: true } : update, this.highlighted());
      if (!timeOnly || update.reset || update.records > 0 || changesDraws(update)) changed.push("draws");
    }
    const changes = session?.changeLog.length ?? 0;
    if (this.#synced.session !== session || this.#synced.changes !== changes) changed.push("composition");
    this.#synced = { session, changes };
    return changed;
  }

  /** Restyles the view for a selection that changed nothing of the draws. */
  restyle(): void {
    this.viewport?.update(this.scene, this.noUpdate(), this.highlighted());
  }

  /**
   * What drawing the whole scene would cost in full, estimated from counts
   * (nothing built), at the current time; made when first asked for and
   * again after an edit. `null` with no scene, or when it cannot be made.
   */
  estimate(): DrawCost | null {
    const { session, time } = this.#host.request();
    if (session === null) return null;
    const at = this.#estimate;
    if (at !== null && at.session === session && at.changes === session.changeLog.length && at.time === time) return at.cost;
    let cost: DrawCost | null = null;
    try {
      cost = this.#model.estimate(session.stage, time, { roots: ["/"] });
    } catch (error) {
      if (moduleTrap(error) !== null) throw error;
    }
    this.#estimate = { session, changes: session.changeLog.length, time, cost };
    return cost;
  }

  /** The draw list's scope: {@link scopeFor} the choice and the asset view's focus, which it reads again. */
  #scope(session: Session, request: DrawRequest): DrawScope {
    this.focus = assetFocus(session.stage, request.selection, request.property, request.time);
    return scopeFor(request.drawScope, this.focus);
  }

  /** The prim the viewport highlights: the selection, or, drawing it alone, only a face subset of it. */
  highlighted(): string | null {
    const request = this.#host.request();
    if (request.drawScope !== "selection") return request.selection;
    return this.focus.kind === "geometry" ? this.focus.subset : null;
  }

  /** An update that changes nothing: a restyle for a new selection. */
  noUpdate(): ViewportUpdate {
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
}

/**
 * Whether `update` changed what the draws are, beyond where they are: an
 * item added, removed, shown, hidden, recolored or rebound, a buffer
 * added, refilled, rebuilt or freed, a light, camera, material or light
 * link changed, a skeleton bound. A time change that only moves draws
 * (`matrices`) or poses skins on the GPU changes none of it.
 */
export function changesDraws(update: ViewportUpdate): boolean {
  return (
    update.added.size +
      update.removed.size +
      update.visibility.size +
      update.recolored.size +
      update.rebound.size +
      update.buffersAdded.size +
      update.refilled.size +
      update.rebuilt.size +
      update.freed.size +
      update.lightsAdded.size +
      update.lightsRemoved.size +
      update.lightsChanged.size +
      update.cameras.size +
      update.materialsAdded.size +
      update.materialsRemoved.size +
      update.materialsChanged.size +
      update.links +
      update.reskinned.size >
    0
  );
}
