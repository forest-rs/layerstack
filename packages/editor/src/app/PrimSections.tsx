// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's sections every prim has: its state (active,
// instanceable) and its bound material. Each is a panel presentation of
// `Prim` (`presentations/prims.tsx`); its variant sets and the arcs it
// composes from are `VariantsSection.tsx` and `CompositionSection.tsx`.
import type { Prim, Stage } from "@forest-rs/layerstack";
import { Switch } from "@/components/ui/switch";
import { materialSummary } from "../model/materials.ts";
import type { EditorStore } from "../store.ts";
import { Section } from "./Section.tsx";

/** Active and instanceable: composed metadata, authored as edits. */
export function State({ store, prim }: { store: EditorStore; prim: Prim }) {
  const instanceable = prim.metadata("instanceable");
  const isInstanceable = instanceable?.kind === "scalar" && instanceable.value === true;
  const readOnly = prim.instanceProxy;
  return (
    <Section title="State">
      <div className="grid grid-cols-[1fr_auto] items-center gap-y-1.5 text-xs">
        <span>Active</span>
        <Switch
          size="sm"
          aria-label="Active"
          isDisabled={readOnly}
          isSelected={prim.active}
          onChange={(on) => store.apply(`${on ? "activate" : "deactivate"} ${prim.path}`, [{ type: "setActive", path: prim.path, active: on }])}
        />
        <span>Instanceable</span>
        <Switch
          size="sm"
          aria-label="Instanceable"
          isDisabled={readOnly}
          isSelected={isInstanceable}
          onChange={(on) => store.apply(`set ${prim.path} instanceable ${on}`, [{ type: "setInstanceable", path: prim.path, instanceable: on }])}
        />
      </div>
      {readOnly && <p className="mt-1.5 text-[11px] text-muted-foreground">An instance proxy is read-only: edit its prototype's source, or the instance.</p>}
    </Section>
  );
}

export function MaterialSection({ stage, prim }: { stage: Stage; prim: Prim }) {
  const summary = materialSummary(stage, prim.path);
  if (summary === null) return null;
  return (
    <Section title="Material">
      <div className="font-mono text-[11px]">{summary.material}</div>
      <div className="mb-1.5 text-[11px] text-muted-foreground">{summary.binding}</div>
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] gap-x-2 gap-y-0.5 text-[11px]">
        {summary.inputs.map((input) => (
          <div key={input.name} className="contents">
            <span className="truncate text-muted-foreground">{input.name}</span>
            <span className="truncate font-mono" title={input.value}>
              {input.value}
            </span>
          </div>
        ))}
      </div>
    </Section>
  );
}
