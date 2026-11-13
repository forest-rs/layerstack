// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A prim's properties, nested by namespace: `primvars:displayColor` is
// `displayColor` under `primvars`, `xformOp:rotateXYZ` is `rotateXYZ`
// under `xformOp`. A namespace with one property stays inline, its
// prefix dimmed. Each name says what the property is in a tooltip: its
// full name, type, variability, where its value comes from, and its
// `displayName`, `displayGroup` and `documentation` where it has them.
// Each name is a live presentation of its property path: a click explains
// it, and its context menu (or "⋯", or Shift+F10) offers what else applies,
// clearing, blocking, keying or removing it where edits go among them. A
// relationship's targets are edited in place; "Add" adds an attribute or
// a relationship. An edit a stronger opinion hides says so under its field.
import { useRef, useState } from "react";
import type { Attribute, Prim, Stage } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowRight01Icon, MoreHorizontalIcon, PlusSignIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { commands, type PresentationContext } from "../presentations/registry.ts";
import type { EditorStore } from "../store.ts";
import { AddPropertyDialog } from "./AddPropertyDialog.tsx";
import { useStoreSelector, useTime } from "./context.ts";
import { EditShadow } from "./EditShadow.tsx";
import { shown, ValueField } from "./fields.tsx";
import { CommandMenu, presentationContext } from "./presentations/Present.tsx";
import { RelationshipTargets } from "./RelationshipTargets.tsx";
import { Section } from "./Section.tsx";
import { countRender } from "./renders.ts";

/**
 * One property row. An attribute is `animated` when its value varies
 * with time (read again as time moves), and `keyed` when it has samples at
 * all, so an edit authors a sample at the current time: one sample is
 * not animated, but a default under it would not show.
 */
type Entry =
  | { readonly kind: "attribute"; readonly name: string; readonly attribute: Attribute; readonly animated: boolean; readonly keyed: boolean }
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
  countRender("PropertyList");
  // Shown by default when nothing is authored: a new prim's schema is what there is to edit.
  const [schemaChoice, setSchema] = useState<boolean | null>(null);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const [adding, setAdding] = useState(false);
  // The last value edit that does not show, said under its row.
  const shadow = useStoreSelector(store, ["history", "target"], (at) => at.shadow);
  const readOnly = prim.instanceProxy;
  const attributes = prim
    .attributeNames()
    .map((name) => prim.attribute(name))
    .filter((attribute) => attribute !== null);
  const authored = attributes.filter((attribute) => attribute.authored);
  const defined = attributes.filter((attribute) => !attribute.authored);
  // A schema just applied to this prim shows what it brought, at its fallbacks.
  const revealed = useStoreSelector(store, ["history"], (at) => (at.revealed?.prim === prim.path && at.revealed.names.length > 0 ? at.revealed : null));
  const schema = schemaChoice ?? (authored.length === 0 || revealed !== null);
  const authoredPaths = authored.map((attribute) => attribute.path);
  const varying = new Set(stage.valuesMightBeTimeVarying(authoredPaths).flatMap((varies, index) => (varies ? [authoredPaths[index]!] : [])));
  const keyed = new Set(stage.valueSourcesMightBeTimeVarying(authoredPaths).flatMap((varies, index) => (varies ? [authoredPaths[index]!] : [])));
  const entries: Entry[] = [
    ...(schema ? attributes : authored).map((attribute): Entry => ({
      kind: "attribute",
      name: attribute.name,
      attribute,
      animated: varying.has(attribute.path),
      keyed: keyed.has(attribute.path),
    })),
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
    for (const entry of flat) {
      rows.push(<Row key={entry.name} store={store} entry={entry} under={at.prefix} depth={depth} readOnly={readOnly} />);
      if (entry.kind === "attribute" && shadow?.path === entry.attribute.path) rows.push(<EditShadow key={`${entry.name} shadow`} store={store} shadow={shadow} />);
    }
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
        <span className="flex items-center gap-2">
          {defined.length > 0 && (
            <button type="button" onClick={() => setSchema(!schema)} className="text-[11px] text-muted-foreground hover:text-foreground">
              {schema ? "Hide" : "Show"} {defined.length} schema defaults
            </button>
          )}
          <Button variant="ghost" size="xs" isDisabled={readOnly} onPress={() => setAdding(true)} aria-label="Add a property: an attribute or a relationship, where edits go">
            <HugeiconsIcon icon={PlusSignIcon} data-icon="inline-start" />
            Add
          </Button>
        </span>
      }
    >
      <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-center gap-x-2 gap-y-1">
        {colorHint(prim, authored) && (
          <span className="col-span-2 text-[11px] text-muted-foreground">
            This prim's <span className="font-mono">color</span> is not a schema property, so nothing draws it: renderers draw <span className="font-mono">primvars:displayColor</span>.
          </span>
        )}
        {readOnly && (
          <span className="col-span-2 text-[11px] text-muted-foreground">An instance proxy's properties are its prototype's: they are read here, and edited on the prototype.</span>
        )}
        {authored.length === 0 && (
          <span className="col-span-2 text-[11px] text-muted-foreground">No layer authors a value here{defined.length > 0 ? ": its schema's fallbacks follow." : "."}</span>
        )}
        {rows}
      </div>
      {adding && <AddPropertyDialog store={store} prim={prim} onClose={() => setAdding(false)} />}
    </Section>
  );
}

/**
 * Whether to say that a gprim's `color` draws nothing: it authors a
 * `color` attribute, which no geometry schema defines (some exporters
 * write one), and no `primvars:displayColor`, which is what renderers draw
 * (`UsdGeomGprim`'s `displayColor`).
 */
function colorHint(prim: Prim, authored: readonly Attribute[]): boolean {
  return authored.some((attribute) => attribute.name === "color") && !authored.some((attribute) => attribute.name === "primvars:displayColor") && prim.isA("Gprim");
}

/** A metadata field's text, if the property authors or its schema defines one. */
function text(attribute: Attribute, key: string): string | null {
  const value = attribute.metadata(key);
  return value?.kind === "scalar" && typeof value.value === "string" && value.value !== "" ? value.value : null;
}

/**
 * A property's name as a live presentation of its path: a click runs its
 * first command (explaining it), and its context menu, Shift+F10 or the
 * "⋯" beside it (shown on hover or focus) lists every command that applies.
 */
function PropertyName({
  context,
  path,
  label,
  focused,
  children,
  tooltip,
  indent,
}: {
  context: PresentationContext;
  path: string;
  label: string;
  focused: boolean;
  children: React.ReactNode;
  tooltip: React.ReactNode;
  indent: React.CSSProperties;
}) {
  const object = { kind: "propertyPath", path } as const;
  const anchor = useRef<Element | null>(null);
  const [menu, setMenu] = useState(false);
  const open = (at: Element) => {
    anchor.current = at;
    setMenu(true);
  };
  return (
    <div className="group/name flex min-w-0 items-center" style={indent}>
      <TooltipTrigger delay={400}>
        <button
          type="button"
          onClick={() => commands.forObject(object, context)[0]?.run(object, context)}
          onContextMenu={(event) => {
            event.preventDefault();
            open(event.currentTarget);
          }}
          onKeyDown={(event) => {
            if (event.key === "F10" && event.shiftKey) {
              event.preventDefault();
              open(event.currentTarget);
            }
          }}
          aria-label={`${label}: explain its value; Shift+F10 for more`}
          className={cn("flex min-w-0 flex-1 items-center gap-1.5 rounded py-0.5 pr-1 text-left text-xs hover:bg-muted", focused && "bg-accent text-accent-foreground")}
        >
          {children}
        </button>
        <Tooltip placement="left" className="max-w-xs">
          {tooltip}
        </Tooltip>
      </TooltipTrigger>
      <Button
        variant="ghost"
        size="icon-xs"
        aria-label={`Commands for ${label}`}
        className="opacity-0 group-hover/name:opacity-100 focus-visible:opacity-100 data-[pressed]:opacity-100"
        onPress={(event) => open(event.target as Element)}
      >
        <HugeiconsIcon icon={MoreHorizontalIcon} />
      </Button>
      {menu && <CommandMenu object={object} context={context} triggerRef={anchor} onClose={() => setMenu(false)} />}
    </div>
  );
}

function Row({ store, entry, under, depth, readOnly }: { store: EditorStore; entry: Entry; under: string; depth: number; readOnly: boolean }) {
  countRender("PropertyRow");
  // Only an animated attribute is read again as time moves.
  const time = useTime(store, entry.kind === "attribute" && entry.animated);
  const context = presentationContext(store);
  const label = shortName(entry.name, under);
  const dimmed = label.lastIndexOf(":");
  const name = (
    <span className="truncate">
      {dimmed >= 0 && <span className="text-muted-foreground">{label.slice(0, dimmed + 1)}</span>}
      {label.slice(dimmed + 1)}
    </span>
  );
  const indent = { paddingLeft: `${depth * 12 + 4}px` };
  if (context === null) return null;
  if (entry.kind === "relationship") {
    return (
      <>
        <PropertyName
          context={context}
          path={entry.path}
          label={entry.name}
          focused={store.property === entry.path}
          indent={indent}
          tooltip={
            <div className="flex flex-col gap-0.5 text-left">
              <div className="font-mono">{entry.name}</div>
              <div className="opacity-70">
                relationship · {entry.targets.length} target{entry.targets.length === 1 ? "" : "s"}
              </div>
              <div className="mt-1 opacity-70">Right-click or ⋯ for what to do with it; its targets are edited beside it.</div>
            </div>
          }
        >
          <span className="size-1.5 shrink-0 rotate-45 bg-muted-foreground" />
          {name}
        </PropertyName>
        <RelationshipTargets store={store} path={entry.path} targets={entry.targets} readOnly={readOnly} />
      </>
    );
  }
  const { attribute, animated, keyed } = entry;
  const value = shown(attribute.get(time));
  const displayName = text(attribute, "displayName");
  const displayGroup = text(attribute, "displayGroup");
  const documentation = text(attribute, "documentation");
  const facts = [
    attribute.typeName ?? "untyped",
    attribute.variability === "uniform" ? "uniform" : null,
    attribute.custom ? "custom" : null,
    attribute.authored ? "authored" : "schema fallback",
    animated ? "animated" : keyed ? "keyed" : null,
  ].filter((fact) => fact !== null);
  return (
    <>
      <PropertyName
        context={context}
        path={attribute.path}
        label={attribute.name}
        focused={store.property === attribute.path}
        indent={indent}
        tooltip={
          <div className="flex flex-col gap-0.5 text-left">
            <div className="font-mono">{attribute.name}</div>
            {displayName !== null && <div>{displayName}</div>}
            <div className="opacity-70">{facts.join(" · ")}</div>
            {displayGroup !== null && <div className="opacity-70">group: {displayGroup}</div>}
            {documentation !== null && <div className="mt-1">{documentation}</div>}
            <div className="mt-1 opacity-70">
              Click to explain its value; right-click or ⋯ to clear, block, key or remove it{keyed ? "; an edit authors a sample at the current frame" : ""}.
            </div>
          </div>
        }
      >
        <span className={cn("size-1.5 shrink-0 rounded-full", attribute.authored ? "bg-primary" : "border border-muted-foreground")} />
        <span className={cn("flex min-w-0", !attribute.authored && "text-muted-foreground")}>{displayName === null ? name : <span className="truncate">{displayName}</span>}</span>
        {keyed && <span className={cn("size-1.5 shrink-0 rotate-45", animated ? "bg-amber-500" : "border border-amber-500")} />}
      </PropertyName>
      <div className="flex min-w-0 items-center">
        <ValueField store={store} attribute={attribute} shown={value} at={keyed ? (time ?? null) : null} />
      </div>
    </>
  );
}
