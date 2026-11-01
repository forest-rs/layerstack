// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where edits go: the stage's edit layer, another layer of its own stack,
// or an edit target through an arc chosen from an opinion (the explain
// panel's "Edit here").
import type { EditTarget, Layer } from "@forest-rs/layerstack";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { useStore } from "./context.ts";
import { layerName } from "./format.ts";

const DEFAULT = "default";
const THROUGH = "through";

/** A target in words: its layer, and the spec it maps its prim to. */
export function targetText(target: Layer | EditTarget): string {
  if (!("kind" in target)) return layerName(target.identifier);
  const layer = layerName(target.layer.identifier);
  return target.site === null ? layer : `${layer} at ${target.site}`;
}

export function TargetPicker() {
  const store = useStore();
  const session = store.session;
  if (session === null) return null;
  const stage = session.stage;
  const own = stage.layers().filter((layer) => stage.hasLocalLayer(layer) && !layer.readOnly);
  const editLayer = stage.editLayer;
  const target = session.target;
  const through = target !== undefined && "kind" in target ? target : null;
  const selected = target === undefined ? DEFAULT : through !== null ? THROUGH : String((target as Layer).id);
  return (
    <Select
      aria-label="Where edits go"
      value={selected}
      onChange={(key) => {
        if (key === DEFAULT) store.setTarget(undefined);
        else if (key !== THROUGH) store.setTarget(own.find((layer) => String(layer.id) === key));
      }}
      className="w-64"
    >
      <SelectTrigger size="sm" className="text-muted-foreground">
        <span className="shrink-0">Edits go to</span>
        <SelectValue className="truncate text-foreground" />
      </SelectTrigger>
      <SelectContent>
        <SelectItem id={DEFAULT} textValue={`${layerName(editLayer?.identifier ?? null)} (edit layer)`}>
          {editLayer === null ? "no edit layer" : layerName(editLayer.identifier)} <span className="text-muted-foreground">edit layer</span>
        </SelectItem>
        {own
          .filter((layer) => layer.id !== editLayer?.id)
          .map((layer) => (
            <SelectItem key={String(layer.id)} id={String(layer.id)} textValue={layerName(layer.identifier)}>
              {layerName(layer.identifier)}
            </SelectItem>
          ))}
        {through !== null && (
          <SelectItem id={THROUGH} textValue={targetText(through)}>
            {targetText(through)} <span className="text-muted-foreground">through {through.arc}</span>
          </SelectItem>
        )}
      </SelectContent>
    </Select>
  );
}
