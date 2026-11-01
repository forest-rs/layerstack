// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A prim's properties, nested by namespace: `primvars:displayColor` is
// `displayColor` under `primvars`, `xformOp:rotateXYZ` is `rotateXYZ`
// under `xformOp`. A namespace with one property stays inline, its
// prefix dimmed. Each name says what the property is in a tooltip: its
// full name, type, variability, where its value comes from, and its
// `displayName`, `displayGroup` and `documentation` where it has them.
import { useState } from "react";
import type { Attribute, Prim, Stage } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowRight01Icon } from "@hugeicons/core-free-icons";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { EditorStore } from "../store.ts";
import { shown, ValueField } from "./fields.tsx";
import { Section } from "./Section.tsx";

/** One property row. */
type Entry =
  | { readonly kind: "attribute"; readonly name: string; readonly attribute: Attribute; readonly animated: boolean }
  | { readonly kind: "relationship"; readonly name: string; readonly path: string; readonly targets: readonly string[] };

/** A namespace: its own rows and the namespaces nested in it. */
interface Group {
  /** The namespace's full prefix without the trailing `:` (`primvars`, `collection:lightLink`). */
  readonly prefix: string;
  readonly entries: Entry[];
  readonly groups: Map<string, Group>;
}

const group = (prefix: string): Group => ({ prefix, entries: [], groups: new Map() });

/** `entries` nested by the namespaces of their names. */
function nest(entries: readonly Entry[]): Group {
  const root = group("");
  for (const entry of entries) {
    const parts = entry.name.split(":");
    let at = root;
    for (const part of parts.slice(0, -1)) {
      const prefix = at.prefix === "" ? part : `${at.prefix}:${part}`;
      let next = at.groups.get(part);
      if (next === undefined) {
        next = group(prefix);
        at.groups.set(part, next);
      }
      at = next;
    }
    at.entries.push(entry);
  }
  return root;
}

/** How many rows a group holds, nested ones included. */
function size(of: Group): number {
  let count = of.entries.length;
  for (const nested of of.groups.values()) count += size(nested);
  return count;
}

/** A row's name under its group: the last segment, or for a namespace kept inline the rest after `under`. */
function shortName(name: string, under: string): string {
  return under === "" ? name : name.slice(under.length + 1);
}

export function PropertyList({ store, stage, prim }: { store: EditorStore; stage: Stage; prim: Prim }) {
  // Shown by default when nothing is authored: a new prim's schema is what there is to edit.
  const [schemaChoice, setSchema] = useState<boolean | null>(null);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const attributes = prim
    .attributeNames()
    .map((name) => prim.attribute(name))
    .filter((attribute) => attribute !== null);
  const authored = attributes.filter((attribute) => attribute.authored);
  const defined = attributes.filter((attribute) => !attribute.authored);
  const schema = schemaChoice ?? authored.length === 0;
  const varying = new Set(
    stage
      .valuesMightBeTimeVarying(authored.map((attribute) => attribute.path))
      .flatMap((varies, index) => (varies ? [authored[index]!.path] : [])),
  );
  const entries: Entry[] = [
    ...(schema ? attributes : authored).map((attribute): Entry => ({ kind: "attribute", name: attribute.name, attribute, animated: varying.has(attribute.path) })),
    ...prim.relationshipNames().flatMap((name): Entry[] => {
      const relationship = prim.relationship(name);
      return relationship === null ? [] : [{ kind: "relationship", name, path: relationship.path, targets: relationship.targets() }];
    }),
  ].sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const toggle = (prefix: string) =>
    setCollapsed((was) => {
      const next = new Set(was);
      if (!next.delete(prefix)) next.add(prefix);
      return next;
    });
  const rows: React.ReactNode[] = [];
  const emit = (at: Group, depth: number) => {
    // A namespace with one row stays inline; others nest under a header.
    const inline = [...at.groups.values()].filter((nested) => size(nested) === 1);
    const nested = [...at.groups.values()].filter((nested) => size(nested) > 1);
    const flat: Entry[] = [...at.entries];
    for (const one of inline) {
      const collect = (of: Group): Entry[] => [...of.entries, ...[...of.groups.values()].flatMap(collect)];
      flat.push(...collect(one));
    }
    flat.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    for (const entry of flat) rows.push(<Row key={entry.name} store={store} entry={entry} under={at.prefix} depth={depth} />);
    for (const child of nested.sort((a, b) => (a.prefix < b.prefix ? -1 : 1))) {
      const open = !collapsed.has(child.prefix);
      rows.push(
        <button
          key={`${child.prefix}:`}
          type="button"
          onClick={() => toggle(child.prefix)}
          aria-expanded={open}
          className="col-span-2 flex items-center gap-1 rounded px-1 py-0.5 text-left text-xs text-muted-foreground hover:bg-muted hover:text-foreground"
          style={{ paddingLeft: `${depth * 12 + 4}px` }}
        >
          <HugeiconsIcon icon={ArrowRight01Icon} className={cn("size-3 transition-transform", open && "rotate-90")} />
          <span className="font-mono">{shortName(child.prefix, at.prefix)}:</span>
          <span className="text-[10px]">{size(child)}</span>
        </button>,
      );
      if (open) emit(child, depth + 1);
    }
  };
  emit(nest(entries), 0);
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
      <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-center gap-x-2 gap-y-1">
        {authored.length === 0 && (
          <span className="col-span-2 text-[11px] text-muted-foreground">No layer authors a value here{defined.length > 0 ? ": its schema's fallbacks follow." : "."}</span>
        )}
        {rows}
      </div>
    </Section>
  );
}

/** A metadata field's text, if the property authors or its schema defines one. */
function text(attribute: Attribute, key: string): string | null {
  const value = attribute.metadata(key);
  return value?.kind === "scalar" && typeof value.value === "string" && value.value !== "" ? value.value : null;
}

function Row({ store, entry, under, depth }: { store: EditorStore; entry: Entry; under: string; depth: number }) {
  const label = shortName(entry.name, under);
  const dimmed = label.lastIndexOf(":");
  const name = (
    <span className="truncate">
      {dimmed >= 0 && <span className="text-muted-foreground">{label.slice(0, dimmed + 1)}</span>}
      {label.slice(dimmed + 1)}
    </span>
  );
  const indent = { paddingLeft: `${depth * 12 + 4}px` };
  if (entry.kind === "relationship") {
    return (
      <>
        <TooltipTrigger delay={400}>
          <span className="flex min-w-0 items-center gap-1.5 rounded py-0.5 pr-1 text-xs" style={indent} tabIndex={0}>
            <span className="size-1.5 shrink-0 rotate-45 bg-muted-foreground" />
            {name}
          </span>
          <Tooltip placement="left" className="max-w-xs">
            <div className="flex flex-col gap-0.5 text-left">
              <div className="font-mono">{entry.name}</div>
              <div className="opacity-70">relationship · {entry.targets.length} target{entry.targets.length === 1 ? "" : "s"}</div>
            </div>
          </Tooltip>
        </TooltipTrigger>
        <div className="flex min-w-0 flex-col">
          {entry.targets.length === 0 && <span className="text-[11px] text-muted-foreground">no targets</span>}
          {entry.targets.map((target) => (
            <button key={target} type="button" onClick={() => store.select(target.split(".")[0]!)} className="truncate text-left font-mono text-[11px] text-sky-400 hover:underline" title={target}>
              {target}
            </button>
          ))}
        </div>
      </>
    );
  }
  const { attribute, animated } = entry;
  const focused = store.property === attribute.path;
  const value = shown(attribute.get(store.time));
  const displayName = text(attribute, "displayName");
  const displayGroup = text(attribute, "displayGroup");
  const documentation = text(attribute, "documentation");
  const facts = [
    attribute.typeName ?? "untyped",
    attribute.variability === "uniform" ? "uniform" : null,
    attribute.custom ? "custom" : null,
    attribute.authored ? "authored" : "schema fallback",
    animated ? "animated" : null,
  ].filter((fact) => fact !== null);
  return (
    <>
      <TooltipTrigger delay={400}>
        <button
          type="button"
          onClick={() => store.explain(attribute.path)}
          aria-label={`${attribute.name}: explain its value`}
          className={cn("flex min-w-0 items-center gap-1.5 rounded py-0.5 pr-1 text-left text-xs hover:bg-muted", focused && "bg-accent text-accent-foreground")}
          style={indent}
        >
          <span className={cn("size-1.5 shrink-0 rounded-full", attribute.authored ? "bg-primary" : "border border-muted-foreground")} />
          <span className={cn("flex min-w-0", !attribute.authored && "text-muted-foreground")}>{displayName === null ? name : <span className="truncate">{displayName}</span>}</span>
          {animated && <span className="size-1.5 shrink-0 rotate-45 bg-amber-500" />}
        </button>
        <Tooltip placement="left" className="max-w-xs">
          <div className="flex flex-col gap-0.5 text-left">
            <div className="font-mono">{attribute.name}</div>
            {displayName !== null && <div>{displayName}</div>}
            <div className="opacity-70">{facts.join(" · ")}</div>
            {displayGroup !== null && <div className="opacity-70">group: {displayGroup}</div>}
            {documentation !== null && <div className="mt-1">{documentation}</div>}
            <div className="mt-1 opacity-70">Click to explain its value{animated ? "; an edit authors a sample at the current frame" : ""}.</div>
          </div>
        </Tooltip>
      </TooltipTrigger>
      <div className="flex min-w-0 items-center">
        <ValueField store={store} attribute={attribute} shown={value} at={animated ? (store.time ?? null) : null} />
      </div>
    </>
  );
}
