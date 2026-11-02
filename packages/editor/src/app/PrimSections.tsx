// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's sections every prim has: its state (active,
// instanceable), its variant sets, its bound material and the arcs it
// composes from. Each is a panel presentation of `Prim`
// (`presentations/prims.tsx`).
import type { Prim, Stage } from "@forest-rs/layerstack";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { materialSummary } from "../model/materials.ts";
import type { EditorStore } from "../store.ts";
import { layerName } from "./format.ts";
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

export function Variants({ store, prim }: { store: EditorStore; prim: Prim }) {
  const sets = prim.variantSets();
  if (sets.length === 0) return null;
  return (
    <Section title="Variants">
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-x-2 gap-y-1.5 text-xs">
        {sets.map((set) => (
          <VariantRow key={set.name} store={store} prim={prim} name={set.name} selection={set.selection} variants={set.variants} />
        ))}
      </div>
    </Section>
  );
}

function VariantRow({ store, prim, name, selection, variants }: { store: EditorStore; prim: Prim; name: string; selection: string | null; variants: readonly string[] }) {
  return (
    <>
      <span className="truncate">{name}</span>
      <Select
        aria-label={name}
        value={selection}
        placeholder="none selected"
        onChange={(key) => key !== null && key !== selection && void store.selectVariant(prim.path, name, String(key))}
        className="w-full"
      >
        <SelectTrigger size="sm">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {variants.map((variant) => (
            <SelectItem key={variant} id={variant} textValue={variant}>
              {variant}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </>
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

/** The prim's composition, strongest first: each arc's kind, the layer stack it reaches and the spec there. */
export function Arcs({ prim }: { prim: Prim }) {
  const arcs = prim.compositionArcs();
  return (
    <Section title="Composition">
      <ol className="flex flex-col gap-1">
        {arcs.map((arc, index) => (
          <li key={index} className="text-[11px]" style={{ paddingLeft: `${depth(arcs, index) * 10}px` }}>
            <span className="font-medium">{arc.arc}</span>
            {arc.implied && <span className="text-muted-foreground"> (implied)</span>}{" "}
            <span className="font-mono text-muted-foreground">
              {layerName(arc.layerStack.identifier)} {arc.site}
            </span>
          </li>
        ))}
      </ol>
    </Section>
  );
}

/** How many arcs `index`'s arc is beneath. */
function depth(arcs: readonly { readonly parent: number | null }[], index: number): number {
  let count = 0;
  for (let at = arcs[index]?.parent ?? null; at !== null; at = arcs[at]?.parent ?? null) count++;
  return count;
}
