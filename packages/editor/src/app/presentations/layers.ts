// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Commands on a layer wherever one is shown, besides the built-in ones
// (`objects.tsx`): read it again from the source, for a file changed on
// disk, refused while it has unsaved edits (the store says so in a
// notice). The Layers tab reloads every layer at once.
import { registerCommand } from "../../presentations/registry.ts";

/** Registers the layer stack's commands on layers. */
export function registerLayerCommands(): void {
  registerCommand({
    id: "builtin.layer.reload",
    kind: "layer",
    type: "layer",
    title: "Reload from source",
    label: (object, { store }) => (store.isEdited(object.layer) ? "Reload from source (save or undo its edits first)" : "Reload from source"),
    // The session layer has no source; a muted layer is not read.
    applies: (object, { stage }) => !object.layer.anonymous && !stage.isMuted(object.layer),
    run: (object, { store }) => {
      // A refusal (unsaved edits) is a notice; the Notices tab says why.
      void store.reload(object.layer);
    },
  });
}
