// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where an edit went, when it does not show: the edit target holds the
// new opinion, but a stronger one still gives the value (or the target is
// a variant the stage does not select). Said beside the field that made
// the edit, with the winning opinion's layer live and "Edit there", which
// makes that opinion's edit target where edits go, as the explain panel's
// "Edit here" does. Read once by the store after the edit (`shadow`), not
// as the inspector renders.
import { Button } from "@/components/ui/button";
import type { Shadow } from "../model/propertyOps.ts";
import type { EditorStore } from "../store.ts";
import { formatTime } from "./format.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";
import { targetText } from "./TargetPicker.tsx";

/** The note under a field whose edit a stronger opinion hides. */
export function EditShadow({ store, shadow }: { store: EditorStore; shadow: Shadow }) {
  const context = presentationContext(store);
  const session = store.session;
  if (context === null || session === null) return null;
  const destination = session.target ?? context.stage.editLayer;
  const where = destination === null ? "the edit layer" : targetText(destination);
  const at = shadow.time === null ? "" : ` at frame ${formatTime(shadow.time)}`;
  const layer = <Present object={{ kind: "layer", layer: shadow.layer }} context={context} />;
  return (
    <div role="status" className="col-span-2 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 rounded-md bg-amber-500/10 px-2 py-1 text-[11px] text-amber-800 dark:text-amber-300">
      {shadow.composed ? (
        <span>
          The edit{at} is in {where}, but a stronger opinion wins: {layer} <span className="font-mono opacity-80">{shadow.specPath}</span>.
        </span>
      ) : (
        <span>
          The edit{at} is in {where}, which the stage does not compose now (a variant not selected, or an arc no longer there); the value comes from {layer}.
        </span>
      )}
      {shadow.editTarget !== null && (
        // A layer of the stage's own stack is chosen as that layer, as the toolbar's picker lists it.
        <Button variant="outline" size="xs" onPress={() => store.setTarget(shadow.editTarget!.kind === "layer" ? shadow.editTarget!.layer : shadow.editTarget!)}>
          Edit there
        </Button>
      )}
    </div>
  );
}
