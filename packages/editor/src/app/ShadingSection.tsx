// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's shading section, for a material or a shader: what it
// is (a shader's `info:id` and implementation; a material's outputs and
// what they read), a preview of the material on a sphere, and its inputs
// as their authors lay them out: grouped by `displayGroup`, labelled by
// `displayName`, ordered by `uiorder`, a number between its `uimin` and
// `uimax` a slider (over its soft range where given). A connected input
// is a link to its source.
import { useMemo, useState } from "react";
import type { Prim, ShaderPort, Stage } from "@forest-rs/layerstack";
import { Slider } from "@/components/ui/slider";
import { cn } from "@/lib/utils";
import { enclosingMaterial, primOf } from "../model/network.ts";
import { clampToBounds, groupInputs, type ShadingInput, type ShadingInputGroup, sliderRange } from "../model/shadingInputs.ts";
import { formatNumber } from "../model/values.ts";
import type { EditorStore } from "../store.ts";
import { CommitNumber, shown, ValueField } from "./fields.tsx";
import { MaterialPreview } from "./MaterialPreview.tsx";
import { Section } from "./Section.tsx";

export function ShadingSection({ store, stage, prim }: { store: EditorStore; stage: Stage; prim: Prim }) {
  const material = stage.material(prim.path);
  const shader = material === null ? stage.shader(prim.path) : null;
  if (material === null && shader === null) return null;
  const previewed = material !== null ? prim.path : enclosingMaterial(stage, prim.path);
  return (
    <Section title={material !== null ? "Material" : "Shader"}>
      {shader !== null && (
        <div className="mb-2 grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] gap-x-2 gap-y-0.5 text-[11px]">
          <span className="text-muted-foreground">info:id</span>
          <span className="truncate font-mono" title={shader.id ?? undefined}>
            {shader.id ?? "none"}
          </span>
          <span className="text-muted-foreground">implementation</span>
          <span className="font-mono">{shader.implementationSource}</span>
          {[...shader.sdrMetadata].map(([key, value]) => (
            <div key={key} className="contents">
              <span className="truncate text-muted-foreground">{key}</span>
              <span className="truncate font-mono">{value}</span>
            </div>
          ))}
        </div>
      )}
      {material !== null && <Terminals store={store} outputs={material.outputs()} baseMaterial={material.baseMaterial} />}
      {previewed !== null && <MaterialPreview store={store} stage={stage} material={previewed} />}
      <Inputs key={prim.path} store={store} stage={stage} path={prim.path} />
    </Section>
  );
}

/** A material's connected outputs (`surface`, `mtlx:surface`), each a link to what it reads. */
function Terminals({ store, outputs, baseMaterial }: { store: EditorStore; outputs: readonly ShaderPort[]; baseMaterial: string | null }) {
  const connected = outputs.filter((output) => output.connections.length > 0);
  return (
    <div className="mb-2 grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] gap-x-2 gap-y-0.5 text-[11px]">
      {connected.map((output) => (
        <div key={output.path} className="contents">
          <span className="truncate text-muted-foreground">outputs:{output.name}</span>
          <Sources store={store} sources={output.connections} />
        </div>
      ))}
      {connected.length === 0 && <span className="col-span-2 text-muted-foreground">No output is connected: the material has no surface.</span>}
      {baseMaterial !== null && (
        <>
          <span className="text-muted-foreground">base material</span>
          <SourceLink store={store} path={baseMaterial} text={baseMaterial} />
        </>
      )}
    </div>
  );
}

/** Links to the ports `sources` names: clicking one selects its prim and explains it. */
function Sources({ store, sources }: { store: EditorStore; sources: readonly string[] }) {
  return (
    <div className="flex min-w-0 flex-col">
      {sources.map((source) => {
        const prim = primOf(source);
        const name = source.slice(source.lastIndexOf(".") + 1).replace(/^outputs:/, "");
        return <SourceLink key={source} store={store} path={source} text={`← ${prim.slice(prim.lastIndexOf("/") + 1)}.${name}`} />;
      })}
    </div>
  );
}

function SourceLink({ store, path, text }: { store: EditorStore; path: string; text: string }) {
  const property = path.includes(".");
  return (
    <button
      type="button"
      title={path}
      onClick={() => {
        store.select(property ? primOf(path) : path);
        if (property) store.explain(path);
      }}
      className="truncate text-left font-mono text-[11px] text-sky-400 hover:underline"
    >
      {text}
    </button>
  );
}

/** The inputs by display group; a group opens and closes, and opens at first when this prim sets one of its inputs. */
function Inputs({ store, stage, path }: { store: EditorStore; stage: Stage; path: string }) {
  // Read again after a recomposition only: explaining every input is not free.
  const recompositions = store.session?.changeLog.length ?? 0;
  const groups = useMemo(
    () => groupInputs(stage, path, (stage.material(path) ?? stage.shader(path))?.inputs() ?? []),
    [stage, path, recompositions],
  );
  const [open, setOpen] = useState<ReadonlySet<string>>(() => initiallyOpen(groups));
  const paths = groups.flatMap((group) => group.inputs.map((input) => input.port.path));
  const varying = new Set(stage.valuesMightBeTimeVarying(paths).flatMap((varies, index) => (varies ? [paths[index]!] : [])));
  if (groups.length === 0) return <p className="text-[11px] text-muted-foreground">No inputs.</p>;
  // The legend, when some inputs are set here and some are not.
  const owned = groups.some((group) => group.inputs.some((input) => input.own)) && groups.some((group) => group.inputs.some((input) => !input.own));
  return (
    <div className="mt-2 flex flex-col gap-1">
      {owned && (
        <p className="text-[11px] text-muted-foreground">
          <span className="mr-1 inline-block size-1.5 rounded-full bg-primary align-middle" />
          set here; the rest come from what this prim inherits or references.
        </p>
      )}
      {groups.map((group) => (
        <InputGroup
          key={group.name}
          store={store}
          group={group}
          varying={varying}
          open={open.has(group.name) || groups.length === 1}
          toggle={() =>
            setOpen((current) => {
              const next = new Set(current);
              if (!next.delete(group.name)) next.add(group.name);
              return next;
            })
          }
        />
      ))}
    </div>
  );
}

/** The groups shown open at first: those with an input this prim sets, else the first. */
function initiallyOpen(groups: readonly ShadingInputGroup[]): ReadonlySet<string> {
  const owned = groups.filter((group) => group.inputs.some((input) => input.own)).map((group) => group.name);
  return new Set(owned.length > 0 ? owned : groups.slice(0, 1).map((group) => group.name));
}

function InputGroup({
  store,
  group,
  varying,
  open,
  toggle,
}: {
  store: EditorStore;
  group: ShadingInputGroup;
  varying: ReadonlySet<string>;
  open: boolean;
  toggle: () => void;
}) {
  const own = group.inputs.filter((input) => input.own).length;
  return (
    <div>
      <button type="button" onClick={toggle} aria-expanded={open} className="flex w-full items-center gap-1.5 rounded px-1 py-0.5 text-left text-xs font-medium hover:bg-muted">
        <span className={cn("inline-block text-[9px] text-muted-foreground transition-transform", open && "rotate-90")}>▶</span>
        {group.name}
        <span className="ml-auto text-[11px] font-normal text-muted-foreground">
          {own > 0 && `${own} set · `}
          {group.inputs.length}
        </span>
      </button>
      {open && (
        <div className="mt-0.5 mb-1 grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-x-2 gap-y-1">
          {group.inputs.map((input) => (
            <InputRow key={input.port.path} store={store} input={input} animated={varying.has(input.port.path)} />
          ))}
        </div>
      )}
    </div>
  );
}

function InputRow({ store, input, animated }: { store: EditorStore; input: ShadingInput; animated: boolean }) {
  const { port } = input;
  const focused = store.property === port.path;
  return (
    <>
      <button
        type="button"
        onClick={() => store.explain(port.path)}
        title={`inputs:${port.name}: ${port.typeName ?? "untyped"}. Explain its value.`}
        className={cn("flex min-w-0 items-center gap-1.5 rounded px-1 py-0.5 text-left text-xs hover:bg-muted", focused && "bg-accent text-accent-foreground")}
      >
        <span className={cn("size-1.5 shrink-0 rounded-full", input.own ? "bg-primary" : "border border-muted-foreground")} />
        <span className="truncate">{input.label}</span>
        {animated && <span className="size-1.5 shrink-0 rotate-45 bg-amber-500" title="Animated: an edit authors a sample at the current time" />}
      </button>
      <div className="flex min-w-0 items-center">
        {port.connections.length > 0 ? <Sources store={store} sources={port.connections} /> : <InputValue store={store} input={input} at={animated ? (store.time ?? null) : null} />}
      </div>
    </>
  );
}

/** An unconnected input's editor: a slider for a bounded number, else the inspector's usual field. */
function InputValue({ store, input, at }: { store: EditorStore; input: ShadingInput; at: number | null }) {
  const attribute = store.session?.stage.attribute(input.port.path) ?? null;
  if (attribute === null) return null;
  const value = shown(attribute.get(store.time));
  const range = sliderRange(input.bounds);
  const number = value.value?.kind === "scalar" && typeof value.value.value === "number" ? value.value.value : null;
  if (range !== null && number !== null) {
    const type = attribute.typeName ?? value.value!.type;
    return (
      <SliderField
        label={input.label}
        value={number}
        range={range}
        integer={/^u?int/.test(type)}
        onCommit={(next) => store.setValue(attribute.path, { type, value: clampToBounds(next, input.bounds) }, at)}
      />
    );
  }
  return <ValueField store={store} attribute={attribute} shown={value} at={at} />;
}

/**
 * A slider over `range` beside the number: dragging moves the slider and
 * the number, and authors once, when the drag ends (one undo step); a
 * typed number may go past the soft range, within the hard bounds.
 */
function SliderField({ label, value, range, integer, onCommit }: { label: string; value: number; range: readonly [number, number]; integer: boolean; onCommit: (value: number) => void }) {
  const [dragging, setDragging] = useState<number | null>(null);
  const [min, max] = range;
  const step = integer ? 1 : (max - min) / 200;
  const current = dragging ?? value;
  return (
    <div className="flex w-full items-center gap-2">
      <Slider
        aria-label={label}
        minValue={min}
        maxValue={max}
        step={step}
        value={Math.min(max, Math.max(min, current))}
        onChange={(next) => setDragging(next as number)}
        onChangeEnd={(next) => {
          setDragging(null);
          if (next !== value) onCommit(next as number);
        }}
        className="min-w-0 flex-1"
      />
      <div className="flex w-14 shrink-0">
        <CommitNumber label={`${label} value`} value={formatNumber(current)} onCommit={onCommit} />
      </div>
    </div>
  );
}
