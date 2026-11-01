// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where a scene's bytes come from. The stage names each asset by an
// identifier relative to the root's directory (`assets/robot.usda`,
// `../textures/wood.png`); a source answers it from HTTP, a directory the
// person picked, or files they dropped. The stage never reads storage
// itself: the session's asset loop asks, and the source answers.
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
  const fetcher: Fetcher = async (identifier) => {
    const response = await fetch(new URL(identifier, base));
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
    async fetch(identifier) {
      const found = await file(identifier);
      return found === null ? null : new Uint8Array(await found.arrayBuffer());
    },
    async exists(identifier) {
      return (await file(identifier)) !== null;
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
    async fetch(identifier) {
      const found = file(identifier);
      return found === undefined ? null : new Uint8Array(await found.arrayBuffer());
    },
    async exists(identifier) {
      return file(identifier) !== undefined;
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
