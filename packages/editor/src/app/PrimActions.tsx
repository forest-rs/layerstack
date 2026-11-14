// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The "⋯" menu of a prim's actions: add a child of any type the stage
// registers, duplicate, move under another prim or among its siblings,
// bind a material, load or unload payloads, rename, delete. The list, its
// wording and why an action cannot run now are `model/primActions.ts`'s,
// which the context menu of the prim's path lists too, so the two menus
// offer the same. Each edit is one undoable action through the store,
// written where edits go; the dialogs are the host's (`PrimDialogs.tsx`),
// and the stage refuses what it must (a name that is not a valid
// identifier, a path already taken) with a notice saying why. What cannot
// go through an arc or variant where edits go is disabled, and why is
// said, before it is tried.
import { Fragment } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { MoreHorizontalIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { type PrimActionId, primActions, refusals } from "../model/primActions.ts";
import type { EditorStore } from "../store.ts";
import { duplicatePrim } from "./PrimDialogs.tsx";
import { askPrim } from "./primDialogRequests.ts";

/** Runs the action `id` on the prim at `path` ({@link primActions}). */
export function runPrimAction(store: EditorStore, path: string, id: PrimActionId): void {
  switch (id) {
    case "add":
      return askPrim({ kind: "add", parent: path });
    case "duplicate":
      return duplicatePrim(store, path);
    case "moveTo":
      return askPrim({ kind: "moveTo", path });
    case "moveUp":
      return void store.reorderChild(path, -1);
    case "moveDown":
      return void store.reorderChild(path, 1);
    case "bind":
      return askPrim({ kind: "bind", path });
    case "load":
      return void store.load(path);
    case "loadOnly":
      return void store.load(path, "withoutDescendants");
    case "unload":
      return void store.unload(path);
    case "rename":
      return askPrim({ kind: "rename", path });
    case "delete":
      return void store.delete(path);
  }
}

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
 * The menu's items ({@link primActions}), read only while it is open, so a
 * tree row costs no read for its menu: each part apart, a refused one
 * disabled, and why said under them.
 */
function PrimMenuItems({ store, path }: { store: EditorStore; path: string }) {
  const session = store.session;
  if (session === null) return null;
  const actions = primActions(session.stage, path, session.target);
  const why = refusals(actions);
  return (
    <>
      {actions.map((action, index) => (
        <Fragment key={action.id}>
          {index > 0 && actions[index - 1]!.group !== action.group && <DropdownMenuSeparator />}
          <DropdownMenuItem variant={action.destructive ? "destructive" : "default"} isDisabled={!action.enabled} onAction={() => runPrimAction(store, path, action.id)}>
            {action.title}
          </DropdownMenuItem>
        </Fragment>
      ))}
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
  );
}
