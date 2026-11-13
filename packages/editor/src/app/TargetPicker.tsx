// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where edits go: the stage's edit layer, another layer of its own stack,
// or an edit target through an arc or inside a variant, chosen from an
// opinion (the explain panel's "Edit here") or the inspector's
// Composition and Variants sections, named by its arc and layer.
import type { EditTarget, Layer } from "@forest-rs/layerstack";
import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectTrigger, SelectValue } from "@/components/ui/select";
import { describeTarget } from "../model/composition.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { layerName } from "./format.ts";
import { countRender } from "./renders.ts";

const DEFAULT = "default";
const THROUGH = "through";

/**
 * A target in words: its layer, and how it is reached (inside which
 * variant, through which arc to which spec), as `describeTarget` says.
 */
export function targetText(target: Layer | EditTarget): string {
  return describeTarget(target);
}

export function TargetPicker() {
  countRender("TargetPicker");
  const store = useEditorStore();
  useRevision(store, "composition", "target");
  const session = store.session;
  if (session === null) return null;
  const stage = session.stage;
  const own = stage.layers().filter((layer) => stage.hasLocalLayer(layer) && !layer.readOnly);
  const editLayer = stage.editLayer;
  const target = session.target;
  const through = target !== undefined && "kind" in target ? target : null;
  // The edit layer is the picker's default item, so it is chosen as that.
  const defaultItem = (
    <SelectItem id={DEFAULT} textValue={`${editLayer === null ? "no edit layer" : layerName(editLayer.identifier)} (edit layer)`}>
      {editLayer === null ? "no edit layer" : layerName(editLayer.identifier)} <span className="text-muted-foreground">edit layer</span>
    </SelectItem>
  );
  const selected = target === undefined || (through === null && (target as Layer).id === editLayer?.id) ? DEFAULT : through !== null ? THROUGH : String((target as Layer).id);
  return (
    <Select
      aria-label="Where edits go"
      value={selected}
      onChange={(key) => {
        if (key === DEFAULT) store.setTarget(undefined);
        else if (key !== THROUGH) store.setTarget(own.find((layer) => String(layer.id) === key));
      }}
      // Wider while edits go through an arc or inside a variant, which takes more words to name.
      className={through === null ? "w-64" : "w-96"}
    >
      <SelectTrigger size="sm" className="text-muted-foreground">
        <span className="shrink-0">Edits go to</span>
        <SelectValue className="truncate text-foreground" />
      </SelectTrigger>
      <SelectContent>
        {session.sessionNote === null ? (
          defaultItem
        ) : (
          // Why a scene opened without asking for one edits in a session layer.
          <SelectGroup>
            <SelectLabel className="max-w-64">Edits go to a session layer in front of the root: {session.sessionNote}.</SelectLabel>
            {defaultItem}
          </SelectGroup>
        )}
        {own
          .filter((layer) => layer.id !== editLayer?.id)
          .map((layer) => (
            <SelectItem key={String(layer.id)} id={String(layer.id)} textValue={layerName(layer.identifier)}>
              {layerName(layer.identifier)}
            </SelectItem>
          ))}
        {through !== null && (
          <SelectItem id={THROUGH} textValue={targetText(through)}>
            {targetText(through)}
          </SelectItem>
        )}
      </SelectContent>
    </Select>
  );
}
