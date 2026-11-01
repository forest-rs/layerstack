// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector: the selected prim as the stage composes it. Its
// identity and state (type, specifier, active, instanceable), its
// variant sets, its properties (what layers author, then what its schemas
// define with their fallbacks), its bound material, and the arcs it
// composes from. Every editor authors through the store; a property's
// name opens its explanation in the bottom panel.
import { useState } from "react";
import type { Attribute, Prim, Stage } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { FocusPointIcon } from "@hugeicons/core-free-icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { materialSummary } from "../model/materials.ts";
import type { EditorStore } from "../store.ts";
import { useStore } from "./context.ts";
import { shown, ValueField } from "./fields.tsx";
import { layerName } from "./format.ts";
import { primIcon } from "./icons.ts";

export function Inspector() {
  const store = useStore();
  const stage = store.session?.stage ?? null;
  const prim = stage === null || store.selection === null ? null : stage.prim(store.selection);
  if (stage === null || prim === null) {
    return (
      <div className="flex h-full items-center justify-center p-6 text-center text-xs text-muted-foreground">
        Select a prim in the hierarchy or the viewport to inspect it.
      </div>
    );
  }
  return (
    <div className="flex h-full flex-col overflow-y-auto">
      <Header store={store} prim={prim} />
      <State store={store} prim={prim} />
      <Variants store={store} prim={prim} />
      <Properties store={store} stage={stage} prim={prim} />
      <MaterialSection stage={stage} prim={prim} />
      <Arcs prim={prim} />
    </div>
  );
}

function Section({ title, children, aside }: { title: string; children: React.ReactNode; aside?: React.ReactNode }) {
  return (
    <section className="border-b px-3 py-2">
      <h3 className="mb-1.5 flex items-center text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
        {title}
        <span className="ml-auto normal-case">{aside}</span>
      </h3>
      {children}
    </section>
  );
}

function Header({ store, prim }: { store: EditorStore; prim: Prim }) {
  const kind = prim.metadata("kind");
  return (
    <div className="border-b px-3 py-2.5">
      <div className="flex items-center gap-2">
        <HugeiconsIcon icon={primIcon(prim.typeName)} className="size-4 text-muted-foreground" />
        <span className="truncate text-sm font-semibold">{prim.name || "/"}</span>
        <TooltipTrigger>
          <Button variant="ghost" size="icon-xs" className="ml-auto" aria-label="Frame in the viewport" onPress={() => store.frameSelection()}>
            <HugeiconsIcon icon={FocusPointIcon} />
          </Button>
          <Tooltip>Frame in the viewport (F)</Tooltip>
        </TooltipTrigger>
      </div>
      <div className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground" title={prim.path}>
        {prim.path}
      </div>
      <div className="mt-1.5 flex flex-wrap gap-1">
        <Badge variant="secondary">{prim.typeName ?? "typeless"}</Badge>
        <Badge variant="outline">{prim.specifier}</Badge>
        {kind?.kind === "scalar" && <Badge variant="outline">kind {String(kind.value)}</Badge>}
        {prim.instance && <Badge variant="outline">instance</Badge>}
        {prim.instanceProxy && <Badge variant="outline">instance proxy</Badge>}
        {prim.abstract && <Badge variant="outline">abstract</Badge>}
      </div>
    </div>
  );
}

/** Active and instanceable: composed metadata, authored as edits. */
function State({ store, prim }: { store: EditorStore; prim: Prim }) {
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

function Variants({ store, prim }: { store: EditorStore; prim: Prim }) {
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

/**
 * The prim's attributes and relationships: those layers author, then, when
 * shown, those only its schemas define (each at its fallback). Names that
 * share a namespace (`primvars:`, `xformOp:`) stay together.
 */
function Properties({ store, stage, prim }: { store: EditorStore; stage: Stage; prim: Prim }) {
  const [schema, setSchema] = useState(false);
  const attributes = prim
    .attributeNames()
    .map((name) => prim.attribute(name))
    .filter((attribute) => attribute !== null);
  const authored = attributes.filter((attribute) => attribute.authored);
  const defined = attributes.filter((attribute) => !attribute.authored);
  const relationships = prim.relationshipNames();
  const varying = new Set(
    stage
      .valuesMightBeTimeVarying(authored.map((attribute) => attribute.path))
      .flatMap((varies, index) => (varies ? [authored[index]!.path] : [])),
  );
  return (
    <Section
      title="Properties"
      aside={
        defined.length > 0 && (
          <button type="button" onClick={() => setSchema(!schema)} className="text-[11px] text-muted-foreground hover:text-foreground">
            {schema ? "Hide" : "Show"} {defined.length} schema defaults
          </button>
        )
      }
    >
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-x-2 gap-y-1">
        {authored.map((attribute) => (
          <AttributeRow key={attribute.path} store={store} attribute={attribute} animated={varying.has(attribute.path)} />
        ))}
        {authored.length === 0 && <span className="col-span-2 text-[11px] text-muted-foreground">No layer authors a value here.</span>}
        {schema && defined.map((attribute) => <AttributeRow key={attribute.path} store={store} attribute={attribute} animated={false} />)}
        {relationships.map((name) => {
          const relationship = prim.relationship(name);
          if (relationship === null) return null;
          const targets = relationship.targets();
          return (
            <RelationshipRow key={name} name={name} targets={targets} onSelect={(path) => store.select(path.split(".")[0]!)} />
          );
        })}
      </div>
    </Section>
  );
}

function AttributeRow({ store, attribute, animated }: { store: EditorStore; attribute: Attribute; animated: boolean }) {
  const focused = store.property === attribute.path;
  const value = shown(attribute.get());
  const notes = [attribute.typeName ?? "untyped", attribute.variability === "uniform" ? "uniform" : null, attribute.custom ? "custom" : null, attribute.authored ? "authored" : "schema fallback"]
    .filter((note) => note !== null)
    .join(" · ");
  return (
    <>
      <button
        type="button"
        onClick={() => store.explain(attribute.path)}
        title={`${attribute.name}: ${notes}. Explain its value.`}
        className={cn("flex min-w-0 items-center gap-1.5 rounded px-1 py-0.5 text-left text-xs hover:bg-muted", focused && "bg-accent text-accent-foreground")}
      >
        <span className={cn("size-1.5 shrink-0 rounded-full", attribute.authored ? "bg-primary" : "border border-muted-foreground")} />
        <span className={cn("truncate", !attribute.authored && "text-muted-foreground")}>{attribute.name}</span>
        {animated && <span className="shrink-0 text-[10px] text-amber-500">animated</span>}
      </button>
      <div className="flex min-w-0 items-center">
        <ValueField store={store} attribute={attribute} shown={value} />
      </div>
    </>
  );
}

function RelationshipRow({ name, targets, onSelect }: { name: string; targets: readonly string[]; onSelect: (path: string) => void }) {
  return (
    <>
      <span className="flex min-w-0 items-center gap-1.5 px-1 py-0.5 text-xs">
        <span className="size-1.5 shrink-0 rotate-45 bg-muted-foreground" />
        <span className="truncate">{name}</span>
      </span>
      <div className="flex min-w-0 flex-col">
        {targets.length === 0 && <span className="text-[11px] text-muted-foreground">no targets</span>}
        {targets.map((target) => (
          <button key={target} type="button" onClick={() => onSelect(target)} className="truncate text-left font-mono text-[11px] text-sky-400 hover:underline" title={target}>
            {target}
          </button>
        ))}
      </div>
    </>
  );
}

function MaterialSection({ stage, prim }: { stage: Stage; prim: Prim }) {
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
function Arcs({ prim }: { prim: Prim }) {
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
