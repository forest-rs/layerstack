// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// How often the editor's panels render, counted: a panel that renders when
// nothing it shows changed is work the page did for nothing, and on a
// production stage one render of everything is tens of milliseconds. Each
// panel counts itself as it renders; a measurement clears the counts, does
// something (plays a second of the timeline) and reads them. In
// development a module is one instance per URL, so the page's console, or
// a script driving it, reads them with
// `(await import("/src/app/renders.ts")).renderCounts()`.

const counts = new Map<string, number>();

/**
 * Counts one render of the panel `name` (or one of whatever a panel does
 * per render that is worth counting: `PropertyRow` is one attribute read).
 * React's strict mode renders twice in development, and counts twice.
 */
export function countRender(name: string): void {
  counts.set(name, (counts.get(name) ?? 0) + 1);
}

/** The renders counted since the last {@link clearRenderCounts}, by name. */
export function renderCounts(): Readonly<Record<string, number>> {
  return Object.fromEntries([...counts].sort(([a], [b]) => (a < b ? -1 : 1)));
}

/** Forgets every count. */
export function clearRenderCounts(): void {
  counts.clear();
}
