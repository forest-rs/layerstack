// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Composition commands on presented objects, wherever they are shown: a
// layer (in the Layers tab, an arc's row, the explain panel) can be
// referenced from the selected prim, or added as its payload, the arc
// authored where edits go to the layer's default prim, its asset path
// relative to the layer edits go to; the store fetches what it names.
import { relativeAsset } from "../../model/composition.ts";
import { type PresentationContext, registerCommand } from "../../presentations/registry.ts";
import { compositionBlock, currentTarget } from "../CompositionSection.tsx";

/** The selected prim, when it can take an arc where edits go. */
function selectedPrim({ store, stage }: PresentationContext): string | null {
  const path = store.selection;
  const prim = path === null ? null : stage.prim(path);
  return prim === null || compositionBlock(store, prim) !== null ? null : prim.path;
}

/** The asset path of `layer` from the layer edits go to; `null` for a layer that has no identifier, or is that layer. */
function assetFrom(context: PresentationContext, identifier: string | null): string | null {
  const target = currentTarget(context.store);
  const from = target === null ? null : "kind" in target ? target.layer : target;
  if (identifier === null || from === null || from.identifier === identifier) return null;
  return relativeAsset(from.identifier, identifier);
}

/** Registers the composition commands. */
export function registerCompositionCommands(): void {
  for (const kind of ["reference", "payload"] as const) {
    registerCommand({
      id: `builtin.layer.${kind}-from-selection`,
      kind: "layer",
      type: "layer",
      title: kind === "reference" ? "Reference it from the selected prim" : "Add it as the selected prim's payload",
      label: (object, context) =>
        `${kind === "reference" ? "Reference it from" : "Add it as a payload of"} ${context.store.selection} (${assetFrom(context, object.layer.identifier)})`,
      applies: (object, context) => selectedPrim(context) !== null && assetFrom(context, object.layer.identifier) !== null && !object.layer.identifier!.includes("["),
      run: (object, context) => {
        const prim = selectedPrim(context);
        const asset = assetFrom(context, object.layer.identifier);
        if (prim === null || asset === null) return;
        void context.store.addArc(prim, { kind, asset, primPath: null, position: "backOfPrepend", offset: 0, scale: 1 }).done;
      },
    });
  }
}
