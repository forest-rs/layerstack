// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The actions on a prim, as one list both menus show: the prim's "⋯"
// (`../app/PrimActions.tsx`) and the context menu of its path wherever it
// is shown (the commands `../app/presentations/primAuthoring.tsx`
// registers from this list). Each has one wording, and one that cannot run
// where edits go now is listed disabled with why, said before it is
// tried: an instance proxy is read-only, a rename, delete or move edits
// the stage's own layer stack (`namespaceRefusal`), a duplicate's copy
// must be one the target maps and not inside a variant (`copyRefusal`), a move among siblings is authored on the
// parent, which the target must map, and an inactive prim loads no payloads. (A
// sibling at its end is disabled for Move up or down with nothing said:
// the rows show it.)
import type { Stage } from "@forest-rs/layerstack";
import { mapping, mapThrough, namespaceRefusal } from "./composition.ts";
import { copyPath, copyRefusal, nameOf, parentOf } from "./primAuthoring.ts";
import type { Target } from "./propertyOps.ts";

/** An action on a prim. */
export type PrimActionId = "add" | "duplicate" | "moveTo" | "moveUp" | "moveDown" | "bind" | "load" | "loadOnly" | "unload" | "rename" | "delete";

/** Which part of the menu an action is in: a separator falls between parts. */
export type PrimActionGroup = "edit" | "payloads" | "namespace";

/** One action as a menu lists it. */
export interface PrimAction {
  readonly id: PrimActionId;
  /** Its label in either menu; an ellipsis when it asks something first. */
  readonly title: string;
  readonly group: PrimActionGroup;
  /** Whether it can run now; a disabled one is listed all the same. */
  readonly enabled: boolean;
  /** Why it cannot run now, said under the menu's items; `null` when it can, or when its place says so (a first sibling's Move up). */
  readonly refusal: string | null;
  /** Whether it takes something away (Delete). */
  readonly destructive: boolean;
}

/** Each action's one wording, in the order both menus list them. */
export const PRIM_ACTION_TITLES: Readonly<Record<PrimActionId, string>> = {
  add: "Add a child prim…",
  duplicate: "Duplicate",
  moveTo: "Move to…",
  moveUp: "Move up",
  moveDown: "Move down",
  bind: "Bind a material…",
  load: "Load payloads",
  loadOnly: "Load payloads, not beneath",
  unload: "Unload payloads",
  rename: "Rename…",
  delete: "Delete",
};

/** Why an instance proxy takes no edit. */
export const PROXY_REFUSAL = "An instance proxy is read-only: edit its prototype's source, or the instance.";

/**
 * The actions on the prim at `path` (none when there is no prim), in menu
 * order, each with why it cannot run where edits go now (`target`). The
 * pseudo-root takes a child and its payloads' loads alone. Payload loads
 * are listed when the prim has payloads, or any beneath it, or load rules
 * are set (as usdview offers them on any prim); not on an instance proxy,
 * whose payloads load through its instance.
 */
export function primActions(stage: Stage, path: string, target: Target): PrimAction[] {
  const prim = stage.prim(path);
  if (prim === null) return [];
  const proxy = prim.instanceProxy ? PROXY_REFUSAL : null;
  const actions: PrimAction[] = [];
  const add = (id: PrimActionId, group: PrimActionGroup, refusal: string | null, enabled = refusal === null) =>
    actions.push({ id, title: PRIM_ACTION_TITLES[id], group, enabled, refusal, destructive: id === "delete" });
  add("add", "edit", proxy);
  const root = path === "/";
  let namespace: string | null = null;
  if (!root) {
    const parent = parentOf(path);
    const siblings = stage.children(parent).map(nameOf);
    const at = siblings.indexOf(nameOf(path));
    namespace = namespaceRefusal(target, "rename", path);
    const copy = copyRefusal(target, copyPath(stage, path));
    add("duplicate", "edit", proxy ?? copy);
    add("moveTo", "edit", proxy ?? namespace);
    // A place among siblings is the parent's child order, authored on the parent: the target must map it.
    const order = proxy ?? (target !== undefined && "kind" in target && mapThrough(target, parent) === null ? `${mapping(target, parent).text}: its children's order is authored on it.` : null);
    add("moveUp", "edit", order, order === null && at > 0);
    add("moveDown", "edit", order, order === null && at >= 0 && at < siblings.length - 1);
    add("bind", "edit", proxy);
  }
  const loadable = proxy === null && (prim.hasAuthoredPayloads || stage.findLoadable(path).length > 0 || stage.loadRules().length > 0);
  if (loadable) {
    const inactive = prim.active ? null : `${root ? "The stage" : nameOf(path)} is inactive: its payloads load once it is active.`;
    add("load", "payloads", inactive);
    add("loadOnly", "payloads", inactive);
    add("unload", "payloads", null);
  }
  if (!root) {
    add("rename", "namespace", proxy ?? namespace);
    add("delete", "namespace", proxy ?? namespace);
  }
  return actions;
}

/** The reasons of the refused `actions`, each once, in order: what a menu says under its items. */
export function refusals(actions: readonly PrimAction[]): string[] {
  return [...new Set(actions.flatMap((action) => (action.refusal === null ? [] : [action.refusal])))];
}
