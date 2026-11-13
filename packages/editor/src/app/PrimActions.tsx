// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The "⋯" menu of a prim's actions: add a child of any type the stage
// registers, duplicate, move under another prim or among its siblings,
// bind a material, load or unload payloads, rename, delete. Each edit is
// one undoable action through the store, written where edits go; the
// dialogs are the host's (`PrimDialogs.tsx`), and the stage refuses what
// it must (a name that is not a valid identifier, a path already taken)
// with a notice saying why. What cannot go through an arc or variant
// where edits go is disabled, and why is said, before it is tried.
import { HugeiconsIcon } from "@hugeicons/react";
import { MoreHorizontalIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { namespaceRefusal } from "../model/composition.ts";
import { childPath, freeSiblingName, nameOf, parentOf } from "../model/primAuthoring.ts";
import type { EditorStore } from "../store.ts";
import { duplicatePrim } from "./PrimDialogs.tsx";
import { askPrim } from "./primDialogRequests.ts";

/** The "⋯" menu of a prim's actions. */
export function PrimActions({ store, path, className }: { store: EditorStore; path: string; className?: string }) {
  if (store.session === null) return null;
  return (
    <DropdownMenuTrigger>
      <Button variant="ghost" size="icon-xs" aria-label={`Actions for ${path}`} className={className}>
        <HugeiconsIcon icon={MoreHorizontalIcon} />
      </Button>
      <DropdownMenu placement="bottom end" className="min-w-44">
        <PrimMenuItems store={store} path={path} />
      </DropdownMenu>
    </DropdownMenuTrigger>
  );
}

/**
 * The menu's items, read only while it is open, so a tree row costs no
 * read for its menu. A rename, delete or move through an arc or variant
 * is refused (`namespaceRefusal`), and so is a duplicate whose copy the
 * target does not map: those items are disabled, and why is said under
 * them.
 */
function PrimMenuItems({ store, path }: { store: EditorStore; path: string }) {
  const stage = store.session?.stage;
  if (stage === undefined) return null;
  const prim = stage.prim(path);
  const readOnly = prim?.instanceProxy === true;
  const target = store.session?.target;
  const parent = parentOf(path);
  const namespace = path === "/" ? null : namespaceRefusal(target, "rename", path);
  const copy = path === "/" ? null : namespaceRefusal(target, "define", childPath(parent, freeSiblingName(nameOf(path), new Set(stage.children(parent).map(nameOf)))));
  const why = [...new Set([namespace, copy].filter((each) => each !== null))];
  // Loading works on any prim (its payloads and those beneath it, as
  // usdview offers); a prototype's proxies are loaded through their instance.
  const loadable = prim !== null && !readOnly && (prim.hasAuthoredPayloads || stage.findLoadable(path).length > 0 || stage.loadRules().length > 0);
  return (
    <>
      <DropdownMenuItem onAction={() => askPrim({ kind: "add", parent: path })} isDisabled={readOnly}>
        Add a child prim…
      </DropdownMenuItem>
      {path !== "/" && (
        <>
          <DropdownMenuItem onAction={() => duplicatePrim(store, path)} isDisabled={readOnly || copy !== null}>
            Duplicate
          </DropdownMenuItem>
          <DropdownMenuItem onAction={() => askPrim({ kind: "moveTo", path })} isDisabled={readOnly || namespace !== null}>
            Move to…
          </DropdownMenuItem>
          <SiblingMoves store={store} path={path} isDisabled={readOnly} />
          <DropdownMenuItem onAction={() => askPrim({ kind: "bind", path })} isDisabled={readOnly}>
            Bind a material…
          </DropdownMenuItem>
        </>
      )}
      {loadable && (
        <>
          <DropdownMenuSeparator />
          <DropdownMenuItem onAction={() => void store.load(path)} isDisabled={!prim?.active}>
            Load payloads
          </DropdownMenuItem>
          <DropdownMenuItem onAction={() => void store.load(path, "withoutDescendants")} isDisabled={!prim?.active}>
            Load payloads, not beneath
          </DropdownMenuItem>
          <DropdownMenuItem onAction={() => void store.unload(path)}>Unload payloads</DropdownMenuItem>
        </>
      )}
      {path !== "/" && (
        <>
          <DropdownMenuSeparator />
          <DropdownMenuItem onAction={() => askPrim({ kind: "rename", path })} isDisabled={readOnly || namespace !== null}>
            Rename…
          </DropdownMenuItem>
          <DropdownMenuItem variant="destructive" onAction={() => store.delete(path)} isDisabled={readOnly || namespace !== null}>
            Delete
          </DropdownMenuItem>
          {why.length > 0 && (
            <DropdownMenuGroup>
              {why.map((each) => (
                <DropdownMenuLabel key={each} className="max-w-64 text-[11px]">
                  {each}
                </DropdownMenuLabel>
              ))}
            </DropdownMenuGroup>
          )}
        </>
      )}
    </>
  );
}

/** Move up and Move down among the prim's siblings (`reorder nameChildren` on its parent), each disabled at its end. */
function SiblingMoves({ store, path, isDisabled }: { store: EditorStore; path: string; isDisabled: boolean }) {
  const siblings = store.session?.stage.children(parentOf(path)).map(nameOf) ?? [];
  const at = siblings.indexOf(nameOf(path));
  return (
    <>
      <DropdownMenuItem onAction={() => store.reorderChild(path, -1)} isDisabled={isDisabled || at <= 0}>
        Move up
      </DropdownMenuItem>
      <DropdownMenuItem onAction={() => store.reorderChild(path, 1)} isDisabled={isDisabled || at < 0 || at >= siblings.length - 1}>
        Move down
      </DropdownMenuItem>
    </>
  );
}
