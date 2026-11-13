// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The built-in presentations that author a prim: the inspector's State,
// Material, Applied schemas and Metadata sections, and the commands on a
// prim path wherever one is shown (the tree's context menu, a target, a
// binding's material, the changes panel): bind a material, duplicate,
// move under another prim, add a child, rename and delete. Priorities as `prims.tsx` places them: 100 the
// prim's state, -8 its applied schemas (after its properties and their
// ops), -10 its material, -30 its metadata (after its composition).
import { namespaceRefusal } from "../../model/composition.ts";
import { childPath, freeSiblingName, nameOf, parentOf } from "../../model/primAuthoring.ts";
import { type PresentationContext, registerCommand, registerPresentation } from "../../presentations/registry.ts";
import { AppliedSchemasSection } from "../AppliedSchemasSection.tsx";
import { MaterialBindingSection, showsMaterialSection } from "../MaterialBindingSection.tsx";
import { duplicatePrim } from "../PrimDialogs.tsx";
import { askPrim } from "../primDialogRequests.ts";
import { PrimMetadataSection } from "../PrimMetadataSection.tsx";
import { PrimStateSection } from "../PrimStateSection.tsx";

/** Whether a prim is at `path` that edits may change: not the pseudo-root, nor an instance proxy. */
function editable(path: string, { stage }: PresentationContext): boolean {
  if (path === "/") return false;
  try {
    const prim = stage.prim(path);
    return prim !== null && !prim.instanceProxy;
  } catch {
    return false;
  }
}

/** Where a duplicate of the prim at `path` would go: a free sibling name. */
function copyPath(path: string, { stage }: PresentationContext): string {
  const parent = parentOf(path);
  return childPath(parent, freeSiblingName(nameOf(path), new Set(stage.children(parent).map(nameOf))));
}

/** Registers the prim-authoring panels and commands. */
export function registerPrimAuthoringPresentations(): void {
  registerPresentation({
    id: "builtin.state",
    kind: "prim",
    type: "Prim",
    title: "State",
    priority: 100,
    panel: ({ object, context }) => <PrimStateSection store={context.store} prim={object.prim} />,
  });
  registerPresentation({
    id: "builtin.applied-schemas",
    kind: "prim",
    type: "Prim",
    title: "Applied schemas",
    priority: -8,
    applies: ({ prim }) => prim.path !== "/",
    panel: ({ object, context }) => <AppliedSchemasSection store={context.store} prim={object.prim} />,
  });
  registerPresentation({
    id: "builtin.bound-material",
    kind: "prim",
    type: "Prim",
    title: "Material",
    priority: -10,
    applies: ({ prim }) => prim.path !== "/" && showsMaterialSection(prim),
    panel: ({ object, context }) => <MaterialBindingSection store={context.store} stage={context.stage} prim={object.prim} />,
  });
  registerPresentation({
    id: "builtin.metadata",
    kind: "prim",
    type: "Prim",
    title: "Metadata",
    priority: -30,
    applies: ({ prim }) => prim.path !== "/",
    panel: ({ object, context }) => <PrimMetadataSection store={context.store} prim={object.prim} />,
  });

  registerCommand({
    id: "builtin.prim-path.bind-material",
    kind: "primPath",
    type: "primPath",
    title: "Bind a material…",
    applies: (object, context) => editable(object.path, context),
    run: (object) => askPrim({ kind: "bind", path: object.path }),
  });
  registerCommand({
    id: "builtin.prim-path.duplicate",
    kind: "primPath",
    type: "primPath",
    title: "Duplicate",
    label: (object) => `Duplicate ${object.path.slice(object.path.lastIndexOf("/") + 1)} beside itself`,
    // Offered where the copy can be made: through an arc or variant, only where the target maps it.
    applies: (object, context) => editable(object.path, context) && namespaceRefusal(context.store.session?.target, "define", copyPath(object.path, context)) === null,
    run: (object, { store }) => duplicatePrim(store, object.path),
  });
  registerCommand({
    id: "builtin.prim-path.move-to",
    kind: "primPath",
    type: "primPath",
    title: "Move to…",
    // A move edits the stage's own layer stack: not offered while edits go through an arc or variant.
    applies: (object, context) => editable(object.path, context) && namespaceRefusal(context.store.session?.target, "rename", object.path) === null,
    run: (object) => askPrim({ kind: "moveTo", path: object.path }),
  });
  // What the "⋯" menu of a prim also offers, so a context menu on its path lacks none of it.
  registerCommand({
    id: "builtin.prim-path.add-child",
    kind: "primPath",
    type: "primPath",
    title: "Add a child prim…",
    applies: (object, context) => editable(object.path, context),
    run: (object) => askPrim({ kind: "add", parent: object.path }),
  });
  registerCommand({
    id: "builtin.prim-path.rename",
    kind: "primPath",
    type: "primPath",
    title: "Rename…",
    applies: (object, context) => editable(object.path, context) && namespaceRefusal(context.store.session?.target, "rename", object.path) === null,
    run: (object) => askPrim({ kind: "rename", path: object.path }),
  });
  registerCommand({
    id: "builtin.prim-path.delete",
    kind: "primPath",
    type: "primPath",
    title: "Delete",
    applies: (object, context) => editable(object.path, context) && namespaceRefusal(context.store.session?.target, "rename", object.path) === null,
    run: (object, { store }) => void store.delete(object.path),
  });
}
