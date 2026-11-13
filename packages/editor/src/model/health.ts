// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The stage's health, as the Health tab shows it: the composition errors
// the stage reports (`Stage.status`), grouped by the prim whose
// composition found each (or the layer stack, for one in a layer stack);
// the assets that contribute nothing (missing, or failed to read) with
// the layer that authors each and the prims the asset loop saw wanting
// it; the assets not supplied yet; and the paths the last move or delete
// left naming what moved (`EditReceipt.unrepaired`). Built once per
// change of the stage, never per frame: the prims with errors are a set
// the prim tree looks its rows up in. No DOM, no stage: the caller reads
// those and this sorts them, so it is tested in Node.

/** One composition error (`Issue`). */
export interface HealthIssue {
  readonly code: string;
  readonly message: string;
  /** The prim whose composition found it; `null` for one in a layer stack. */
  readonly prim: string | null;
}

/** An asset the stage uses that contributes nothing, or is not supplied yet. */
export interface HealthAsset<L> {
  /** The asset path as authored. */
  readonly assetPath: string;
  /** The layer that authors it. */
  readonly anchor: L;
  /** What uses it (`reference`, `payload`, `sublayer`, ...). */
  readonly usedBy: string;
  /** The identifier read for it; `null` when none was. */
  readonly identifier: string | null;
  /** Why it contributes nothing: the read failure's code (`io.not_found`); `null` when it is waiting to be supplied, or no read failed. */
  readonly reason: string | null;
  /** The prims composed without it, as the asset loop recorded the stage naming them (`/` for the stage's own layer stack). */
  readonly prims: readonly string[];
}

/** A path an edit left naming what it moved or deleted (`UnrepairedPath`). */
export interface HealthUnrepaired<L> {
  readonly layer: L;
  /** The spec that holds it. */
  readonly spec: string;
  /** The field of the spec that authors it (`targets`, `references`, ...). */
  readonly field: string;
  /** The path it names. */
  readonly path: string;
}

/** What {@link healthReport} reads. */
export interface HealthInput<L> {
  /** The prims the default traversal visits (`StageStatus.prims`). */
  readonly prims: number;
  readonly errors: readonly HealthIssue[];
  /** Missing, or failed to read (`StageStatus.unresolved`). */
  readonly unresolved: readonly Omit<HealthAsset<L>, "prims">[];
  /** Not supplied yet (`StageStatus.unavailable`), with the prims it names. */
  readonly unavailable: readonly HealthAsset<L>[];
  /** The prims the asset loop saw wanting each identifier (`AssetTiming.prims`, over every run). */
  readonly wanting: ReadonlyMap<string, readonly string[]>;
  /** The last move or delete on the undo stack: its label and what it left unrepaired; `null` when there is none. */
  readonly lastNamespaceEdit: { readonly label: string; readonly unrepaired: readonly HealthUnrepaired<L>[] } | null;
  /** The layers the stage holds, for the all-clear's words. */
  readonly layers: number;
}

/** The composition errors of one prim, or of the layer stacks (`prim` `null`). */
export interface ErrorGroup {
  readonly prim: string | null;
  readonly issues: readonly HealthIssue[];
}

/** The stage's health. */
export interface HealthReport<L> {
  /** Errors by prim, in namespace order, the layer stacks' first. */
  readonly groups: readonly ErrorGroup[];
  /** The prims with a composition error: what the prim tree marks. */
  readonly errorPrims: ReadonlySet<string>;
  readonly missing: readonly HealthAsset<L>[];
  readonly waiting: readonly HealthAsset<L>[];
  /** The last move or delete, and what it left; `null` when there was none. */
  readonly namespaceEdit: HealthInput<L>["lastNamespaceEdit"];
  /** How many problems: errors, assets not supplied, and paths left unrepaired (a missing asset is one of the errors). */
  readonly count: number;
  /** What was checked, in words: what the all-clear says. */
  readonly checked: string;
}

/** Namespace order of prim paths: `/A` before `/A/B` before `/B`. */
function comparePaths(a: string, b: string): number {
  const left = a.split("/");
  const right = b.split("/");
  for (let at = 0; at < Math.min(left.length, right.length); at++) {
    if (left[at] !== right[at]) return left[at]! < right[at]! ? -1 : 1;
  }
  return left.length - right.length;
}

/** The prims `identifier` was wanted by, the loop's and the stage's own together, each once, in namespace order. */
function primsOf(wanting: ReadonlyMap<string, readonly string[]>, identifier: string | null, own: readonly string[] = []): string[] {
  const loop = identifier === null ? [] : (wanting.get(identifier) ?? []);
  return [...new Set([...own, ...loop])].sort(comparePaths);
}

/** The stage's health from what it reports ({@link HealthInput}). */
export function healthReport<L>(input: HealthInput<L>): HealthReport<L> {
  const byPrim = new Map<string | null, HealthIssue[]>();
  for (const issue of input.errors) {
    const list = byPrim.get(issue.prim);
    if (list === undefined) byPrim.set(issue.prim, [issue]);
    else list.push(issue);
  }
  const groups = [...byPrim]
    .map(([prim, issues]) => ({ prim, issues }))
    .sort((a, b) => (a.prim === null ? -1 : b.prim === null ? 1 : comparePaths(a.prim, b.prim)));
  const errorPrims = new Set(input.errors.flatMap((issue) => (issue.prim === null ? [] : [issue.prim])));
  const missing = input.unresolved.map((asset) => ({ ...asset, prims: primsOf(input.wanting, asset.identifier) }));
  const waiting = input.unavailable.map((asset) => ({ ...asset, prims: primsOf(input.wanting, asset.identifier, asset.prims) }));
  const unrepaired = input.lastNamespaceEdit?.unrepaired.length ?? 0;
  const plural = (count: number, one: string, many = `${one}s`) => `${count} ${count === 1 ? one : many}`;
  const checked = [
    `No composition errors among ${plural(input.prims, "prim")}`,
    `every asset of ${plural(input.layers, "layer")} resolved`,
    input.lastNamespaceEdit === null ? "no move or delete to repair" : `${input.lastNamespaceEdit.label} left no path naming the old one`,
  ].join("; ");
  return {
    groups,
    errorPrims,
    missing,
    waiting,
    namespaceEdit: input.lastNamespaceEdit,
    count: input.errors.length + waiting.length + unrepaired,
    checked: `${checked}.`,
  };
}

/** What a run of the asset loop recorded of one asset: its identifier and the prims composed without it. */
export interface WantedAsset {
  readonly identifier: string;
  readonly prims: readonly string[];
}

/** The prims each identifier was wanted by, over every run of the asset loop. */
export function wantingPrims(runs: readonly { readonly assets: readonly WantedAsset[] }[]): Map<string, string[]> {
  const out = new Map<string, Set<string>>();
  for (const run of runs) {
    for (const asset of run.assets) {
      let prims = out.get(asset.identifier);
      if (prims === undefined) out.set(asset.identifier, (prims = new Set()));
      for (const prim of asset.prims) prims.add(prim);
    }
  }
  return new Map([...out].map(([identifier, prims]) => [identifier, [...prims]]));
}

/** What an error code says, for a heading: `composition.unresolved_asset` is "unresolved asset". */
export function issueKind(code: string): string {
  const dot = code.lastIndexOf(".");
  return code.slice(dot + 1).replaceAll("_", " ");
}
