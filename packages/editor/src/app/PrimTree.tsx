// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The prim hierarchy: the stage's composed prims, read as rows are
// expanded (a prim's children are asked for only when it is open), drawn
// by a virtualized React Aria tree so a stage of tens of thousands of
// prims scrolls as one of ten. Selecting a row selects the prim; a prim
// picked in the viewport opens its ancestors. A prim with payloads says
// whether they are loaded, and loads or unloads them in place, as
// usdview's tree does.
import { useEffect, useMemo, useRef, useState } from "react";
import { Button as AriaButton, Collection, ListLayout, Tree, TreeItem, TreeItemContent, Virtualizer, type Key } from "react-aria-components";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowRight01Icon } from "@hugeicons/core-free-icons";
import type { Prim, Stage } from "@forest-rs/layerstack";
import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import { searchPrims } from "../model/primSearch.ts";
import type { EditorStore } from "../store.ts";
import { useEditorStore, useRevision, useStoreSelector } from "./context.ts";
import { primIcon } from "./icons.ts";
import { PrimActions, RenameDialog } from "./PrimActions.tsx";
import { rowAction, TYPEAHEAD_MS } from "./treeKeys.ts";
import { CommandMenu, presentationContext } from "./presentations/Present.tsx";
import { countRender } from "./renders.ts";

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

/** The most matches a search lists. */
const MATCHES = 500;

export function PrimTree() {
  countRender("PrimTree");
  const store = useEditorStore();
  // The hierarchy is read again when the stage changes, not when time or the draws do.
  const revision = useRevision(store, "composition");
  useRevision(store, "status");
  const stage = store.session?.stage ?? null;
  const [expanded, setExpanded] = useState<Set<Key>>(new Set());
  const [query, setQuery] = useState("");
  // The row a context menu or a rename is for, and where the menu opens.
  const [menu, setMenu] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const menuAnchor = useRef<Element | null>(null);
  // When a letter last went to the tree's type-to-find: F frames unless it continues one.
  const typedAt = useRef(0);
  // A new stage starts with its root prims' children shown.
  useEffect(() => {
    setExpanded(new Set(stage === null ? [] : stage.children("/").slice(0, 8)));
  }, [stage]);
  // A prim renamed (or a rename undone) keeps its rows open under its new path.
  const renamed = useStoreSelector(store, ["composition"], (at) => at.renamed);
  useEffect(() => {
    if (renamed === null) return;
    const { from, to } = renamed;
    setExpanded((open) => {
      const moved = [...open].map((key) => (key === from || String(key).startsWith(`${from}/`) ? `${to}${String(key).slice(from.length)}` : key));
      return moved.every((key, at) => key === [...open][at]) ? open : new Set(moved);
    });
  }, [renamed]);
  // A selection made elsewhere (the viewport) opens its ancestors.
  const selection = useStoreSelector(store, ["selection"], (at) => at.selection);
  useEffect(() => {
    if (selection === null) return;
    setExpanded((open) => {
      const missing = ancestors(selection).filter((path) => !open.has(path));
      return missing.length === 0 ? open : new Set([...open, ...missing]);
    });
  }, [selection]);
  const items = useMemo(() => (stage === null ? [] : rows(stage, "/", expanded)), [stage, expanded, revision]);
  const matches = useMemo(
    () => (stage === null || query.trim() === "" ? null : searchPrims(stage, { text: query, limit: MATCHES })),
    [stage, query, revision],
  );
  if (stage === null) return <p className="p-3 text-xs text-muted-foreground">Nothing open.</p>;
  const context = presentationContext(store);
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-1 p-2">
        <Input placeholder="Find a prim by name" value={query} onChange={(event) => setQuery(event.target.value)} className="h-7 text-xs" />
        <PrimActions store={store} path="/" />
      </div>
      {store.payloadNote !== null && <p className="mx-2 mb-2 rounded-md bg-muted px-2 py-1 text-[11px] text-muted-foreground">{store.payloadNote}</p>}
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
        // The row's keys are read in the capture phase: the tree's type-to-find
        // takes a letter before it can reach the shortcuts, and the key that
        // opens a context menu is sent to the focused row, not what is in it.
        <div
          className="flex min-h-0 flex-1 flex-col"
          onKeyDownCapture={(event) => {
            // Only a key on the row itself: a button in it keeps its keys.
            const path = event.target instanceof HTMLElement ? event.target.dataset.path : undefined;
            const searching = Date.now() - typedAt.current < TYPEAHEAD_MS;
            const action = path === undefined ? null : rowAction(event, searching);
            if (path === undefined || action === null) {
              if (event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) typedAt.current = Date.now();
              return;
            }
            event.preventDefault();
            event.stopPropagation();
            if (action === "frame") store.frameSelection();
            else if (stage.prim(path)?.instanceProxy === true) return;
            else if (action === "delete") store.delete(path);
            else setRenaming(path);
          }}
          onContextMenu={(event) => {
            const row = event.target instanceof HTMLElement ? event.target.closest<HTMLElement>("[data-path]") : null;
            if (context === null || row === null) return;
            event.preventDefault();
            menuAnchor.current = row;
            setMenu(row.dataset.path ?? null);
          }}
        >
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
                <TreeItem id={row.id} data-path={row.id} textValue={row.prim.name} hasChildItems={row.hasChildren} className={treeItemClass}>
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
        </div>
      )}
      {menu !== null && context !== null && <CommandMenu object={{ kind: "primPath", path: menu }} context={context} triggerRef={menuAnchor} onClose={() => setMenu(null)} />}
      {renaming !== null && <RenameDialog store={store} stage={stage} path={renaming} onClose={() => setRenaming(null)} />}
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
  countRender("PrimRow");
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
      <span
        className={cn(
          "truncate",
          !prim.active && "text-muted-foreground line-through",
          prim.active && !prim.loaded && "text-muted-foreground",
          prim.specifier !== "def" && "italic",
        )}
      >
        {prim.name}
      </span>
      <span className="ml-auto flex shrink-0 items-center gap-1 pl-2 text-[10px] text-muted-foreground">
        {prim.hasAuthoredPayloads && <PayloadToggle store={store} prim={prim} />}
        {prim.instance && <span title="An instance: its descendants come from a shared prototype">instance</span>}
        {prim.specifier !== "def" && <span title={`Its specifier is ${prim.specifier}`}>{prim.specifier}</span>}
        <span>{prim.typeName ?? ""}</span>
      </span>
      <PrimActions store={store} path={prim.path} className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100 data-[pressed]:opacity-100" />
    </div>
  );
}

/**
 * A prim's payloads: whether they are loaded, and a press that loads them
 * with those beneath (or unloads them), as usdview's Load column does.
 */
function PayloadToggle({ store, prim }: { store: EditorStore; prim: Prim }) {
  // Its row is kept by the tree while the hierarchy is: it listens for the status itself.
  const busy = useStoreSelector(store, ["status"], (at) => at.status.kind !== "ready");
  const label = prim.loaded ? `Unload the payloads of ${prim.path}` : `Load the payloads of ${prim.path} and beneath it`;
  return (
    <AriaButton
      aria-label={label}
      isDisabled={busy || !prim.active}
      onPress={() => void (prim.loaded ? store.unload(prim.path) : store.load(prim.path))}
      className={cn(
        "rounded border px-1 leading-4 outline-none hover:bg-background hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring",
        prim.loaded ? "border-transparent" : "border-dashed",
      )}
    >
      {prim.loaded ? "loaded" : "unloaded"}
    </AriaButton>
  );
}
