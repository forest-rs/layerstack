// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a draw list draws under a budget: each item's cost estimated from
// counts before anything is built, every item given a stand-in first and
// then refined to its full geometry, most worth per share of the budget
// first, while the budget holds (as 3D Tiles keeps coarser tiles and stops refining), what
// does not fit reported. Admission is a pure pass over the requests, so
// the same stage always degrades the same way.
import { INSTANCE_GPU_BYTES } from "./cost.js";
import type { BufferCost } from "./cost.js";
import type { ItemSource } from "./draw.js";

/**
 * The most a {@link DrawList} draws ({@link DrawOptions.budget}). Every
 * limit is optional, and one left out does not limit; a list without a
 * budget draws everything. The library has no default: the host decides.
 *
 * A limit bounds the items drawn, as {@link DrawCost} counts them from
 * counts before anything is built (a mesh's triangles exactly, its
 * vertices and bytes as an estimate).
 */
export interface DrawBudget {
  /** Bytes uploaded: each distinct buffer's once, and {@link INSTANCE_GPU_BYTES} per instance drawn. */
  readonly gpuBytes?: number;
  /** Triangles drawn: each item's buffer's times its instances, so instancing counts in full. */
  readonly triangles?: number;
  /** Instances drawn, over every item: matrices, stand-ins' included. */
  readonly instances?: number;
}

/** A limit of a {@link DrawBudget}. */
export type BudgetLimit = "gpuBytes" | "triangles" | "instances";

/**
 * How an item is drawn under a budget, best first:
 *
 * - `full`: its geometry, as without a budget.
 * - `standin`: a box of its extent at each of its instances, drawn as a
 *   `bounds` draw mode stand-in ({@link DrawBuffers.standin}, its lines in
 *   the item prim's space) in Hydra's default draw mode color. A point
 *   instancer's item draws one box over all the instances of its
 *   prototype, in the instancer's space: one matrix, the instancer's, and
 *   no {@link DrawItem.ids}. A model drawn by its own draw mode has no
 *   such rung: its stand-in is its full geometry.
 * - `skipped`: nothing; the item is not listed.
 */
export type Rung = "full" | "standin" | "skipped";

/** What drawing an item costs against a {@link DrawBudget}, in its units. */
export interface BudgetCost {
  /** Bytes uploaded: its buffer's (when no item admitted before shares it) and its instances'. */
  readonly gpuBytes: number;
  /** Triangles drawn: its buffer's times its instances. */
  readonly triangles: number;
  /** Instances drawn. */
  readonly instances: number;
}

/** An item's place under the budget: the rung it was granted and why. */
export interface BudgetGrant {
  /** The item's id ({@link DrawItem.id}) while it is listed; `null` when skipped. */
  readonly item: number | null;
  readonly source: ItemSource;
  /** The prim whose geometry is drawn ({@link DrawItem.prim}). */
  readonly prim: string;
  readonly rung: Rung;
  /** What its full geometry would cost. */
  readonly requested: BudgetCost;
  /** What its rung costs: when `full`, `requested` less its buffer when an item granted before it shares it; nothing when `skipped`. */
  readonly granted: BudgetCost;
  /**
   * Why it is below `full`: the limits raising it to its full geometry
   * would have exceeded, on top of every item's floor and the items
   * refined before it; empty when `full`. A `skipped` item's stand-in did
   * not fit either.
   */
  readonly over: readonly BudgetLimit[];
  /** Whether it was promoted ({@link DrawList.promote}), and so admitted before every item that was not. */
  readonly promoted: boolean;
  /** Its place in the admission order, 0 first. */
  readonly order: number;
}

/**
 * Where a draw list stands against its budget ({@link DrawList.budgetReport}).
 * Only items drawing at least one instance are admitted; an item drawing
 * none (invisible, of a purpose not drawn) is not listed and costs nothing
 * until it draws one.
 */
export interface BudgetReport {
  readonly budget: DrawBudget;
  /** What every item's full geometry would cost: each distinct buffer once. */
  readonly requested: BudgetCost;
  /** What the rungs granted cost. */
  readonly granted: BudgetCost;
  /** Items admitted: those drawing at least one instance. */
  readonly items: number;
  /** Every item below `full`, in admission order. */
  readonly degraded: readonly BudgetGrant[];
}

/** What a diff changed under the budget ({@link DrawDiff.budget}). */
export interface BudgetChange {
  /** What every item's full geometry would cost now. */
  readonly requested: BudgetCost;
  /** What the rungs granted cost now. */
  readonly granted: BudgetCost;
  /** Items below `full` now. */
  readonly degraded: number;
  /**
   * Items whose rung changed in this diff, in admission order: new items
   * below `full`, and listed or withheld items moved to another rung (back
   * to `full` too). An item that stops drawing anything is not listed.
   */
  readonly changed: readonly BudgetGrant[];
}

/** @internal An item as the budget weighs it, read from counts before anything is built. */
export interface BudgetRequest {
  /** The item's source key: what the list keys its items by. */
  readonly key: string;
  readonly source: ItemSource;
  readonly prim: string;
  /** Its buffer's cost, once however many instances draw it. */
  readonly geometry: BufferCost;
  /** What it shares its buffer by: items with the same buffer key pay for it once. */
  readonly buffer: string;
  /** Instances drawn. */
  readonly instances: number;
  /** World bound diagonal of its prim: times {@link instances}, its importance ({@link density}). */
  readonly size: number;
  /** 0 for `default` and `render`, 1 for `proxy`, 2 for `guide`: render geometry before its proxy. */
  readonly purpose: number;
  /** The boxes its stand-in draws: its instances, one for a point instancer's item; 0 for a model drawn by its draw mode, which has no stand-in. */
  readonly standinInstances: number;
  /** Paths a change under which may change its cost, and against which promotion matches. */
  readonly depends: readonly string[];
  /** Whether its cost might differ at another time: its visibility or counts (its instancer's) might; {@link DrawList.setTime} weighs it again. */
  readonly varies: boolean;
}

/** @internal A rung granted, with its cost and why. */
export interface Grant {
  readonly rung: Rung;
  readonly granted: BudgetCost;
  readonly over: readonly BudgetLimit[];
  readonly order: number;
  readonly promoted: boolean;
}

/** Bytes a `bounds` stand-in uploads: twelve edges, two `f32` endpoints of three numbers each. */
export const STANDIN_GPU_BYTES = 12 * 2 * 3 * 4;

const NOTHING: BudgetCost = Object.freeze({ gpuBytes: 0, triangles: 0, instances: 0 });

/**
 * The least share of the budget an item is taken to cost, so an item
 * that costs (almost) nothing ranks by its importance alone and never
 * divides by zero: a millionth.
 */
export const LEAST_SHARE = 1e-6;

/**
 * @internal How much `request` is worth per share of `budget` it costs:
 * its importance (world bound diagonal times instances) over its full
 * geometry's dominant fraction of the budget, the largest of its bytes,
 * triangles and instances each over its limit, at least
 * {@link LEAST_SHARE}. Under a budget without limits, its importance.
 */
export function density(request: BudgetRequest, budget: DrawBudget): number {
  const full = fullCost(request, false);
  let share = LEAST_SHARE;
  if (budget.gpuBytes !== undefined) share = Math.max(share, full.gpuBytes / Math.max(budget.gpuBytes, 1));
  if (budget.triangles !== undefined) share = Math.max(share, full.triangles / Math.max(budget.triangles, 1));
  if (budget.instances !== undefined) share = Math.max(share, full.instances / Math.max(budget.instances, 1));
  const importance = request.size * request.instances;
  return Number.isFinite(importance) ? importance / share : 0;
}

/**
 * @internal The admission order: promoted first; then `render` and
 * `default` before `proxy` before `guide`; then most worth per share of
 * the budget first ({@link density}: greedy by value density, as for a
 * knapsack); ties by prim, then key. A total order, so admission is
 * deterministic.
 */
export function admissionOrder(a: BudgetRequest, b: BudgetRequest, promotedA: boolean, promotedB: boolean, densityA: number, densityB: number): number {
  if (promotedA !== promotedB) return promotedA ? -1 : 1;
  if (a.purpose !== b.purpose) return a.purpose - b.purpose;
  if (densityA !== densityB) return densityB > densityA ? 1 : -1;
  if (a.prim !== b.prim) return a.prim < b.prim ? -1 : 1;
  return a.key < b.key ? -1 : a.key > b.key ? 1 : 0;
}

/** @internal What `request`'s full geometry costs, its buffer counted unless already paid for. */
export function fullCost(request: BudgetRequest, bufferPaid: boolean): BudgetCost {
  return {
    gpuBytes: (bufferPaid ? 0 : request.geometry.gpuBytes) + request.instances * INSTANCE_GPU_BYTES,
    triangles: request.geometry.triangles * request.instances,
    instances: request.instances,
  };
}

const WITHIN: readonly BudgetLimit[] = Object.freeze([]);

/** The limits `cost` would exceed on top of `used`; one shared empty array when it fits. */
function exceeded(budget: DrawBudget, used: BudgetCost, cost: BudgetCost): readonly BudgetLimit[] {
  const gpuBytes = budget.gpuBytes !== undefined && used.gpuBytes + cost.gpuBytes > budget.gpuBytes;
  const triangles = budget.triangles !== undefined && used.triangles + cost.triangles > budget.triangles;
  const instances = budget.instances !== undefined && used.instances + cost.instances > budget.instances;
  if (!gpuBytes && !triangles && !instances) return WITHIN;
  const over: BudgetLimit[] = [];
  if (gpuBytes) over.push("gpuBytes");
  if (triangles) over.push("triangles");
  if (instances) over.push("instances");
  return over;
}

/** @internal The result of one admission pass. */
export interface Admission {
  readonly grants: Map<string, Grant>;
  readonly requested: BudgetCost;
  readonly granted: BudgetCost;
  readonly degraded: number;
}

/** @internal What `request`'s stand-in costs; `null` when it has none. */
export function standinCost(request: BudgetRequest): BudgetCost | null {
  if (request.standinInstances === 0) return null;
  return { gpuBytes: STANDIN_GPU_BYTES + request.standinInstances * INSTANCE_GPU_BYTES, triangles: 0, instances: request.standinInstances };
}

/**
 * @internal Admits `order` (sorted by {@link admissionOrder}) under
 * `budget` in two passes. First the floor: each request, in order, gets
 * its stand-in where it fits (a model drawn by its draw mode, which has
 * none, its full geometry), else is skipped. Then refinement: each
 * request, in order, is raised to its full geometry where the difference
 * fits what was granted. So everything drawable is drawn at least as a
 * box, and full geometry goes to the most important first. A request
 * drawing no instance is granted `skipped` at no cost and not counted as
 * degraded. No allocation per request beyond its grant.
 */
export function admit(order: readonly BudgetRequest[], promoted: (request: BudgetRequest) => boolean, budget: DrawBudget): Admission {
  // Buffers paid for: by the full rungs granted (granted totals), and by
  // any request (requested totals).
  const paid = new Set<string>();
  const requestedBuffers = new Set<string>();
  const used = { gpuBytes: 0, triangles: 0, instances: 0 };
  const requested = { gpuBytes: 0, triangles: 0, instances: 0 };
  const add = (into: { gpuBytes: number; triangles: number; instances: number }, cost: BudgetCost, sign = 1) => {
    into.gpuBytes += sign * cost.gpuBytes;
    into.triangles += sign * cost.triangles;
    into.instances += sign * cost.instances;
  };
  const rungs: Rung[] = new Array<Rung>(order.length);
  // What the requests granted their full geometry in the floor pass paid.
  const floorFull = new Map<number, BudgetCost>();
  order.forEach((request, index) => {
    rungs[index] = "skipped";
    if (request.instances === 0) return;
    if (!requestedBuffers.has(request.buffer)) requested.gpuBytes += request.geometry.gpuBytes;
    requestedBuffers.add(request.buffer);
    requested.gpuBytes += request.instances * INSTANCE_GPU_BYTES;
    requested.triangles += request.geometry.triangles * request.instances;
    requested.instances += request.instances;
    const floor = standinCost(request);
    if (floor !== null) {
      if (exceeded(budget, used, floor).length === 0) {
        add(used, floor);
        rungs[index] = "standin";
      }
    } else {
      const full = fullCost(request, paid.has(request.buffer));
      if (exceeded(budget, used, full).length === 0) {
        add(used, full);
        paid.add(request.buffer);
        rungs[index] = "full";
        floorFull.set(index, full);
      }
    }
  });
  const grants = new Map<string, Grant>();
  let degraded = 0;
  order.forEach((request, index) => {
    const isPromoted = promoted(request);
    if (request.instances === 0) {
      grants.set(request.key, { rung: "skipped", granted: NOTHING, over: [], order: index, promoted: isPromoted });
      return;
    }
    const rung = rungs[index]!;
    if (rung === "full") {
      grants.set(request.key, { rung, granted: floorFull.get(index)!, over: [], order: index, promoted: isPromoted });
      return;
    }
    const full = fullCost(request, paid.has(request.buffer));
    const floor = rung === "standin" ? standinCost(request)! : NOTHING;
    // The difference refinement adds: the full cost, less the floor it replaces.
    const delta = { gpuBytes: full.gpuBytes - floor.gpuBytes, triangles: full.triangles - floor.triangles, instances: full.instances - floor.instances };
    const over = exceeded(budget, used, delta);
    if (over.length === 0) {
      add(used, delta);
      paid.add(request.buffer);
      grants.set(request.key, { rung: "full", granted: full, over, order: index, promoted: isPromoted });
    } else {
      degraded++;
      grants.set(request.key, { rung, granted: floor, over, order: index, promoted: isPromoted });
    }
  });
  return { grants, requested, granted: used, degraded };
}
