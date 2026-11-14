// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The built-in presentations that author a prim: the inspector's State,
// Material, Applied schemas and Metadata sections, and the commands on a
// prim path wherever one is shown (the tree's context menu, a target, a
// binding's material, the changes panel): the prim's actions, as its "⋯"
// menu lists them (`model/primActions.ts`). Priorities as `prims.tsx` places them: 100 the
// prim's state, -8 its applied schemas (after its properties and their
// ops), -10 its material, -30 its metadata (after its composition).
import { PRIM_ACTION_TITLES, type PrimAction, type PrimActionId, primActions } from "../../model/primActions.ts";
import { type PresentationContext, registerCommand, registerPresentation } from "../../presentations/registry.ts";
import { AppliedSchemasSection } from "../AppliedSchemasSection.tsx";
import { MaterialBindingSection, showsMaterialSection } from "../MaterialBindingSection.tsx";
import { runPrimAction } from "../PrimActions.tsx";
import { PrimMetadataSection } from "../PrimMetadataSection.tsx";
import { PrimStateSection } from "../PrimStateSection.tsx";

/** The last list {@link actionOf} read, and what it was asked: each command asks the same once a menu opens. */
let last: { readonly key: readonly unknown[]; readonly actions: readonly PrimAction[] } | null = null;

/** The action `id` on the prim at `path` now ({@link primActions}); `undefined` when it is not one of its actions. */
function actionOf(path: string, { store, stage }: PresentationContext, id: PrimActionId): PrimAction | undefined {
  const target = store.session?.target;
  // The same stage, path, target and recomposition count read the same.
  const key = [stage, path, target, store.session?.changeLog.length];
  if (last === null || !last.key.every((part, index) => part === key[index])) {
    let actions: readonly PrimAction[] = [];
    try {
      actions = primActions(stage, path, target);
    } catch {
      // Not a prim path the stage takes: nothing to offer.
    }
    last = { key, actions };
  }
  return last.actions.find((action) => action.id === id);
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

  // The prim's actions, as its "⋯" lists them (`model/primActions.ts`): one list, one wording, a refused one disabled with why.
  for (const id of Object.keys(PRIM_ACTION_TITLES) as PrimActionId[]) {
    registerCommand({
      id: `builtin.prim-path.${id}`,
      kind: "primPath",
      type: "primPath",
      title: PRIM_ACTION_TITLES[id],
      applies: (object, context) => actionOf(object.path, context, id) !== undefined,
      enabled: (object, context) => actionOf(object.path, context, id)?.enabled === true,
      refusal: (object, context) => actionOf(object.path, context, id)?.refusal ?? null,
      run: (object, { store }) => runPrimAction(store, object.path, id),
    });
  }
}
