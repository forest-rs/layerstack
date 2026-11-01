// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// When each step of the asset loop happened, for the waterfall: the
// session records `performance.now()` as it asks the stage what it wants,
// fetches, supplies and has the stage read, round by round. No DOM and no
// React; the session owns one of these and the panel reads it.
import type { AssetFailure, AssetUse, LoadResult, UnavailableAsset } from "@forest-rs/layerstack";

/** What uses an asset: what the stage says (`UnavailableAsset.usedBy`), or `root` for the layer the session opened. */
export type AssetRole = AssetUse | "root";

/** Where an asset's bytes came from. */
export type AssetSourceKind = "fetched" | "saved" | "absent" | "error";

/** What became of an asset once the stage read what was supplied. */
export type AssetOutcome = "pending" | "read" | "failed" | "absent";

/** Where an asset is authored: the path as written, and the layer that writes it. */
export interface AssetMention {
  readonly assetPath: string;
  readonly anchor: string;
  readonly usedBy: AssetRole;
}

/**
 * One asset's way through the loop, in `performance.now()` milliseconds.
 * The stage reads every asset a round supplied in one `loadSupplied`, so
 * an asset's read is its round's: {@link RoundTiming.readStart} to
 * {@link RoundTiming.readEnd}.
 */
export interface AssetTiming {
  readonly identifier: string;
  /** The round that wanted it; 0 for the root. */
  readonly round: number;
  /** What uses it, each kind once, in the order the stage named them. */
  readonly roles: AssetRole[];
  /** Where it is authored. */
  readonly mentions: AssetMention[];
  /** The prims composed without it until it loads (`UnavailableAsset.prims`), each once. */
  readonly prims: string[];
  /** When the stage's `unavailable()` named it. */
  wantedAt: number;
  /** When the host's fetch was called; `null` before it is. */
  fetchStart: number | null;
  /** When the fetch settled; `null` before it does. */
  fetchEnd: number | null;
  /** The bytes that arrived; `null` for none (absent, failed, pending). */
  bytes: number | null;
  /** Where the bytes came from; `null` before the fetch settles. */
  source: AssetSourceKind | null;
  /** When `supply` handed the bytes (or their absence) to the stage. */
  suppliedAt: number | null;
  outcome: AssetOutcome;
  /** The read or fetch failure, in words. */
  failure: string | null;
}

/**
 * One round of the loop: the stage names what it wants, the host fetches
 * it all at once, supplies what arrived, and the stage reads it. The
 * stage's own time is the want, supply and read; the rest is waiting on
 * fetches. Round 0 is the root: fetched, then read by `Layerstack.open`.
 */
export interface RoundTiming {
  readonly index: number;
  /** When `unavailable()` was called. */
  readonly wantStart: number;
  /** When it returned. */
  readonly wantEnd: number;
  /** When every fetch of the round settled; `null` before. */
  fetchEnd: number | null;
  /** When the last `supply` returned (bytes copied into the module); `null` before. */
  supplyEnd: number | null;
  /** When `loadSupplied()` (or, for the root, `Layerstack.open`) was called and returned. */
  readStart: number | null;
  readEnd: number | null;
  /** The distinct identifiers the round wanted. */
  readonly assets: number;
  /** The layers the read loaded (package members included). */
  loaded: number;
}

/**
 * One run of the loop and what caused it: the open, a variant selected,
 * an undo, a layer unmuted.
 */
export interface LoopRun {
  readonly cause: string;
  readonly startedAt: number;
  /** When the loop found nothing more unavailable, or failed; `null` while it runs. */
  endedAt: number | null;
  readonly rounds: RoundTiming[];
  /** Every asset, in the order the stage wanted them. */
  readonly assets: AssetTiming[];
  /**
   * The final `unavailable()` that found nothing left: stage time after
   * the last round's read. `null` while the loop runs.
   */
  settle: { readonly start: number; readonly end: number } | null;
  /** Why the loop stopped early; `null` when it finished. */
  error: string | null;
  /** Bumped on every record, so a view can tell the run changed. */
  version: number;
}

/** A run's totals, as the waterfall's header shows them. */
export interface LoopSummary {
  readonly assets: number;
  /** Bytes that arrived from the host (fetched or saved). */
  readonly bytes: number;
  /** Rounds after the root's. */
  readonly rounds: number;
  /** Start to end (to now while it runs). */
  readonly wall: number;
  /** Rounds' fetch phases: the loop waiting on the host. */
  readonly fetching: number;
  /** The stage's calls: `unavailable()`, `supply()`, `loadSupplied()`, the root's `Layerstack.open`. */
  readonly stage: number;
  readonly failed: number;
  readonly absent: number;
}

/** The time between two marks, 0 when either is missing. */
function span(start: number | null, end: number | null): number {
  return start === null || end === null ? 0 : Math.max(0, end - start);
}

/** Totals of `run`, measured up to `now` while it runs. */
export function summarize(run: LoopRun, now: number = performance.now()): LoopSummary {
  let bytes = 0;
  let failed = 0;
  let absent = 0;
  for (const asset of run.assets) {
    bytes += asset.bytes ?? 0;
    if (asset.outcome === "failed") failed += 1;
    if (asset.outcome === "absent") absent += 1;
  }
  let fetching = 0;
  let stage = span(run.settle?.start ?? null, run.settle?.end ?? null);
  for (const round of run.rounds) {
    stage += span(round.wantStart, round.wantEnd) + span(round.fetchEnd, round.supplyEnd) + span(round.readStart, round.readEnd);
    fetching += span(round.wantEnd, round.fetchEnd);
  }
  return {
    assets: run.assets.length,
    bytes,
    rounds: run.rounds.filter((round) => round.index > 0).length,
    wall: (run.endedAt ?? now) - run.startedAt,
    fetching,
    stage,
    failed,
    absent,
  };
}

/**
 * Records the loop's timings as the session runs it. Every method takes
 * the moment it marks, so the session measures around the stage's calls
 * and the recorder stays out of them.
 */
export class LoopRecorder {
  /** Every run, oldest first. */
  readonly runs: LoopRun[] = [];
  #run: LoopRun | null = null;
  #byIdentifier = new Map<string, AssetTiming>();

  /** The run in progress, if any. */
  get current(): LoopRun | null {
    return this.#run;
  }

  /** Starts a run for `cause` at `at`. */
  begin(cause: string, at: number): LoopRun {
    const run: LoopRun = { cause, startedAt: at, endedAt: null, rounds: [], assets: [], settle: null, error: null, version: 0 };
    this.#run = run;
    this.#byIdentifier = new Map();
    this.runs.push(run);
    return run;
  }

  /** The root layer: wanted and fetched as round 0, before the stage exists. */
  root(identifier: string, start: number, fetchEnd: number, bytes: number, source: AssetSourceKind): void {
    const run = this.#active();
    run.rounds.push({ index: 0, wantStart: start, wantEnd: start, fetchEnd, supplyEnd: fetchEnd, readStart: null, readEnd: null, assets: 1, loaded: 0 });
    const asset = this.#asset(identifier, 0, start);
    asset.roles.push("root");
    asset.fetchStart = start;
    asset.fetchEnd = fetchEnd;
    asset.bytes = bytes;
    asset.source = source;
    asset.suppliedAt = fetchEnd;
    run.version += 1;
  }

  /** The root's read: `Layerstack.open` from `start` to `end`. */
  rootRead(start: number, end: number): void {
    const round = this.#round();
    round.readStart = start;
    round.readEnd = end;
    round.loaded = 1;
    for (const asset of this.#active().assets) if (asset.round === 0) asset.outcome = "read";
    this.#active().version += 1;
  }

  /**
   * A round: `unavailable()` ran from `start` to `end` and returned
   * `wanted`. An empty `wanted` settles the run instead.
   */
  want(wanted: readonly UnavailableAsset[], start: number, end: number): void {
    const run = this.#active();
    if (wanted.length === 0) {
      run.settle = { start, end };
      run.version += 1;
      return;
    }
    const index = run.rounds.length === 0 || run.rounds[0]!.index !== 0 ? run.rounds.length + 1 : run.rounds.length;
    const before = run.assets.length;
    for (const want of wanted) {
      const asset = this.#byIdentifier.get(want.identifier) ?? this.#asset(want.identifier, index, end);
      if (!asset.roles.includes(want.usedBy)) asset.roles.push(want.usedBy);
      asset.mentions.push({ assetPath: want.assetPath, anchor: want.anchor.identifier ?? "(anonymous)", usedBy: want.usedBy });
      for (const prim of want.prims) if (!asset.prims.includes(prim)) asset.prims.push(prim);
    }
    run.rounds.push({
      index,
      wantStart: start,
      wantEnd: end,
      fetchEnd: null,
      supplyEnd: null,
      readStart: null,
      readEnd: null,
      assets: run.assets.length - before,
      loaded: 0,
    });
    run.version += 1;
  }

  /** The host's fetch of `identifier` was called at `at`. */
  fetchStarted(identifier: string, at: number): void {
    const asset = this.#byIdentifier.get(identifier);
    if (asset === undefined) return;
    asset.fetchStart = at;
    this.#active().version += 1;
  }

  /** The fetch of `identifier` settled at `at`: `bytes` arrived (`null` for none) from `source`. */
  fetchSettled(identifier: string, at: number, bytes: number | null, source: AssetSourceKind, failure: string | null = null): void {
    const asset = this.#byIdentifier.get(identifier);
    if (asset === undefined) return;
    asset.fetchEnd = at;
    asset.bytes = bytes;
    asset.source = source;
    if (failure !== null) {
      asset.outcome = "failed";
      asset.failure = failure;
    }
    this.#active().version += 1;
  }

  /** Every fetch of the round settled at `at`. */
  fetched(at: number): void {
    this.#round().fetchEnd = at;
    this.#active().version += 1;
  }

  /** `supply` handed `identifier` to the stage at `at`. */
  supplied(identifier: string, at: number): void {
    const asset = this.#byIdentifier.get(identifier);
    if (asset !== undefined) asset.suppliedAt = at;
  }

  /** The round's supplies ended at `at`. */
  suppliesDone(at: number): void {
    this.#round().supplyEnd = at;
    this.#active().version += 1;
  }

  /**
   * `loadSupplied()` ran from `start` to `end` and returned `result`:
   * each asset of the round is read, failed (matched by its authored path
   * and anchor) or absent.
   */
  read(result: LoadResult, start: number, end: number): void {
    const run = this.#active();
    const round = this.#round();
    round.readStart = start;
    round.readEnd = end;
    round.loaded = result.loaded.length;
    const failures = new Map<string, AssetFailure>();
    for (const failure of result.failures) failures.set(mentionKey(failure.assetPath, failure.anchor?.identifier ?? null), failure);
    for (const asset of run.assets) {
      if (asset.round !== round.index || asset.outcome === "failed") continue;
      const failure = asset.mentions.map((mention) => failures.get(mentionKey(mention.assetPath, mention.anchor))).find((found) => found !== undefined);
      if (failure !== undefined && failure.code !== "io.not_found") {
        asset.outcome = "failed";
        asset.failure = `${failure.code}: ${failure.message}`;
      } else if (asset.bytes === null || failure !== undefined) {
        asset.outcome = "absent";
      } else {
        asset.outcome = "read";
      }
    }
    run.version += 1;
  }

  /** The run ended at `at`; `error` says why when it stopped early. */
  end(at: number, error: string | null = null): void {
    const run = this.#active();
    run.endedAt = at;
    run.error = error;
    run.version += 1;
    this.#run = null;
  }

  /** Drops the run in progress: it wanted nothing, so there is nothing to show. */
  discard(): void {
    const run = this.#run;
    if (run === null) return;
    this.runs.splice(this.runs.indexOf(run), 1);
    this.#run = null;
  }

  #active(): LoopRun {
    if (this.#run === null) throw new Error("no loop run is being recorded");
    return this.#run;
  }

  #round(): RoundTiming {
    const round = this.#active().rounds.at(-1);
    if (round === undefined) throw new Error("no round is being recorded");
    return round;
  }

  #asset(identifier: string, round: number, at: number): AssetTiming {
    const asset: AssetTiming = {
      identifier,
      round,
      roles: [],
      mentions: [],
      prims: [],
      wantedAt: at,
      fetchStart: null,
      fetchEnd: null,
      bytes: null,
      source: null,
      suppliedAt: null,
      outcome: "pending",
      failure: null,
    };
    this.#byIdentifier.set(identifier, asset);
    this.#active().assets.push(asset);
    return asset;
  }
}

/** A key matching a failure to the mention it reports: the authored path and its anchor's identifier. */
function mentionKey(assetPath: string, anchor: string | null): string {
  return `${assetPath}\u0000${anchor ?? "(anonymous)"}`;
}
