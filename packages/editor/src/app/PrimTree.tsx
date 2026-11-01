// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The prim hierarchy: the stage's composed prims, read as rows are
// expanded (a prim's children are asked for only when it is open), drawn
// by a virtualized React Aria tree so a stage of tens of thousands of
// prims scrolls as one of ten. Selecting a row selects the prim; a prim
// picked in the viewport opens its ancestors.
import { useEffect, useMemo, useState } from "react";
import { Button as AriaButton, Collection, ListLayout, Tree, TreeItem, TreeItemContent, Virtualizer, type Key } from "react-aria-components";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowRight01Icon } from "@hugeicons/core-free-icons";
import type { Prim, Stage } from "@forest-rs/layerstack";
import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import type { EditorStore } from "../store.ts";
import { useStore } from "./context.ts";
import { primIcon } from "./icons.ts";
import { PrimActions } from "./PrimActions.tsx";

/** A row: a prim, and its children when it is open. */
interface Row {
  readonly id: string;
  readonly prim: Prim;
  readonly hasChildren: boolean;
  readonly children: readonly Row[];
}

/** The rows under `path`, opening those in `expanded`. */
function rows(stage: Stage, path: string, expanded: ReadonlySet<Key>): Row[] {
  const out: Row[] = [];
  for (const child of stage.children(path)) {
    const prim = stage.prim(child);
    if (prim === null) continue;
    const children = stage.children(child);
    out.push({
      id: child,
      prim,
      hasChildren: children.length > 0,
      children: expanded.has(child) ? rows(stage, child, expanded) : [],
    });
  }
  return out;
}

/** `path`'s ancestors, nearest the root first, `/` left out. */
function ancestors(path: string): string[] {
  const out: string[] = [];
  for (let slash = path.indexOf("/", 1); slash > 0; slash = path.indexOf("/", slash + 1)) out.push(path.slice(0, slash));
  return out;
}

/** The prims whose names contain `text`, at most `limit`, in depth-first order. */
function search(stage: Stage, text: string, limit: number): string[] {
  const needle = text.toLowerCase();
  const found: string[] = [];
  const walk = (path: string) => {
    for (const child of stage.children(path)) {
      if (found.length >= limit) return;
      if (child.slice(child.lastIndexOf("/") + 1).toLowerCase().includes(needle)) found.push(child);
      walk(child);
    }
  };
  walk("/");
  return found;
}

/** The most matches a search lists. */
const MATCHES = 500;

export function PrimTree() {
  const store = useStore();
  const stage = store.session?.stage ?? null;
  const [expanded, setExpanded] = useState<Set<Key>>(new Set());
  const [query, setQuery] = useState("");
  // A new stage starts with its root prims' children shown.
  useEffect(() => {
    setExpanded(new Set(stage === null ? [] : stage.children("/").slice(0, 8)));
  }, [stage]);
  // A selection made elsewhere (the viewport) opens its ancestors.
  const selection = store.selection;
  useEffect(() => {
    if (selection === null) return;
    setExpanded((open) => {
      const missing = ancestors(selection).filter((path) => !open.has(path));
      return missing.length === 0 ? open : new Set([...open, ...missing]);
    });
  }, [selection]);
  const revision = store.revision;
  const items = useMemo(() => (stage === null ? [] : rows(stage, "/", expanded)), [stage, expanded, revision]);
  const matches = useMemo(
    () => (stage === null || query.trim() === "" ? null : search(stage, query.trim(), MATCHES)),
    [stage, query, revision],
  );
  if (stage === null) return <p className="p-3 text-xs text-muted-foreground">Nothing open.</p>;
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-1 p-2">
        <Input placeholder="Find a prim by name" value={query} onChange={(event) => setQuery(event.target.value)} className="h-7 text-xs" />
        <PrimActions store={store} path="/" />
      </div>
      {matches !== null ? (
        <div className="min-h-0 flex-1 overflow-y-auto px-1 pb-2">
          {matches.length === 0 && <p className="px-2 text-xs text-muted-foreground">No prim is named like that.</p>}
          {matches.map((path) => (
            <button
              key={path}
              type="button"
              onClick={() => store.select(path)}
              className={cn(
                "flex w-full flex-col rounded-md px-2 py-1 text-left text-xs hover:bg-muted",
                path === selection && "bg-accent text-accent-foreground",
              )}
            >
              <span>{path.slice(path.lastIndexOf("/") + 1)}</span>
              <span className="truncate font-mono text-[10px] text-muted-foreground">{path}</span>
            </button>
          ))}
          {matches.length === MATCHES && <p className="px-2 pt-1 text-[11px] text-muted-foreground">The first {MATCHES} matches.</p>}
        </div>
      ) : (
        <Virtualizer layout={ListLayout} layoutOptions={{ rowHeight: 24 }}>
          <Tree
            aria-label="Prims"
            items={items}
            selectionMode="single"
            selectedKeys={selection === null ? [] : [selection]}
            onSelectionChange={(keys) => {
              const [first] = keys === "all" ? [] : [...keys];
              store.select(first === undefined ? null : String(first));
            }}
            expandedKeys={expanded}
            onExpandedChange={(keys) => setExpanded(new Set(keys))}
            className="min-h-0 flex-1 overflow-y-auto px-1 pb-2 outline-none"
          >
            {function render(row: Row) {
              return (
                <TreeItem id={row.id} textValue={row.prim.name} hasChildItems={row.hasChildren} className={treeItemClass}>
                  <TreeItemContent>
                    {({ isExpanded, hasChildItems, level }) => (
                      <PrimRow store={store} row={row} isExpanded={isExpanded} hasChildItems={hasChildItems} level={level} />
                    )}
                  </TreeItemContent>
                  <Collection items={row.children}>{render}</Collection>
                </TreeItem>
              );
            }}
          </Tree>
        </Virtualizer>
      )}
    </div>
  );
}

const treeItemClass = ({ isSelected, isFocusVisible }: { isSelected: boolean; isFocusVisible: boolean }) =>
  cn(
    "group flex h-6 cursor-default items-center rounded-md text-xs outline-none select-none hover:bg-muted",
    isSelected && "bg-accent text-accent-foreground hover:bg-accent",
    isFocusVisible && "ring-1 ring-ring",
  );

function PrimRow({ store, row, isExpanded, hasChildItems, level }: { store: EditorStore; row: Row; isExpanded: boolean; hasChildItems: boolean; level: number }) {
  const { prim } = row;
  return (
    <div className="flex w-full min-w-0 items-center gap-1 pr-2" style={{ paddingLeft: `${(level - 1) * 12 + 2}px` }}>
      {hasChildItems ? (
        <AriaButton slot="chevron" className="flex size-4 shrink-0 items-center justify-center rounded text-muted-foreground outline-none hover:text-foreground">
          <HugeiconsIcon icon={ArrowRight01Icon} className={cn("size-3 transition-transform", isExpanded && "rotate-90")} />
        </AriaButton>
      ) : (
        <span className="size-4 shrink-0" />
      )}
      <HugeiconsIcon icon={primIcon(prim.typeName)} className="size-3.5 shrink-0 text-muted-foreground" />
      <span className={cn("truncate", !prim.active && "text-muted-foreground line-through", prim.specifier !== "def" && "italic")}>{prim.name}</span>
      <span className="ml-auto flex shrink-0 items-center gap-1 pl-2 text-[10px] text-muted-foreground">
        {prim.instance && <span title="An instance: its descendants come from a shared prototype">instance</span>}
        {prim.specifier !== "def" && <span title={`Its specifier is ${prim.specifier}`}>{prim.specifier}</span>}
        <span>{prim.typeName ?? ""}</span>
      </span>
      <PrimActions store={store} path={prim.path} className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100 data-[pressed]:opacity-100" />
    </div>
  );
}
