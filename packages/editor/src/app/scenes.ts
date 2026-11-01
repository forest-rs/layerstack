// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The scenes the editor offers: the sample it ships, the scenes in a
// local directory the dev server serves (`LAYERSTACK_SCENES`, see
// `vite.config.ts`; never in a build), a directory the person picks, and
// files they drop.
import { directorySource, filesSource, httpSource, isLayer, type SceneSource } from "../model/sources.ts";

/** A scene the editor can open by name. */
export interface SceneEntry {
  /** What the editor calls it, and its `?scene=` name. */
  readonly label: string;
  /** Opens it. */
  readonly source: () => SceneSource;
}

/** The sample scene the editor ships (`public/scene`). */
export const SAMPLE: SceneEntry = {
  label: "Sample",
  source: () => httpSource("Sample", new URL("scene/", document.baseURI), "scene.usda"),
};

/** One root the dev server's scene directory has, as `/@scenes/index.json` lists it. */
interface LocalScene {
  /** The root's path under the scene directory (`ALab-2.3.0/ALab/entry.usda`). */
  readonly path: string;
}

/**
 * The local scene at `path` under the served directory. Its identifier is
 * that path, so an asset path climbing out of the root's own directory
 * still resolves.
 */
export function localScene(path: string): SceneSource {
  return httpSource(path, new URL("/@scenes/", document.baseURI), path);
}

/** The scenes the dev server serves from the local scene directory; none in a build. */
export async function localScenes(): Promise<SceneEntry[]> {
  if (!import.meta.env.DEV) return [];
  const response = await fetch("/@scenes/index.json");
  if (!response.ok) return [];
  const scenes = (await response.json()) as LocalScene[];
  return scenes.map(({ path }) => ({
    label: path,
    source: () => localScene(path),
  }));
}

/** The scene `?scene=` names (the sample by default, or a local scene's path). */
export async function initialSource(): Promise<SceneSource> {
  const wanted = new URLSearchParams(location.search).get("scene");
  if (wanted !== null && wanted !== SAMPLE.label) {
    const found = (await localScenes()).find((entry) => entry.label === wanted);
    if (found !== undefined) return found.source();
  }
  return SAMPLE.source();
}

/** The layers in `directory` a stage could open as its root, two levels deep, sorted. */
export async function directoryRoots(directory: FileSystemDirectoryHandle): Promise<string[]> {
  const roots: string[] = [];
  const walk = async (handle: FileSystemDirectoryHandle, prefix: string, depth: number) => {
    for await (const [name, entry] of handle.entries()) {
      if (entry.kind === "file" && isLayer(name)) roots.push(`${prefix}${name}`);
      else if (entry.kind === "directory" && depth > 0 && !name.startsWith(".")) {
        await walk(entry as FileSystemDirectoryHandle, `${prefix}${name}/`, depth - 1);
      }
    }
  };
  await walk(directory, "", 1);
  return roots.sort();
}

/** A scene in `directory` with its root at `root`. */
export function directoryScene(directory: FileSystemDirectoryHandle, root: string): SceneSource {
  return directorySource(directory, root);
}

/**
 * The files of a drop, by their paths in it: a dropped folder's files
 * under the folder's name, a dropped file under its own.
 */
export async function droppedFiles(transfer: DataTransfer): Promise<Map<string, File>> {
  const files = new Map<string, File>();
  const walk = async (entry: FileSystemEntry, prefix: string): Promise<void> => {
    if (entry.isFile) {
      const file = await new Promise<File>((resolve, reject) => (entry as FileSystemFileEntry).file(resolve, reject));
      files.set(`${prefix}${entry.name}`, file);
      return;
    }
    const reader = (entry as FileSystemDirectoryEntry).createReader();
    for (;;) {
      const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject));
      if (batch.length === 0) break;
      for (const child of batch) await walk(child, `${prefix}${entry.name}/`);
    }
  };
  const entries = [...transfer.items].map((item) => item.webkitGetAsEntry()).filter((entry) => entry !== null);
  for (const entry of entries) await walk(entry, "");
  return files;
}

/** The dropped files' scene with its root at `root`, a key of `files`. */
export function droppedScene(files: ReadonlyMap<string, File>, root: string): SceneSource {
  return filesSource(root, files, root);
}
