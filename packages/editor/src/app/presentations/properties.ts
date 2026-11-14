// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The commands on a property path that author it where edits go: clear
// its value, block it, remove it, key it at the current frame, delete the
// key there. Each is offered wherever a property path is shown (the
// inspector's rows, the explain and changes panels) when it would change
// something (`model/propertyOps.ts`'s `propertyActions`), says what it
// authors and in which layer, and runs as one undoable edit through the
// store; a refusal is a notice.
import { type PropertyActions, propertyActions } from "../../model/propertyOps.ts";
import { formatTime } from "../format.ts";
import { type PresentationContext, registerCommand } from "../../presentations/registry.ts";
import { targetText } from "../TargetPicker.tsx";

/** The last answer of {@link actions}, and what it was asked: each command's `applies` asks the same once a menu opens. */
let last: { readonly key: readonly unknown[]; readonly actions: PropertyActions } | null = null;

/** What the operations would do to `path` now, where edits go and at the time (none when the stage has no range). */
function actions(path: string, { store, stage, time }: PresentationContext): PropertyActions {
  const target = store.session?.target;
  const at = store.range === null ? undefined : time;
  // The same stage, path, target, time and recomposition count read the same.
  const key = [stage, path, target, at, store.session?.changeLog.length];
  if (last !== null && last.key.every((part, index) => part === key[index])) return last.actions;
  last = { key, actions: propertyActions(stage, path, target, at) };
  return last.actions;
}

/** Where edits go, in words (`scene.usda`, `robot.usda at /Robot`). */
function where({ store, stage }: PresentationContext): string {
  const target = store.session?.target ?? stage.editLayer;
  return target === null ? "the edit layer" : targetText(target);
}

/** Registers the property path commands that author. */
export function registerPropertyCommands(): void {
  registerCommand({
    id: "builtin.property-path.clear",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Clear its value where edits go",
    label: (_object, context) => `Clear its value in ${where(context)} (weaker opinions show)`,
    applies: (object, context) => actions(object.path, context).clear,
    run: (object, { store }) => store.clearValue(object.path),
  });
  registerCommand({
    id: "builtin.property-path.block",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Block its value where edits go",
    label: (_object, context) => `Block its value in ${where(context)} (hides weaker opinions)`,
    applies: (object, context) => actions(object.path, context).block,
    run: (object, { store }) => store.blockValue(object.path),
  });
  registerCommand({
    id: "builtin.property-path.key",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Key at the current frame",
    label: (_object, context) => `Key at frame ${formatTime(context.time ?? 0)} in ${where(context)}`,
    applies: (object, context) => actions(object.path, context).key,
    run: (object, { store, time }) => {
      if (time !== undefined) store.key(object.path, time);
    },
  });
  registerCommand({
    id: "builtin.property-path.delete-key",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Delete the key at the current frame",
    label: (object, context) =>
      actions(object.path, context).lastKey
        ? `Delete the key at frame ${formatTime(context.time ?? 0)} in ${where(context)}: its only opinion there, so its spec stays empty (Remove takes it)`
        : `Delete the key at frame ${formatTime(context.time ?? 0)} in ${where(context)}`,
    applies: (object, context) => actions(object.path, context).deleteKey,
    run: (object, { store, time }) => {
      if (time !== undefined) store.deleteKey(object.path, time);
    },
  });
  registerCommand({
    id: "builtin.property-path.remove",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Remove it where edits go",
    label: (_object, context) => `Remove it from ${where(context)}`,
    applies: (object, context) => actions(object.path, context).remove,
    run: (object, { store }) => store.removeProperty(object.path),
  });
}
