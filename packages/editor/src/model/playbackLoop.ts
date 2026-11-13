// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Whether playing the timeline loops at its end, as this viewer chose,
// remembered across visits (`localStorage`); it loops until chosen
// otherwise.
const LOOP_KEY = "layerstack-editor:loop-playback";

/** Whether this viewer last chose to loop; loops when it never chose, or storage is blocked. */
export function rememberedLoop(): boolean {
  try {
    return globalThis.localStorage?.getItem(LOOP_KEY) !== "stop";
  } catch {
    return true;
  }
}

/** Remembers the choice for this viewer, when storage is available. */
export function rememberLoop(loop: boolean): void {
  try {
    globalThis.localStorage?.setItem(LOOP_KEY, loop ? "loop" : "stop");
  } catch {
    // Storage blocked: the choice lasts this page.
  }
}
