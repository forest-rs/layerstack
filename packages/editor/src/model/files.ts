// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What leaves the page: which layers a save writes and what it leaves
// alone, what the files are called, which assets a package still needs
// the host to supply, and why a root is edited in a session layer. The
// stage writes nothing anywhere (`Stage.save`, `exportLayer`,
// `exportFlattened`, `exportPackage` hand back bytes); the app downloads
// them or writes them into the folder they came from.
import type { Layer, LayerFormat, Stage, UnpackagedAsset } from "@forest-rs/layerstack";

/** What a save does with one dirty layer. */
export type SaveDisposition =
  /** Written to its identifier. */
  | "write"
  /** The session layer: not part of the root, so a save of the stage leaves it; export it as its own layer. */
  | "session"
  /** Read-only (a package member): it cannot be saved, and stays dirty. */
  | "readOnly"
  /** Muted: a save of the stage leaves it dirty, as OpenUSD's does. */
  | "muted";

/** One dirty layer, and what a save does with it. */
export interface SaveEntry {
  readonly layer: Layer;
  /** What it is saved as, or exported as for the session layer: a file name ({@link fileNames}). */
  readonly name: string;
  readonly disposition: SaveDisposition;
}

/**
 * What a save of `stage` does with each of its `dirty` layers
 * (`Stage.save`: the session layer and muted layers are left, a package
 * member fails), the written ones first. `session` is the session layer,
 * when the stage has one. A layer the stage no longer composes is found
 * only by the save itself (`SaveResult.skipped`).
 */
export function planSave(stage: Stage, dirty: readonly Layer[], session: Layer | null): SaveEntry[] {
  const names = fileNames(dirty.map((layer) => layer.identifier));
  const entries = dirty.map((layer, index): SaveEntry => {
    const disposition: SaveDisposition =
      layer === session || layer.anonymous ? "session" : layer.readOnly ? "readOnly" : stage.isMuted(layer) ? "muted" : "write";
    return { layer, name: names[index]!, disposition };
  });
  const order: readonly SaveDisposition[] = ["write", "session", "readOnly", "muted"];
  return entries.sort((a, b) => order.indexOf(a.disposition) - order.indexOf(b.disposition));
}

/**
 * A file name for the layer `identifier` names: its last segment
 * (`assets/robot.usda` is `robot.usda`; a package member's own,
 * `set.usdz[geo/rock.usda]` is `rock.usda`), `session.usda` for an
 * anonymous layer; with `format`, its extension that format's.
 */
export function fileName(identifier: string | null, format?: LayerFormat | "usdz"): string {
  let name = "session.usda";
  if (identifier !== null) {
    const open = identifier.indexOf("[");
    const inner = open >= 0 && identifier.endsWith("]") ? identifier.slice(open + 1, -1) : identifier;
    name = inner.slice(inner.lastIndexOf("/") + 1) || "layer.usda";
  }
  if (format === undefined) return name;
  const dot = name.lastIndexOf(".");
  return `${dot > 0 ? name.slice(0, dot) : name}.${format}`;
}

/**
 * File names for several layers at once, none the same: each its
 * {@link fileName}, and where two would share one, its whole identifier
 * with its folders joined by `_` (`assets_robot.usda`), so downloads
 * landing in one folder do not overwrite each other.
 */
export function fileNames(identifiers: readonly (string | null)[]): string[] {
  const names = identifiers.map((identifier) => fileName(identifier));
  const counts = new Map<string, number>();
  for (const name of names) counts.set(name, (counts.get(name) ?? 0) + 1);
  return names.map((name, index) => {
    const identifier = identifiers[index] ?? null;
    if (counts.get(name)! < 2 || identifier === null) return name;
    return identifier.replace(/[[\]]/g, "_").replace(/\.\.\//g, "").replace(/\//g, "_");
  });
}

/** Whether `identifier` names a USDZ package (by its extension). */
export function isPackage(identifier: string): boolean {
  return /\.usdz$/i.test(identifier);
}

/** Whether `bytes` are a zip archive, as every USDZ package is (`PK\x03\x04`). */
function isZip(bytes: Uint8Array): boolean {
  return bytes.length >= 4 && bytes[0] === 0x50 && bytes[1] === 0x4b && bytes[2] === 0x03 && bytes[3] === 0x04;
}

/**
 * Why the root `identifier` (whose bytes are `bytes`) must be edited in a
 * session layer: a USDZ package is read-only (AOUSD Core §16), so it has
 * no edit layer of its own. `null` for a root that can take edits.
 */
export function sessionLayerReason(identifier: string, bytes: Uint8Array): string | null {
  return isPackage(identifier) || isZip(bytes) ? "the root is a .usdz package, which is read-only" : null;
}

/** Why any other root has no edit layer: the stage says it cannot be saved. */
export const UNSAVABLE_ROOT = "the root layer cannot be saved";

/**
 * What a package's unresolved dependencies (`PackageResult.unresolved`)
 * ask of the host: the identifiers to fetch and supply, a UDIM set's
 * tiles 1001 to 1100 among them, and the dependencies the host cannot
 * help (confirmed absent, or not to be read).
 */
export interface PackageWants {
  /** Identifiers to fetch and supply before packaging again: the packager named them. */
  readonly fetch: readonly string[];
  /** The other tiles of a UDIM set it named one of: those that exist are supplied too. */
  readonly tiles: readonly string[];
  /** Dependencies no fetch resolves. */
  readonly unresolved: readonly UnpackagedAsset[];
}

/**
 * The identifier an unresolved dependency's `reason` names when the host
 * can resolve it by supplying it: the module's storage says it was never
 * supplied, or its bytes were released after a read and are wanted again
 * (`identifier: the host has not supplied it yet`). `null` for any other
 * reason.
 */
export function suppliable(reason: string): string | null {
  const match = /^(.+?): the host(?: has not supplied it yet|'s bytes were released)/.exec(reason);
  return match?.[1] ?? null;
}

/** The first and last UDIM tiles (`<UDIM>` names 1001 to 1100, ten tiles a row). */
const UDIM_FIRST = 1001;
const UDIM_LAST = 1100;

/** {@link PackageWants} of an export's unresolved dependencies. */
export function packageWants(unresolved: readonly UnpackagedAsset[]): PackageWants {
  const fetch = new Set<string>();
  const tiles = new Set<string>();
  const left: UnpackagedAsset[] = [];
  for (const dependency of unresolved) {
    const identifier = suppliable(dependency.reason);
    if (identifier === null) {
      left.push(dependency);
      continue;
    }
    fetch.add(identifier);
    // A UDIM set is asked for a tile at a time: every other tile is looked for at once instead.
    const at = dependency.assetPath.indexOf("<UDIM>");
    if (at < 0) continue;
    const tail = dependency.assetPath.length - at - "<UDIM>".length;
    const tile = identifier.length - tail - 4;
    if (tile < 0 || !/^\d{4}$/.test(identifier.slice(tile, tile + 4))) continue;
    for (let number = UDIM_FIRST; number <= UDIM_LAST; number++) {
      tiles.add(`${identifier.slice(0, tile)}${number}${identifier.slice(tile + 4)}`);
    }
  }
  return { fetch: [...fetch], tiles: [...tiles].filter((tile) => !fetch.has(tile)), unresolved: left };
}
