// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// How a model is drawn (`UsdGeomModelAPI`): as itself, or by its draw
// mode's stand-in (axes, box or textured cards) in place of everything
// beneath it. Shown for prims with `GeomModelAPI` applied, the only prims
// whose draw mode counts; authoring `model:drawMode` on one passes it down
// to the models beneath that apply it (components do unless they say not).
import type { Prim, Stage } from "@forest-rs/layerstack";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { DRAW_MODES, drawModeState } from "../model/drawModes.ts";
import type { EditorStore } from "../store.ts";
import { Section } from "./Section.tsx";

const LABELS: Record<string, string> = {
  inherited: "Inherited",
  default: "Itself",
  origin: "Origin axes",
  bounds: "Bounding box",
  cards: "Cards",
};

export function DrawModeSection({ store, stage, prim }: { store: EditorStore; stage: Stage; prim: Prim }) {
  if (!prim.hasAPI("GeomModelAPI")) return null;
  const state = drawModeState(stage, prim);
  const readOnly = prim.instanceProxy;
  const own = (DRAW_MODES as readonly string[]).includes(state.own) ? state.own : "inherited";
  const inheritedLabel = state.own === "inherited" ? `Inherited: ${LABELS[state.resolved] ?? state.resolved}${state.from === null ? "" : ` from ${state.from}`}` : "Inherited";
  const drawn = state.applies && ["origin", "bounds", "cards"].includes(state.resolved);
  const note = drawn
    ? "Drawn by its stand-in: nothing beneath it is drawn."
    : state.resolved !== "default" && !state.applies
      ? "It does not apply its draw mode: the models beneath that do are drawn by theirs."
      : null;
  return (
    <Section title="Draw mode">
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-x-2 gap-y-1.5 text-xs">
        <span>Draw as</span>
        <Select
          aria-label="Draw as"
          value={own}
          isDisabled={readOnly}
          onChange={(key) => key !== null && key !== own && store.setValue(`${prim.path}.model:drawMode`, { type: "token", value: String(key) })}
          className="w-full"
        >
          <SelectTrigger size="sm">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {DRAW_MODES.map((mode) => (
              <SelectItem key={mode} id={mode} textValue={mode === "inherited" ? inheritedLabel : LABELS[mode]!}>
                {mode === "inherited" ? inheritedLabel : LABELS[mode]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <span>Applies here</span>
        <Switch
          size="sm"
          aria-label="Applies here"
          isDisabled={readOnly}
          isSelected={state.applies}
          onChange={(on) => store.setValue(`${prim.path}.model:applyDrawMode`, { type: "bool", value: on })}
        />
      </div>
      {note !== null && <p className="mt-1.5 text-[11px] text-muted-foreground">{note}</p>}
    </Section>
  );
}
