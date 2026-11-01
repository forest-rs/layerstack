// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The toolbar: what is open and how to open another scene, where edits
// go, undo and redo, and what the stage and its draws hold.
import { useEffect, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { FolderOpenIcon, Redo02Icon, Undo02Icon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import type { SceneSource } from "../model/sources.ts";
import { useStore } from "./context.ts";
import { formatBytes } from "./format.ts";
import { directoryRoots, directoryScene, droppedFiles, droppedScene, localScenes, SAMPLE, type SceneEntry } from "./scenes.ts";
import { TargetPicker } from "./TargetPicker.tsx";

/** Roots to choose between, and how to open the one chosen. */
interface Choice {
  readonly title: string;
  readonly roots: readonly string[];
  readonly open: (root: string) => SceneSource;
}

export function Toolbar() {
  const store = useStore();
  const [local, setLocal] = useState<SceneEntry[]>([]);
  const [choice, setChoice] = useState<Choice | null>(null);
  useEffect(() => {
    void localScenes().then(setLocal);
  }, []);
  const offer = (title: string, roots: readonly string[], open: (root: string) => SceneSource) => {
    if (roots.length === 1) void store.open(open(roots[0]!));
    else if (roots.length > 1) setChoice({ title, roots, open });
  };
  // A drop anywhere opens what was dropped: a folder, or its files.
  useEffect(() => {
    const over = (event: DragEvent) => event.preventDefault();
    const drop = async (event: DragEvent) => {
      event.preventDefault();
      if (event.dataTransfer === null) return;
      const files = await droppedFiles(event.dataTransfer);
      const roots = [...files.keys()].filter((path) => /\.(usd|usda|usdc|usdz)$/i.test(path) && path.split("/").length <= 3).sort();
      offer("Open a dropped layer", roots, (root) => droppedScene(files, root));
    };
    window.addEventListener("dragover", over);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragover", over);
      window.removeEventListener("drop", drop);
    };
  });
  const pickFolder = async () => {
    if (!("showDirectoryPicker" in window)) return;
    const directory = await window.showDirectoryPicker({ mode: "read" }).catch(() => null);
    if (directory === null) return;
    offer(`Open a layer in ${directory.name}`, await directoryRoots(directory), (root) => directoryScene(directory, root));
  };
  const session = store.session;
  const status = store.status;
  const history = session?.history;
  const draws = store.scene.items.filter((view) => !view.item.hidden).length;
  return (
    <header className="flex h-10 shrink-0 items-center gap-2 border-b px-2">
      <span className="px-1 text-sm font-semibold tracking-tight">Layerstack</span>
      <DropdownMenuTrigger>
        <Button variant="outline" size="sm">
          <HugeiconsIcon icon={FolderOpenIcon} data-icon="inline-start" />
          Open
        </Button>
        <DropdownMenu className="min-w-72">
          <DropdownMenuGroup>
            <DropdownMenuItem onAction={() => void store.open(SAMPLE.source())}>The sample scene</DropdownMenuItem>
            <DropdownMenuItem onAction={() => void pickFolder()} isDisabled={!("showDirectoryPicker" in window)}>
              A folder on this computer…
            </DropdownMenuItem>
          </DropdownMenuGroup>
          {local.length > 0 && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                <DropdownMenuLabel>Local scenes</DropdownMenuLabel>
                {local.map((entry) => (
                  <DropdownMenuItem key={entry.label} onAction={() => void store.open(entry.source())}>
                    {entry.label}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuGroup>
            </>
          )}
        </DropdownMenu>
      </DropdownMenuTrigger>
      <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
        {status.kind === "opening" && <Spinner className="size-3.5" />}
        <span className="truncate">
          {status.kind === "opening"
            ? `Opening ${status.label}${store.opening === null ? "" : `: ${store.opening.fetched.assets} assets, ${formatBytes(store.opening.fetched.bytes)}`}`
            : status.kind === "failed"
              ? `${status.label} failed to open: ${status.message}`
              : (store.source?.label ?? "Nothing open")}
        </span>
      </div>
      <div className="ml-auto flex items-center gap-1">
        <TargetPicker />
        <TooltipTrigger>
          <Button variant="ghost" size="icon-sm" isDisabled={history?.nextUndo == null} onPress={() => void store.undo()} aria-label="Undo">
            <HugeiconsIcon icon={Undo02Icon} />
          </Button>
          <Tooltip>{history?.nextUndo == null ? "Nothing to undo" : `Undo ${history.nextUndo.label} (⌘Z)`}</Tooltip>
        </TooltipTrigger>
        <TooltipTrigger>
          <Button variant="ghost" size="icon-sm" isDisabled={history?.nextRedo == null} onPress={() => void store.redo()} aria-label="Redo">
            <HugeiconsIcon icon={Redo02Icon} />
          </Button>
          <Tooltip>{history?.nextRedo == null ? "Nothing to redo" : `Redo ${history.nextRedo.label} (⇧⌘Z)`}</Tooltip>
        </TooltipTrigger>
        {session !== null && (
          <span className="ml-2 font-mono text-[11px] text-muted-foreground tabular-nums">
            {session.stage.layers().length} layers · {draws} draws · {formatBytes(session.fetched.bytes)} fetched ·{" "}
            {formatBytes(store.layerstack.transfer().memoryBytes)} memory
          </span>
        )}
      </div>
      <Dialog isOpen={choice !== null} onOpenChange={(open) => open || setChoice(null)} className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{choice?.title}</DialogTitle>
          <DialogDescription>Which layer is the stage's root?</DialogDescription>
        </DialogHeader>
        <div className="flex max-h-80 flex-col overflow-y-auto">
          {choice?.roots.map((root) => (
            <Button
              key={root}
              variant="ghost"
              className="justify-start font-mono"
              onPress={() => {
                const picked = choice;
                setChoice(null);
                void store.open(picked.open(root));
              }}
            >
              {root}
            </Button>
          ))}
        </div>
      </Dialog>
    </header>
  );
}
