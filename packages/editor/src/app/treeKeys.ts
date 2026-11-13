// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The keys of a focused prim row that are the editor's, decided apart from
// React and the DOM so the rules are tested in Node (`keys.ts` has the
// global shortcuts' side).

/** The modifiers and key of a keyboard event. */
export interface KeyPress {
  readonly key: string;
  readonly metaKey: boolean;
  readonly ctrlKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;
}

/** What a key on a focused prim row does, when it is the editor's. */
export type RowAction = "frame" | "delete" | "rename";

/**
 * The action of `press` on a focused tree row, or `null` to leave it to the
 * tree. F frames unless a type-to-find search is under way (`searching`:
 * the tree's typeahead takes a letter that continues one, and a letter that
 * starts one that matches takes it from the shortcut, so the row decides
 * first); Delete (or ⌘⌫) deletes; F2 and Enter rename.
 */
export function rowAction(press: KeyPress, searching: boolean): RowAction | null {
  // A Mac keyboard has no Delete key: ⌘⌫ deletes there, as in the Finder.
  if (press.key === "Backspace" && press.metaKey && !press.ctrlKey && !press.altKey && !press.shiftKey) return "delete";
  if (press.metaKey || press.ctrlKey || press.altKey) return null;
  if (press.key === "f" && !press.shiftKey) return searching ? null : "frame";
  if (press.shiftKey) return null;
  if (press.key === "Delete") return "delete";
  if (press.key === "F2" || press.key === "Enter") return "rename";
  return null;
}

/** How long the tree's type-to-find search lasts after a letter, in milliseconds (react-aria's). */
export const TYPEAHEAD_MS = 1000;
