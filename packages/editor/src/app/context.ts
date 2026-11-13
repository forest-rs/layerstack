// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// How a panel reaches the editor and hears of its changes. The store is
// handed down once (`EditorContext`) and never re-renders anything by
// itself: a panel names the concerns it reads (`useRevision`), or what it
// derives from them (`useStoreSelector`), and renders again when those
// change. A panel that reads a value at the time asks for the time
// (`useTime`), saying whether the value can vary, so playing the timeline
// renders the animated rows and leaves the rest.
import { createContext, use, useCallback, useMemo, useSyncExternalStore } from "react";
import type { Stage } from "@forest-rs/layerstack";
import { primRevision } from "../model/revisions.ts";
import { readsMightVary, type TimedReads } from "../model/timeVarying.ts";
import type { EditorStore } from "../store.ts";
import type { Channel } from "../store/signals.ts";

/** The editor every panel reads and acts through. */
export const EditorContext = createContext<EditorStore | null>(null);

/** The editor. Reading it subscribes to nothing: see {@link useRevision} and {@link useStoreSelector}. */
export function useEditorStore(): EditorStore {
  const store = use(EditorContext);
  if (store === null) throw new Error("useEditorStore outside EditorContext");
  return store;
}

/** A stable `subscribe` for `useSyncExternalStore` over `channels`: their names decide, not the array's identity. */
function useSubscribe(store: EditorStore, channels: readonly Channel[]): (listener: () => void) => () => void {
  const key = channels.join(" ");
  return useCallback((listener: () => void) => store.signals.subscribe(key.split(" ") as Channel[], listener), [store, key]);
}

/**
 * Renders the caller again after any of `channels` is bumped
 * (`../store/signals.ts` says what each covers), and returns a number that
 * changes with them: what a `useMemo` over the store's state is keyed by.
 */
export function useRevision(store: EditorStore, ...channels: readonly Channel[]): number {
  return useSyncExternalStore(useSubscribe(store, channels), () => store.signals.sum(channels));
}

/**
 * `select(store)`, read again after any of `channels` is bumped; the
 * caller renders again only when the result is another value
 * (`Object.is`), so `select` returns a primitive or an object the store
 * keeps, not one it builds.
 */
export function useStoreSelector<T>(store: EditorStore, channels: readonly Channel[], select: (store: EditorStore) => T): T {
  return useSyncExternalStore(useSubscribe(store, channels), () => select(store));
}

const TIME = ["time"] as const;
const COMPOSITION = ["composition"] as const;

/**
 * The time values are read at. The caller renders again as time moves
 * when `varying` (the default); a caller whose value cannot vary with
 * time (an attribute with no samples) passes `false`, reads the same
 * time, and is left alone while the timeline plays.
 */
export function useTime(store: EditorStore, varying = true): number | undefined {
  useSyncExternalStore(useSubscribe(store, TIME), () => (varying ? store.time : undefined));
  return store.time;
}

/**
 * The time a view of the prim at `path` reads `reads` at ({@link useTime}),
 * rendering it again as time moves only when one of them might vary
 * ({@link readsMightVary}), asked again after each recomposition the view
 * renders for: a light, mesh or material that holds still is left alone
 * while the timeline plays.
 */
export function useTimeOf(store: EditorStore, stage: Stage, path: string, reads: TimedReads): number | undefined {
  const revision = usePrimRevision(store, path);
  const recompositions = store.session?.changeLog.length ?? 0;
  const key = JSON.stringify(reads);
  // `reads` is keyed by its contents: callers pass a literal.
  const varying = useMemo(() => readsMightVary(stage, path, reads), [stage, path, revision, recompositions, key]);
  return useTime(store, varying);
}

/**
 * A number that changes when, and only when, a recomposition reaches the
 * prim at `path` ({@link primRevision}), rendering the caller again when
 * it does: what a panel keys its reads of the prim by (`useMemo`), so an
 * edit elsewhere reads nothing of it again.
 */
export function usePrimRevision(store: EditorStore, path: string): number {
  return useStoreSelector(store, COMPOSITION, (at) => (at.session === null ? 0 : primRevision(at.session, path)));
}
