// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The prim hierarchy: the stage's composed prims, read as rows are
// expanded (a prim's children are asked for only when it is open), drawn
// by a virtualized React Aria tree so a stage of tens of thousands of
// prims scrolls as one of ten. Selecting a row selects the prim; a prim
// picked in the viewport opens its ancestors. A prim with payloads says
// whether they are loaded, and loads or unloads them in place, as
// usdview's tree does. An imageable prim's eye (shown on hover or focus,
// and always when it is hidden) makes it invisible or visible again, and
// a purpose other than `default` is a badge. A row dragged onto another
// moves the prim beneath it (`Stage.move`), and between two of its
// siblings reorders it among them (`reorder nameChildren`).
import { useEffect, useMemo, useRef, useState } from "react";
import {
  Button as AriaButton,
  Collection,
  DropIndicator,
  ListLayout,
  Tree,
  TreeItem,
  TreeItemContent,
  useDragAndDrop,
  Virtualizer,
  type Key,
} from "react-aria-components";
import { HugeiconsIcon } from "@hugeicons/react";
import { Alert02Icon, ArrowRight01Icon, DragDropVerticalIcon, ViewIcon, ViewOffIcon, ViewOffSlashIcon } from "@hugeicons/core-free-icons";
import type { Prim, PurposeInfo, Stage } from "@forest-rs/layerstack";
import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import { childPath, hiddenBy, nameOf, parentOf, type RowVisibility, rowVisibilities, treeDrop } from "../model/primAuthoring.ts";
import { namespaceRefusal } from "../model/composition.ts";
import { searchPrims } from "../model/primSearch.ts";
import type { EditorStore } from "../store.ts";
import { useEditorStore, useRevision, useStoreSelector } from "./context.ts";
import { primIcon } from "./icons.ts";
import { PrimActions } from "./PrimActions.tsx";
import { askPrim } from "./primDialogRequests.ts";
import { rowAction, rowAfterRemoval, treeRefusal, TYPEAHEAD_MS } from "./treeKeys.ts";
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

/** The rows' paths, parents before children, as the tree lists them. */
function rowPaths(items: readonly Row[], out: string[] = []): string[] {
  for (const row of items) {
    out.push(row.id);
    rowPaths(row.children, out);
  }
  return out;
}

/** What a row shows of its prim besides its fields: its visibility and a purpose other than `default`. */
interface RowLooks {
  readonly visibility: ReadonlyMap<string, RowVisibility>;
  readonly purposes: ReadonlyMap<string, PurposeInfo>;
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
  // The row a context menu is for, and where the menu opens.
  const [menu, setMenu] = useState<string | null>(null);
  const menuAnchor = useRef<Element | null>(null);
  // The prim being dragged, for where it may drop.
  const dragged = useRef<string | null>(null);
  // Why the prim being dragged cannot move to another parent where edits go (the menu's sentence), said while it is dragged.
  const [dragRefusal, setDragRefusal] = useState<string | null>(null);
  // Why Delete on a row was refused, said in the same place until the selection moves.
  const [keyRefusal, setKeyRefusal] = useState<string | null>(null);
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
    // The row that had the focus went with the old path: the prim's new row takes it.
    return focusRow(to);
  }, [renamed]);
  // A selection made elsewhere (the viewport) opens its ancestors.
  const selection = useStoreSelector(store, ["selection"], (at) => at.selection);
  useEffect(() => {
    setKeyRefusal(null);
    if (selection === null) return;
    setExpanded((open) => {
      const missing = ancestors(selection).filter((path) => !open.has(path));
      return missing.length === 0 ? open : new Set([...open, ...missing]);
    });
  }, [selection]);
  const items = useMemo(() => (stage === null ? [] : rows(stage, "/", expanded)), [stage, expanded, revision]);
  const paths = useMemo(() => rowPaths(items), [items]);
  // Visibility can vary with time: read for every row at once when the rows
  // change or the time does while not playing (scrubbing, stepping), not as
  // it plays. Purpose cannot (it is uniform): read when the rows change.
  // The time last read while not playing: starting to play changes nothing here, stopping reads again.
  const stillAt = useRef(store.time);
  const still = useStoreSelector(store, ["time"], (at) => {
    if (!at.playing) stillAt.current = at.time;
    return stillAt.current;
  });
  const visibility = useMemo(
    () => (stage === null ? new Map<string, RowVisibility>() : rowVisibilities(paths, stage.visibilities(paths, { time: store.time }))),
    [stage, paths, still],
  );
  const purposes = useMemo(() => {
    const out = new Map<string, PurposeInfo>();
    if (stage === null) return out;
    stage.purposeInfos(paths).forEach((info, index) => info !== null && info.purpose !== "default" && out.set(paths[index]!, info));
    return out;
  }, [stage, paths]);
  const looks: RowLooks = { visibility, purposes };
  const matches = useMemo(
    () => (stage === null || query.trim() === "" ? null : searchPrims(stage, { text: query, limit: MATCHES })),
    [stage, query, revision],
  );
  const { dragAndDropHooks } = useDragAndDrop<Row>({
    getItems: (keys) => [...keys].map((key) => ({ [PRIM_DRAG]: String(key), "text/plain": String(key) })),
    getAllowedDropOperations: () => ["move"],
    acceptedDragTypes: [PRIM_DRAG],
    onDragStart: (event) => {
      dragged.current = [...event.keys].map(String)[0] ?? null;
      setDragRefusal(dragged.current === null ? null : namespaceRefusal(store.session?.target, "rename", dragged.current));
    },
    onDragEnd: () => {
      dragged.current = null;
      setDragRefusal(null);
    },
    getDropOperation: (target) => (dropOf(store, dragged.current, target) === null ? "cancel" : "move"),
    onMove: (event) => {
      const path = [...event.keys].map(String)[0] ?? null;
      const drop = dropOf(store, path, event.target);
      if (drop === null || path === null) return;
      if (drop.kind === "reorder") {
        store.reorderChildren(drop.parent, drop.order, `move ${nameOf(path)} in ${drop.parent}`);
        return;
      }
      // The prim dropped is selected, which opens the rows down to it.
      if (store.moveTo(path, drop.parent) === undefined) store.select(childPath(drop.parent, nameOf(path)));
    },
    renderDropIndicator: (target) => <DropIndicator target={target} className={({ isDropTarget }) => cn("h-0.5 rounded-full", isDropTarget && "bg-primary")} />,
    isDisabled: stage === null,
  });
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
            else if (action === "deselect") store.select(null);
            else if (action === "menu") {
              menuAnchor.current = event.target as HTMLElement;
              setMenu(path);
            } else if (stage.prim(path)?.instanceProxy === true) return;
            else if (action === "delete") {
              // The focused row goes with its prim: the row after it takes the focus, so the keys go on working.
              const next = rowAfterRemoval(paths, path);
              const why = store.delete(path);
              setKeyRefusal(why === undefined ? null : treeRefusal("delete", why));
              if (why === undefined && next !== null) focusRow(next);
            } else askPrim({ kind: "rename", path });
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
            // A click on the selected row keeps it selected (Esc clears the selection).
            disallowEmptySelection
            selectedKeys={selection === null ? [] : [selection]}
            onSelectionChange={(keys) => {
              const [first] = keys === "all" ? [] : [...keys];
              store.select(first === undefined ? null : String(first));
            }}
            expandedKeys={expanded}
            onExpandedChange={(keys) => setExpanded(new Set(keys))}
            dragAndDropHooks={dragAndDropHooks}
            className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-1 pb-2 outline-none"
          >
            {function render(row: Row) {
              return (
                <TreeItem id={row.id} data-path={row.id} textValue={row.prim.name} hasChildItems={row.hasChildren} className={treeItemClass}>
                  <TreeItemContent>
                    {({ isExpanded, hasChildItems, level }) => (
                      <PrimRow store={store} row={row} looks={looks} isExpanded={isExpanded} hasChildItems={hasChildItems} level={level} />
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
      {/* Below the rows, so saying it moves none of them under the pointer. */}
      {(dragRefusal ?? keyRefusal) !== null && (
        <p role="status" className="mx-2 mb-2 rounded-md bg-muted px-2 py-1 text-[11px] text-muted-foreground">
          {dragRefusal !== null ? treeRefusal("move", dragRefusal) : keyRefusal}
        </p>
      )}
      {menu !== null && context !== null && <CommandMenu object={{ kind: "primPath", path: menu }} context={context} triggerRef={menuAnchor} onClose={() => setMenu(null)} />}
    </div>
  );
}

/**
 * Gives the focus to the tree's row at `path` once the rows are drawn
 * again and a closing dialog has given the focus back, when the focus was
 * left on nothing or on the tree (the row that had it is gone). Returns
 * the call that cancels it.
 */
function focusRow(path: string): () => void {
  const timer = window.setTimeout(() => {
    // Nothing, or the tree itself, which takes the focus of a row that goes.
    const active = document.activeElement;
    if (active !== null && active !== document.body && active.getAttribute("role") !== "treegrid") return;
    document.querySelector<HTMLElement>(`[role="row"][data-path="${CSS.escape(path)}"]`)?.focus();
  }, 60);
  return () => window.clearTimeout(timer);
}

/** The drag type of a prim dragged within the tree: its path. */
const PRIM_DRAG = "application/x-layerstack-prim";

/**
 * What dropping the prim at `path` on `target` does (`treeDrop`); `null`
 * for nothing, a drop on the tree's background, or a move while edits go
 * through an arc or variant, which a move cannot (`namespaceRefusal`).
 */
function dropOf(store: EditorStore, path: string | null, target: { type: "root" } | { type: "item"; key: Key; dropPosition: "on" | "before" | "after" }) {
  const stage = store.session?.stage;
  if (stage === undefined || path === null || target.type !== "item") return null;
  const at = String(target.key);
  if (stage.prim(path)?.instanceProxy !== false || stage.prim(at)?.instanceProxy !== false) return null;
  const siblings = target.dropPosition === "on" ? [] : stage.children(parentOf(at)).map(nameOf);
  const drop = treeDrop(path, at, target.dropPosition, siblings);
  return drop?.kind === "move" && namespaceRefusal(store.session?.target, "rename", path) !== null ? null : drop;
}

const treeItemClass = ({ isSelected, isFocusVisible, isDropTarget }: { isSelected: boolean; isFocusVisible: boolean; isDropTarget?: boolean }) =>
  cn(
    "group flex h-6 cursor-default items-center rounded-md text-xs outline-none select-none hover:bg-muted",
    isSelected && "bg-accent text-accent-foreground hover:bg-accent",
    isFocusVisible && "ring-1 ring-ring",
    isDropTarget === true && "ring-1 ring-primary bg-primary/10",
  );

function PrimRow({ store, row, looks, isExpanded, hasChildItems, level }: { store: EditorStore; row: Row; looks: RowLooks; isExpanded: boolean; hasChildItems: boolean; level: number }) {
  countRender("PrimRow");
  const { prim } = row;
  const visibility = looks.visibility.get(prim.path) ?? null;
  const purpose = looks.purposes.get(prim.path) ?? null;
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
          visibility !== null && visibility !== "visible" && "opacity-60",
        )}
      >
        {prim.name}
      </span>
      {/* The health's set, read once per change: no read of the stage per row. */}
      {store.health?.errorPrims.has(prim.path) === true && (
        <span role="img" aria-label="Composition errors" title="Composition errors: the Health tab says which" className="shrink-0 text-amber-600 dark:text-amber-400">
          <HugeiconsIcon icon={Alert02Icon} className="size-3" />
        </span>
      )}
      <span className="ml-auto flex shrink-0 items-center gap-1 pl-2 text-[10px] text-muted-foreground">
        {prim.hasAuthoredPayloads && <PayloadToggle store={store} prim={prim} />}
        {purpose !== null && (
          <span title={`Its purpose is ${purpose.purpose}${purpose.authoredOn === null || purpose.authoredOn === prim.path ? "" : `, from ${purpose.authoredOn}`}: drawn only when that purpose is`}>
            {purpose.purpose}
          </span>
        )}
        {prim.instance && <span title="An instance: its descendants come from a shared prototype">instance</span>}
        {prim.specifier !== "def" && <span title={`Its specifier is ${prim.specifier}`}>{prim.specifier}</span>}
        <span>{prim.typeName ?? ""}</span>
      </span>
      {visibility !== null && <VisibilityToggle store={store} prim={prim} visibility={visibility} rows={looks.visibility} />}
      {/* What a keyboard or screen reader drags the row by; a pointer drags the row itself. */}
      <AriaButton
        slot="drag"
        aria-label={`Drag ${prim.name} onto a prim to move it there, or between its siblings to reorder it`}
        className="sr-only flex size-4 shrink-0 items-center justify-center rounded text-muted-foreground outline-none focus-visible:not-sr-only focus-visible:ring-1 focus-visible:ring-ring"
      >
        <HugeiconsIcon icon={DragDropVerticalIcon} className="size-3" />
      </AriaButton>
      <PrimActions store={store} path={prim.path} className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100 data-[pressed]:opacity-100" />
    </div>
  );
}

/**
 * An imageable prim's eye: shown on hover or focus, and always when the
 * prim is not visible. Pressing it makes the prim invisible, or visible
 * again (its `visibility` where edits go); when an ancestor hides it, it
 * selects that ancestor, whose visibility is what to change.
 */
function VisibilityToggle({ store, prim, visibility, rows }: { store: EditorStore; prim: Prim; visibility: RowVisibility; rows: ReadonlyMap<string, RowVisibility> }) {
  const by = visibility === "hidden" ? hiddenBy(prim.path, rows) : null;
  const label =
    visibility === "visible"
      ? `Make ${prim.name} invisible`
      : visibility === "invisible"
        ? `Make ${prim.name} visible`
        : by === null
          ? `${prim.name} is hidden by an ancestor`
          : `${prim.name} is hidden by ${nameOf(by)}: select it`;
  return (
    <span title={label} className="flex">
      <AriaButton
        aria-label={label}
        isDisabled={prim.instanceProxy || (visibility === "hidden" && by === null)}
        onPress={() => {
          if (visibility === "hidden") {
            if (by !== null) store.select(by);
          } else store.setVisibility(prim.path, visibility === "invisible");
        }}
        className={cn(
          "flex size-4 shrink-0 items-center justify-center rounded text-muted-foreground outline-none hover:text-foreground focus-visible:opacity-100 focus-visible:ring-1 focus-visible:ring-ring",
          visibility === "visible" ? "opacity-0 group-hover:opacity-100" : visibility === "hidden" && "opacity-50",
        )}
      >
        <HugeiconsIcon icon={visibility === "visible" ? ViewIcon : visibility === "invisible" ? ViewOffSlashIcon : ViewOffIcon} className="size-3" />
      </AriaButton>
    </span>
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
