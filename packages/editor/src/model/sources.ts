// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where a scene's bytes come from. The stage names each asset by an
// identifier relative to the root's directory (`assets/robot.usda`,
// `../textures/wood.png`); a source answers it from HTTP, a directory the
// person picked, or files they dropped. The stage never reads storage
// itself: the session's asset loop asks, and the source answers.
import type { Stage } from "@forest-rs/layerstack";
import type { Fetcher } from "./session.ts";

/** A scene to open: its root, and the storage its identifiers resolve in. */
export interface SceneSource {
  /** What the scene is called in the editor. */
  readonly label: string;
  /** The root layer's identifier. */
  readonly root: string;
  /** An asset's bytes; `null` when it does not exist. */
  readonly fetch: Fetcher;
  /** Whether an asset exists, without reading it: a UDIM set's tiles are asked one by one. */
  readonly exists: (identifier: string) => Promise<boolean>;
  /**
   * An asset's size in bytes, without reading it; `null` when the source
   * cannot tell (or it does not exist). What loading payloads would read
   * is weighed with it before fetching ({@link Session.supplyUnavailable}).
   */
  readonly size: (identifier: string) => Promise<number | null>;
}

/** The identifier's path segments, `.` and `..` resolved; `null` when it climbs out of the source. */
function segments(identifier: string): string[] | null {
  const out: string[] = [];
  for (const part of identifier.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (out.length === 0) return null;
      out.pop();
    } else {
      out.push(part);
    }
  }
  return out;
}

/**
 * A scene served over HTTP under `base`: a 404 is an absent asset. A
 * static host cannot list a directory, so `exists` reads the directory's
 * `listing.json` (an array of file names) when it has one, and asks with
 * `HEAD` otherwise.
 */
export function httpSource(label: string, base: URL, root: string): SceneSource {
  const listings = new Map<string, Promise<readonly string[] | null>>();
  const fetcher: Fetcher = async (identifier, signal) => {
    const response = await fetch(new URL(identifier, base), signal === undefined ? {} : { signal });
    if (response.status === 404) return null;
    if (!response.ok) throw new Error(`${identifier}: HTTP ${response.status}`);
    return new Uint8Array(await response.arrayBuffer());
  };
  return {
    label,
    root,
    fetch: fetcher,
    async exists(identifier) {
      const slash = identifier.lastIndexOf("/");
      const directory = identifier.slice(0, slash + 1);
      let listing = listings.get(directory);
      if (listing === undefined) {
        listing = fetch(new URL(`${directory}listing.json`, base)).then(async (response) =>
          response.ok ? ((await response.json()) as string[]) : null,
        );
        listings.set(directory, listing);
      }
      const names = await listing;
      if (names !== null) return names.includes(identifier.slice(slash + 1));
      const response = await fetch(new URL(identifier, base), { method: "HEAD" });
      return response.ok;
    },
    async size(identifier) {
      const response = await fetch(new URL(identifier, base), { method: "HEAD" });
      const length = response.ok ? response.headers.get("content-length") : null;
      return length === null ? null : Number(length);
    },
  };
}

/**
 * A scene in a directory the person picked (`showDirectoryPicker`): `root`
 * is the root layer's path inside it. Identifiers that climb out of the
 * directory are absent: the page can read nothing else.
 */
export function directorySource(directory: FileSystemDirectoryHandle, root: string): SceneSource {
  const file = async (identifier: string): Promise<File | null> => {
    const parts = segments(identifier);
    if (parts === null || parts.length === 0) return null;
    try {
      let handle = directory;
      for (const part of parts.slice(0, -1)) handle = await handle.getDirectoryHandle(part);
      return await (await handle.getFileHandle(parts.at(-1)!)).getFile();
    } catch (error) {
      if (error instanceof DOMException && (error.name === "NotFoundError" || error.name === "TypeMismatchError")) return null;
      throw error;
    }
  };
  return {
    label: `${directory.name}/${root}`,
    root,
    async fetch(identifier, signal) {
      const found = await file(identifier);
      // A file's read cannot be stopped; one not begun is not begun.
      signal?.throwIfAborted();
      return found === null ? null : new Uint8Array(await found.arrayBuffer());
    },
    async exists(identifier) {
      return (await file(identifier)) !== null;
    },
    async size(identifier) {
      return (await file(identifier))?.size ?? null;
    },
  };
}

/**
 * A scene in dropped files, by their paths relative to the drop (a dropped
 * folder's files keep theirs): `root` is the root layer's.
 */
export function filesSource(label: string, files: ReadonlyMap<string, File>, root: string): SceneSource {
  const file = (identifier: string) => {
    const parts = segments(identifier);
    return parts === null ? undefined : files.get(parts.join("/"));
  };
  return {
    label,
    root,
    async fetch(identifier, signal) {
      const found = file(identifier);
      signal?.throwIfAborted();
      return found === undefined ? null : new Uint8Array(await found.arrayBuffer());
    },
    async exists(identifier) {
      return file(identifier) !== undefined;
    },
    async size(identifier) {
      return file(identifier)?.size ?? null;
    },
  };
}

/** The file extensions of layers a stage can open as its root. */
export const LAYER_EXTENSIONS = [".usd", ".usda", ".usdc", ".usdz"] as const;

/** Whether `name` is a layer a stage can open. */
export function isLayer(name: string): boolean {
  const lower = name.toLowerCase();
  return LAYER_EXTENSIONS.some((extension) => lower.endsWith(extension));
}

/**
 * Whether `identifier` is a search path: neither absolute, nor relative to
 * a layer (`./`, `../`), nor in a package, nor a URL. OpenUSD keeps such a
 * path as authored and leaves it to the resolver.
 */
function isSearchPath(identifier: string): boolean {
  return !/^(\/|\.\.?\/|[A-Za-z][A-Za-z0-9+.-]*:)/.test(identifier) && !identifier.includes("[");
}

/**
 * Where to read `identifier`, which the layer `anchor` authored (its
 * identifier), as OpenUSD's default resolver finds a search path
 * (`ArDefaultResolver::_CreateIdentifier`, then `_Resolve`): beside the
 * authoring layer when it exists there, else as written. Any other
 * identifier is already anchored, and read as it is.
 */
export async function resolveAsset(source: SceneSource, identifier: string, anchor: string | null): Promise<string> {
  if (anchor === null || !isSearchPath(identifier)) return identifier;
  const slash = anchor.lastIndexOf("/");
  if (slash < 0) return identifier;
  const beside = `${anchor.slice(0, slash + 1)}${identifier}`;
  return (await source.exists(beside)) ? beside : identifier;
}

/** What {@link readAsset} asks of the stage: a member of a package it holds. */
export type PackagedAssets = Pick<Stage, "packageMember">;

/**
 * The package members a search path (`textures/wood.png`) authored in a
 * USDZ package names, in the order OpenUSD looks: beside the authoring
 * member, then beside the package's root layer at the archive's top level
 * (`SdfComputeAssetPathRelativeToLayer`; `Sdf_ComputeAnchorPath` in
 * `pxr/usd/sdf/layerUtils.cpp`). `anchor` is the authoring layer's
 * identifier: a member (`set/model.usdz[geo/rock.usda]`) or the package
 * read as a layer (`set/model.usdz`). Empty for any other path or layer:
 * it is not looked for in a package.
 */
export function packagedSearch(identifier: string, anchor: string | null): string[] {
  if (anchor === null || !isSearchPath(identifier)) return [];
  const open = anchor.indexOf("[");
  if (open < 0) return /.\.usdz$/i.test(anchor.slice(anchor.lastIndexOf("/") + 1)) ? [`${anchor}[${identifier}]`] : [];
  if (!anchor.endsWith("]")) return [];
  const outer = anchor.slice(0, open);
  const member = anchor.slice(open + 1, -1);
  // A member of a package inside the package: only the outer root is looked in.
  if (member.includes("[")) return [`${outer}[${identifier}]`];
  const slash = member.lastIndexOf("/");
  const beside = slash < 0 ? identifier : `${member.slice(0, slash + 1)}${identifier}`;
  return [...new Set([`${outer}[${beside}]`, `${outer}[${identifier}]`])];
}

/** The bytes of the first package member of `identifiers` the stage holds. */
function packaged(stage: PackagedAssets | null, identifiers: readonly string[]): Uint8Array | null {
  if (stage === null) return null;
  for (const identifier of identifiers) {
    const bytes = stage.packageMember(identifier);
    if (bytes !== null) return bytes;
  }
  return null;
}

/**
 * An asset's bytes, as the viewport and the views read a texture: a member
 * of a USDZ package the stage read (`set/model.usdz[wood.png]`, as a path
 * authored inside the package is anchored, or a search path found in the
 * package, {@link packagedSearch}) from the stage, which holds the
 * package's bytes, the source having supplied it as one asset; anything
 * else from `source`, found as {@link resolveAsset} finds it.
 */
export async function readAsset(source: SceneSource, stage: PackagedAssets | null, identifier: string, anchor: string | null): Promise<Uint8Array | null> {
  const bytes = packaged(stage, [identifier, ...packagedSearch(identifier, anchor)]);
  if (bytes !== null) return bytes;
  return source.fetch(await resolveAsset(source, identifier, anchor));
}

/** Whether {@link readAsset} would find the asset: a UDIM set's tiles are asked one by one. */
export async function assetExists(source: SceneSource, stage: PackagedAssets | null, identifier: string, anchor: string | null): Promise<boolean> {
  if (packaged(stage, [identifier, ...packagedSearch(identifier, anchor)]) !== null) return true;
  return source.exists(await resolveAsset(source, identifier, anchor));
}
