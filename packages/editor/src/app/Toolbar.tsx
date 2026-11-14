// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The toolbar: what is open and how to open another scene, where edits
// go, undo and redo, and what the stage and its draws hold.
import { useEffect, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { FolderOpenIcon, Redo02Icon, Undo02Icon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
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
import { cn } from "@/lib/utils";
import type { SceneSource } from "../model/sources.ts";
import { type EditorStore, PAYLOAD_BUDGET } from "../store.ts";
import { useEditorStore, useRevision, useStoreSelector } from "./context.ts";
import { formatBytes } from "./format.ts";
import { directoryRoots, directoryScene, droppedFiles, droppedScene, localScenes, SAMPLE, type SceneEntry } from "./scenes.ts";
import { SceneBrowser } from "./SceneBrowser.tsx";
import { TargetPicker } from "./TargetPicker.tsx";
import { countRender } from "./renders.ts";
import { EditLayerChoiceGroup, FileMenu } from "./FileMenu.tsx";
import { openConfirmation } from "../model/files.ts";
import { layerName } from "./format.ts";

/** Roots to choose between, and how to open the one chosen. */
interface Choice {
  readonly title: string;
  readonly roots: readonly string[];
  readonly open: (root: string) => SceneSource;
}

/** How long an action waits on the host before the toolbar says so: a quick one never shows. */
const RUNNING_DELAY_MS = 400;

/**
 * The action waiting on the host (an undo fetching what it brings back, a
 * variant's layers), with how many wait behind it, shown once it has
 * waited {@link RUNNING_DELAY_MS}; Cancel when it can be taken back.
 */
function RunningAction({ store }: { store: EditorStore }) {
  const running = useStoreSelector(store, ["status"], (at) => at.running);
  const queued = useStoreSelector(store, ["status"], (at) => at.queued);
  const cancellable = useStoreSelector(store, ["status"], (at) => at.cancellable);
  const [shown, setShown] = useState<string | null>(null);
  useEffect(() => {
    if (running === null) {
      setShown(null);
      return;
    }
    const timer = setTimeout(() => setShown(running), RUNNING_DELAY_MS);
    return () => clearTimeout(timer);
  }, [running]);
  if (shown === null || running === null) return null;
  return (
    <span role="status" className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
      <Spinner className="size-3.5" />
      <span className="max-w-64 truncate" title={running}>
        {running}
      </span>
      {queued > 0 && <span className="shrink-0 tabular-nums">+{queued} queued</span>}
      {cancellable && (
        <Button variant="ghost" size="xs" onPress={() => store.cancel()}>
          Cancel
        </Button>
      )}
    </span>
  );
}

/** How often the module's memory figure is read again while nothing else changes. */
const MEMORY_REFRESH_MS = 3000;

/** The module's memory, in bytes. */
const memoryBytes = (store: EditorStore) => store.layerstack.transfer().memoryBytes;

/**
 * The module's memory, read again after each recomposition and every
 * {@link MEMORY_REFRESH_MS} (a cache grown, layers released): only this
 * figure renders again, and only when the number changed; never the
 * toolbar, and never per frame.
 */
function MemoryFigure({ store }: { store: EditorStore }) {
  countRender("MemoryFigure");
  const composed = useStoreSelector(store, ["composition"], memoryBytes);
  // Polled into state, so a read that finds the same number renders nothing.
  const [, setPolled] = useState(composed);
  useEffect(() => {
    const timer = setInterval(() => setPolled(memoryBytes(store)), MEMORY_REFRESH_MS);
    return () => clearInterval(timer);
  }, [store]);
  return <>{formatBytes(memoryBytes(store))} memory</>;
}

/**
 * Asks before another scene replaces one with unsaved work
 * (`EditorStore.requestOpen`): the layers whose edits would be lost, then
 * one button that opens it, as leaving the page asks.
 */
function OpenAskedDialog({ store }: { store: EditorStore }) {
  const asked = store.openAsked;
  const lost = asked === null ? [] : store.savePlan();
  const words = asked === null ? null : openConfirmation(asked.label, lost.length);
  return (
    <Dialog isOpen={asked !== null} onOpenChange={(open) => open || store.cancelOpen()} className="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>{words?.title}</DialogTitle>
        <DialogDescription>{words?.what}</DialogDescription>
      </DialogHeader>
      <ul className="flex max-h-60 flex-col gap-1 overflow-y-auto font-mono text-[11px]">
        {lost.map((entry) => (
          <li key={String(entry.layer.id)}>{layerName(entry.layer.identifier)}</li>
        ))}
      </ul>
      <DialogFooter>
        <Button variant="outline" onPress={() => store.cancelOpen()}>
          Keep this scene
        </Button>
        <Button variant="destructive" onPress={() => void store.confirmOpen()}>
          {words?.confirm}
        </Button>
      </DialogFooter>
    </Dialog>
  );
}

/** How many draws are shown. */
const drawnCount = (store: EditorStore) => store.scene.items.reduce((count, view) => count + (view.item.hidden ? 0 : 1), 0);

export function Toolbar() {
  countRender("Toolbar");
  const store = useEditorStore();
  // What is open and opening, the stage's layers, the undo stack and what was fetched: not the time.
  useRevision(store, "status", "composition", "history", "loop");
  const [local, setLocal] = useState<SceneEntry[]>([]);
  const [choice, setChoice] = useState<Choice | null>(null);
  const [browsing, setBrowsing] = useState(false);
  useEffect(() => {
    void localScenes().then(setLocal);
  }, []);
  const offer = (title: string, roots: readonly string[], open: (root: string) => SceneSource) => {
    if (roots.length === 1) void store.requestOpen(open(roots[0]!));
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
    // `offer` reads only the store and a state setter, so the listeners are made once per store.
  }, [store]);
  const pickFolder = async () => {
    if (!("showDirectoryPicker" in window)) return;
    const directory = await window.showDirectoryPicker({ mode: "read" }).catch(() => null);
    if (directory === null) return;
    offer(`Open a layer in ${directory.name}`, await directoryRoots(directory), (root) => directoryScene(directory, root));
  };
  const session = store.session;
  const status = store.status;
  const history = session?.history;
  const draws = useStoreSelector(store, ["draws"], drawnCount);
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
            <DropdownMenuItem onAction={() => void store.requestOpen(SAMPLE.source())}>The sample scene</DropdownMenuItem>
            <DropdownMenuItem onAction={() => void pickFolder()} isDisabled={!("showDirectoryPicker" in window)}>
              A folder on this computer…
            </DropdownMenuItem>
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuGroup
            selectionMode="single"
            disallowEmptySelection
            selectedKeys={[store.payloads]}
            onSelectionChange={(keys) => {
              const [key] = keys === "all" ? [] : [...keys];
              if (key === "auto" || key === "all" || key === "none") store.setPayloads(key);
            }}
          >
            <DropdownMenuLabel>Payloads of the scene opened</DropdownMenuLabel>
            <DropdownMenuItem id="auto">Load unless over {formatBytes(PAYLOAD_BUDGET)} of layers</DropdownMenuItem>
            <DropdownMenuItem id="all">Load all</DropdownMenuItem>
            <DropdownMenuItem id="none">Load none</DropdownMenuItem>
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <EditLayerChoiceGroup store={store} />
          {local.length > 0 && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                <DropdownMenuLabel>Local scenes</DropdownMenuLabel>
                {local.map((entry) => (
                  <DropdownMenuItem key={entry.label} onAction={() => void store.requestOpen(entry.source())}>
                    {entry.label}
                  </DropdownMenuItem>
                ))}
                <DropdownMenuItem onAction={() => setBrowsing(true)}>Browse local scenes…</DropdownMenuItem>
              </DropdownMenuGroup>
            </>
          )}
        </DropdownMenu>
      </DropdownMenuTrigger>
      <FileMenu />
      <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
        {(status.kind === "opening" || status.kind === "loading") && <Spinner className="size-3.5" />}
        <span className={cn("truncate", status.kind === "failed" && "text-destructive")} title={status.kind === "failed" ? status.message : undefined}>
          {status.kind === "opening"
            ? `Opening ${status.label}${store.opening === null ? "" : `: ${store.opening.fetched.assets} assets, ${formatBytes(store.opening.fetched.bytes)}`}`
            : status.kind === "failed"
              ? `${status.label} failed to open: ${status.message}`
              : (store.source?.label ?? "Nothing open")}
        </span>
      </div>
      <div className="ml-auto flex items-center gap-1">
        <RunningAction store={store} />
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
            <MemoryFigure store={store} />
            {session.released.layers > 0 && ` · ${formatBytes(session.released.bytes)} released`}
          </span>
        )}
      </div>
      <OpenAskedDialog store={store} />
      <SceneBrowser isOpen={browsing} onClose={() => setBrowsing(false)} onOpen={(source) => void store.requestOpen(source)} />
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
                void store.requestOpen(picked.open(root));
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
