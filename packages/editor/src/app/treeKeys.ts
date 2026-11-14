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
export type RowAction = "frame" | "delete" | "rename" | "menu" | "deselect";

/**
 * The action of `press` on a focused tree row, or `null` to leave it to the
 * tree. F frames unless a type-to-find search is under way (`searching`:
 * the tree's typeahead takes a letter that continues one, and a letter that
 * starts one that matches takes it from the shortcut, so the row decides
 * first); Delete (or ⌘⌫) deletes; F2 and Enter rename; Shift+F10 (or the
 * Menu key) opens the row's context menu, which a browser on macOS does
 * not do for it; Escape clears the selection.
 */
export function rowAction(press: KeyPress, searching: boolean): RowAction | null {
  // A Mac keyboard has no Delete key: ⌘⌫ deletes there, as in the Finder.
  if (press.key === "Backspace" && press.metaKey && !press.ctrlKey && !press.altKey && !press.shiftKey) return "delete";
  if (press.metaKey || press.ctrlKey || press.altKey) return null;
  if ((press.key === "F10" && press.shiftKey) || press.key === "ContextMenu") return "menu";
  if (press.key === "f" && !press.shiftKey) return searching ? null : "frame";
  if (press.shiftKey) return null;
  // The tree keeps a selection once it has one (a click on the selected row
  // does not clear it), so it does not clear it on Escape either: the row does.
  if (press.key === "Escape") return "deselect";
  if (press.key === "Delete") return "delete";
  if (press.key === "F2" || press.key === "Enter") return "rename";
  return null;
}

/**
 * The row that takes the focus when the row at `removed` goes with its
 * descendants: the next row after them, else the one before it; `null`
 * when there is no other row. `paths` are the rows as the tree lists them.
 */
export function rowAfterRemoval(paths: readonly string[], removed: string): string | null {
  const at = paths.indexOf(removed);
  if (at < 0) return null;
  for (let index = at + 1; index < paths.length; index++) {
    if (!paths[index]!.startsWith(`${removed}/`)) return paths[index]!;
  }
  return at > 0 ? paths[at - 1]! : null;
}

/**
 * What the tree says under its rows when a drag or a key was refused:
 * `move` while a prim is dragged that cannot go to another parent, and
 * `delete` after Delete on a row was refused (`why`, the store's
 * refusal), so a refusal is said where the rows are, not only as a
 * notice in another tab.
 */
export function treeRefusal(action: "move" | "delete", why: string): string {
  return `${action === "move" ? "Not moved to another parent" : "Not deleted"}: ${why}`;
}

/** How long the tree's type-to-find search lasts after a letter, in milliseconds (react-aria's). */
export const TYPEAHEAD_MS = 1000;
