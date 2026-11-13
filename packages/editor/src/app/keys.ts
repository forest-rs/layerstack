// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which keys belong to the widget that has focus, and which are the
// editor's. The global shortcuts (`App.tsx`) step aside for text entry and
// for a key a focused widget already means something by; an element is read
// through the few fields it needs, apart from React and the DOM, so the
// rules are tested in Node.

/** The parts of an element the rules read; a DOM `Element` has them. */
export interface KeyTarget {
  readonly tagName: string;
  readonly type?: string;
  readonly isContentEditable?: boolean;
  /** The nearest ancestor (or the element itself) matching `selector`. */
  closest(selector: string): KeyTarget | null;
}

/** The input types that take no text: a hidden input behind a switch or a slider is one. */
const NON_TEXT = new Set(["checkbox", "radio", "range", "button", "submit", "reset", "color", "file", "image", "hidden"]);

/** Whether typing in `target` enters text: a text-like input, a textarea, a contenteditable. */
export function isTextEntry(target: KeyTarget | null): boolean {
  if (target === null) return false;
  if (target.isContentEditable === true || target.closest("[contenteditable]:not([contenteditable=false])") !== null) return true;
  const tag = target.tagName.toLowerCase();
  if (tag === "textarea") return true;
  return tag === "input" && !NON_TEXT.has((target.type ?? "text").toLowerCase());
}

/** What presses with Space: a focused one of these means "press" by it, not "play". */
const PRESSABLE =
  'button, a[href], summary, select, [role="button"], [role="checkbox"], [role="switch"], [role="radio"], [role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"], [role="option"], [role="tab"], [role="treeitem"], [role="row"], [role="gridcell"], [role="link"], [role="combobox"], input[type="checkbox"], input[type="radio"], input[type="button"], input[type="submit"]';

/** What moves with the arrow keys: a focused one inside one of these keeps Left and Right. */
const ARROWED =
  'select, [role="tree"], [role="treegrid"], [role="grid"], [role="listbox"], [role="menu"], [role="menubar"], [role="tablist"], [role="radiogroup"], [role="slider"], [role="combobox"], [role="spinbutton"], input[type="range"], input[type="radio"]';

/**
 * Whether a global shortcut for `key` leaves it to `target`: text entry
 * keeps every key, a pressable keeps Space, and what moves with the arrows
 * keeps Left and Right.
 */
export function widgetKeeps(target: KeyTarget | null, key: string): boolean {
  if (target === null) return false;
  if (isTextEntry(target)) return true;
  if (key === " ") return target.closest(PRESSABLE) !== null;
  if (key === "ArrowLeft" || key === "ArrowRight") return target.closest(ARROWED) !== null;
  return false;
}
