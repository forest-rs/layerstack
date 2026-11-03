// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Per-prim revisions read from a session's change log: a number per prim
// that changes when, and only when, a recomposition reaches it. Views key
// their reads of a prim by it (the mesh view's buffers, a point instancer's
// columns), so an edit elsewhere reads none of them again. The log is read
// once per session for every prim asked about, each entry once.
import type { Changes } from "@forest-rs/layerstack";
import type { Session } from "./session.ts";

/**
 * Whether `paths` reach `path`: `path` itself, `/` (everything), a prim
 * above it (a resync of its subtree), or, with `below`, a prim or property
 * beneath it.
 */
export function reaches(path: string, paths: readonly string[], below: boolean): boolean {
  for (const changed of paths) {
    if (changed === path || changed === "/") return true;
    if (below && (changed.startsWith(`${path}/`) || changed.startsWith(`${path}.`))) return true;
    if (path.startsWith(changed) && (path[changed.length] === "/" || path[changed.length] === ".")) return true;
  }
  return false;
}

/** Whether `changes` reach the prim at `path`: it or its subtree resynced, created or removed, or it or something beneath it changed. */
export function changesReach(path: string, changes: Changes): boolean {
  return (
    reaches(path, changes.resynced, true) ||
    reaches(path, changes.created, true) ||
    reaches(path, changes.removed, true) ||
    reaches(path, changes.changedInfoOnly, true) ||
    reaches(path, changes.resyncedProperties, true) ||
    reaches(path, changes.changedProperties.map((change) => change.property), true)
  );
}

/** What a session's change log has been read to, and each prim's revision. */
interface Revisions {
  read: number;
  readonly prims: Map<string, number>;
}

const bySession = new WeakMap<Session, Revisions>();

/**
 * The revision of the prim at `path` in `session`: it changes when, and
 * only when, a recomposition after the first time it was asked about
 * reaches the prim ({@link changesReach}).
 */
export function primRevision(session: Session, path: string): number {
  let revisions = bySession.get(session);
  if (revisions === undefined) {
    revisions = { read: session.changeLog.length, prims: new Map() };
    bySession.set(session, revisions);
  }
  const log = session.changeLog;
  for (let at = revisions.read; at < log.length; at++) {
    const { changes } = log[at]!;
    for (const [prim, revision] of revisions.prims) if (changesReach(prim, changes)) revisions.prims.set(prim, revision + 1);
  }
  revisions.read = log.length;
  if (!revisions.prims.has(path)) revisions.prims.set(path, 0);
  return revisions.prims.get(path)!;
}
