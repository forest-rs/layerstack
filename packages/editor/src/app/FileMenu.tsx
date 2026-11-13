// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The File menu: what leaves the page. Save writes each layer with edits
// to the identifier it was read from (`Stage.save`), into the folder it
// came from when the person allows it, else as downloads, after one
// confirmation listing them; a layer exports as USDA or USDC
// (`Stage.exportLayer`), the composed stage as one flattened layer
// (`Stage.exportFlattened`, saying what a flatten loses) or as a USDZ
// package (`Stage.exportPackage`, saying what it could not hold). The
// menu's button counts the layers with unsaved edits, and the page asks
// before it is left while there are any.
import { useEffect, useState } from "react";
import type { FlattenResult, Layer, LayerFormat, PackageResult, SaveResult } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { File02Icon } from "@hugeicons/core-free-icons";
import { Badge } from "@/components/ui/badge";
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
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { fileName, fileNames, type SaveEntry } from "../model/files.ts";
import type { EditorStore } from "../store.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { download, downloadAll, exportLayerFile } from "./download.ts";
import { formatBytes, layerName } from "./format.ts";
import { countRender } from "./renders.ts";

/** Which of the menu's dialogs is open. */
type FileDialog =
  | { readonly kind: "save" }
  | { readonly kind: "layer" }
  | { readonly kind: "flatten"; readonly format: LayerFormat; readonly result: FlattenResult }
  | { readonly kind: "package"; readonly result: PackageResult | null };

/** The flattened stage's file name: the root's, marked flattened (`scene.flat.usda`). */
function flattenedName(store: EditorStore, format: LayerFormat): string {
  return fileName(store.session?.root ?? "stage.usda", format).replace(/\.(usda|usdc)$/, ".flat.$1");
}

export function FileMenu() {
  countRender("FileMenu");
  const store = useEditorStore();
  // The dirty layers change with every edit, undo, redo and save (`history`), and with another scene.
  useRevision(store, "status", "history");
  const [dialog, setDialog] = useState<FileDialog | null>(null);
  const session = store.session;
  const dirty = store.dirtyLayers();
  // ⌘S saves, as anywhere; the page asks before it is left with edits not saved.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "s" || event.shiftKey || event.altKey) return;
      event.preventDefault();
      if (store.session !== null) setDialog({ kind: "save" });
    };
    const onLeave = (event: BeforeUnloadEvent) => {
      if (!store.unsaved) return;
      event.preventDefault();
      // Safari asks only when this is set as well.
      event.returnValue = "";
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("beforeunload", onLeave);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("beforeunload", onLeave);
    };
  }, [store]);
  const flatten = (format: LayerFormat) => {
    const result = store.exportFlattened(format);
    if (result === null) return;
    // A lossless flatten is downloaded at once; one that lost something says what first.
    if (result.lossless) download(flattenedName(store, format), result.bytes);
    else setDialog({ kind: "flatten", format, result });
  };
  const pack = async () => {
    setDialog({ kind: "package", result: null });
    const result = await store.exportPackage();
    if (result === null) {
      setDialog(null);
      return;
    }
    const sessionLayer = store.session?.sessionLayer ?? null;
    const lostEdits = sessionLayer !== null && result.omitted.includes(sessionLayer) && store.isEdited(sessionLayer);
    // A package with nothing to say is downloaded at once, and the dialog says what it holds.
    if (result.bytes !== null && !lostEdits) download(fileName(store.session?.root ?? "stage.usda", "usdz"), result.bytes);
    setDialog({ kind: "package", result });
  };
  return (
    <>
      <DropdownMenuTrigger>
        <Button variant="outline" size="sm" isDisabled={session === null} aria-label={dirty.length > 0 ? `File: ${dirty.length} layers with unsaved edits` : "File"}>
          <HugeiconsIcon icon={File02Icon} data-icon="inline-start" />
          File
          {dirty.length > 0 && (
            <Badge variant="secondary" className="ml-0.5 h-4 px-1.5">
              {dirty.length} unsaved
            </Badge>
          )}
        </Button>
        <DropdownMenu className="min-w-80">
          <DropdownMenuGroup>
            <DropdownMenuItem onAction={() => setDialog({ kind: "save" })} isDisabled={dirty.length === 0} textValue="Save">
              <span className="flex-1">{dirty.length === 0 ? "Save (nothing is edited)" : `Save ${dirty.length === 1 ? "1 edited layer" : `${dirty.length} edited layers`}…`}</span>
              <kbd className="text-muted-foreground">⌘S</kbd>
            </DropdownMenuItem>
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuLabel>Export</DropdownMenuLabel>
            <DropdownMenuItem onAction={() => setDialog({ kind: "layer" })}>A layer as .usda or .usdc…</DropdownMenuItem>
            <DropdownMenuItem onAction={() => flatten("usda")}>The stage flattened, as .usda</DropdownMenuItem>
            <DropdownMenuItem onAction={() => flatten("usdc")}>The stage flattened, as .usdc</DropdownMenuItem>
            <DropdownMenuItem onAction={() => void pack()}>The stage as a .usdz package</DropdownMenuItem>
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuItem
              isDisabled={session?.stage.editLayer == null}
              onAction={() => {
                const layer = store.session?.stage.editLayer;
                if (layer != null) store.showLayerText(layer);
              }}
            >
              Show the edit layer's text
            </DropdownMenuItem>
          </DropdownMenuGroup>
        </DropdownMenu>
      </DropdownMenuTrigger>
      <Dialog isOpen={dialog !== null} onOpenChange={(open) => open || setDialog(null)} className="sm:max-w-xl">
        {dialog?.kind === "save" && <SaveDialog store={store} onClose={() => setDialog(null)} />}
        {dialog?.kind === "layer" && <ExportLayerDialog store={store} onClose={() => setDialog(null)} />}
        {dialog?.kind === "flatten" && (
          <FlattenDialog
            result={dialog.result}
            name={flattenedName(store, dialog.format)}
            onDownload={() => {
              download(flattenedName(store, dialog.format), dialog.result.bytes);
              setDialog(null);
            }}
          />
        )}
        {dialog?.kind === "package" && <PackageDialog store={store} result={dialog.result} onClose={() => setDialog(null)} />}
      </Dialog>
    </>
  );
}

/**
 * The Open menu's choice of where the next scene's edits go: its root, or
 * a session layer in front of it, which is always so for a package.
 */
export function EditLayerChoiceGroup({ store }: { store: EditorStore }) {
  return (
    <DropdownMenuGroup
      selectionMode="multiple"
      selectedKeys={store.editLayerChoice === "session" ? ["session"] : []}
      onSelectionChange={(keys) => store.setEditLayerChoice(keys === "all" || keys.has("session") ? "session" : "root")}
    >
      <DropdownMenuLabel>Edits of the scene opened</DropdownMenuLabel>
      <DropdownMenuItem id="session" textValue="Edit in a session layer">
        <span className="flex flex-col">
          <span>Edit in a session layer</span>
          <span className="text-[11px] text-muted-foreground">
            An anonymous layer in front of the root takes the edits; the root is never saved, and the session layer is kept by exporting it. Always so for a .usdz,
            which is read-only.
          </span>
        </span>
      </DropdownMenuItem>
    </DropdownMenuGroup>
  );
}

/** What a save did: the files written, where, and what it left. */
interface SaveOutcome {
  /** File names written into the folder. */
  readonly written: readonly string[];
  /** File names downloaded. */
  readonly downloaded: readonly string[];
  /** Why writing into the folder did not happen, when it was asked for and did not. */
  readonly folderNote: string | null;
  readonly result: SaveResult;
}

/** What a save does with a layer, in words. */
const DISPOSITION: Record<SaveEntry["disposition"], string> = {
  write: "",
  session: "the session layer: not part of the root, so not saved; export it to keep its edits",
  readOnly: "read-only (a package member): cannot be saved",
  muted: "muted: a save leaves it",
};

/**
 * The save's confirmation: every dirty layer and what the save does with
 * it, then where the files go; once confirmed, what was written.
 */
function SaveDialog({ store, onClose }: { store: EditorStore; onClose: () => void }) {
  const [plan] = useState(() => store.savePlan());
  const [phase, setPhase] = useState<"confirm" | "saving" | SaveOutcome>("confirm");
  const writable = store.source?.writable ?? null;
  const writing = plan.filter((entry) => entry.disposition === "write");
  const session = plan.find((entry) => entry.disposition === "session") ?? null;
  const run = async (intoFolder: boolean) => {
    setPhase("saving");
    // Asked first, while the press still counts as the person's: a browser asks only then.
    let folderNote: string | null = null;
    const permitted = intoFolder && writable !== null ? await writable.permit().catch(() => false) : false;
    if (intoFolder && !permitted) folderNote = `Writing into ${writable?.where ?? "the folder"} was not allowed, so the files were downloaded.`;
    const result = await store.save();
    if (result === null) {
      onClose();
      return;
    }
    const names = fileNames(result.written.map((layer) => layer.identifier));
    const written: string[] = [];
    const downloads: { name: string; bytes: Uint8Array }[] = [];
    for (const [index, layer] of result.written.entries()) {
      const name = names[index]!;
      if (permitted && writable !== null) {
        try {
          await writable.write(layer.identifier, layer.bytes);
          written.push(layer.identifier);
          continue;
        } catch (error) {
          folderNote = `${layer.identifier} could not be written into ${writable.where} (${error instanceof Error ? error.message : String(error)}), so it was downloaded.`;
        }
      }
      downloads.push({ name, bytes: layer.bytes });
    }
    await downloadAll(downloads);
    setPhase({ written, downloaded: downloads.map((file) => file.name), folderNote, result });
  };
  if (plan.length === 0 && phase === "confirm") {
    return (
      <>
        <DialogHeader>
          <DialogTitle>Nothing to save</DialogTitle>
          <DialogDescription>No layer has edits that are not saved.</DialogDescription>
        </DialogHeader>
        <DialogFooter showCloseButton />
      </>
    );
  }
  if (typeof phase === "object") {
    const { result } = phase;
    return (
      <>
        <DialogHeader>
          <DialogTitle>Saved</DialogTitle>
          <DialogDescription>Each layer was written as the stage holds it, to the identifier it was read from; the stage reads those bytes from now on.</DialogDescription>
        </DialogHeader>
        <ul className="flex flex-col gap-1 font-mono text-[11px]">
          {phase.written.map((identifier) => (
            <li key={identifier}>
              {identifier} <span className="font-sans text-muted-foreground">written into {writable?.where}</span>
            </li>
          ))}
          {phase.downloaded.map((name) => (
            <li key={name}>
              {name} <span className="font-sans text-muted-foreground">downloaded</span>
            </li>
          ))}
          {result.failures.map((failure) => (
            <li key={String(failure.layer.id)} className="text-destructive">
              {layerName(failure.layer.identifier)} <span className="font-sans">not saved: {failure.message}</span>
            </li>
          ))}
          {result.skipped.map((skipped) => (
            <li key={String(skipped.layer.id)} className="text-muted-foreground">
              {layerName(skipped.layer.identifier)} <span className="font-sans">left dirty: {skipped.reason === "muted" ? "it is muted" : "the stage no longer uses it"}</span>
            </li>
          ))}
        </ul>
        {phase.folderNote !== null && <p className="text-muted-foreground">{phase.folderNote}</p>}
        <DialogFooter>
          {session !== null && (
            <Button variant="outline" onPress={() => exportLayerFile(store, session.layer, "usda")}>
              Export the session layer
            </Button>
          )}
          <Button onPress={onClose}>Done</Button>
        </DialogFooter>
      </>
    );
  }
  return (
    <>
      <DialogHeader>
        <DialogTitle>{writing.length === 0 ? "Nothing a save can write" : `Save ${writing.length === 1 ? "1 layer" : `${writing.length} layers`}`}</DialogTitle>
        <DialogDescription>
          A save writes each edited layer to the identifier it was read from (<code>Stage.save</code>), with its edits.{" "}
          {writable !== null ? `They can be written back into ${writable.where}, replacing the files there, or downloaded.` : "They are downloaded: this scene's source cannot be written to."}
        </DialogDescription>
      </DialogHeader>
      <ul className="flex max-h-72 flex-col gap-1 overflow-y-auto">
        {plan.map((entry) => (
          <li key={String(entry.layer.id)} className="flex items-baseline gap-2">
            <span className={entry.disposition === "write" ? "font-mono text-[11px]" : "font-mono text-[11px] text-muted-foreground"}>{layerName(entry.layer.identifier)}</span>
            {entry.disposition === "write" ? (
              writable === null && <span className="text-muted-foreground">as {entry.name}</span>
            ) : (
              <span className="text-muted-foreground">{DISPOSITION[entry.disposition]}</span>
            )}
          </li>
        ))}
      </ul>
      <DialogFooter>
        {phase === "saving" && <Spinner className="mr-auto size-4 self-center" />}
        {session !== null && (
          <Button variant="outline" isDisabled={phase === "saving"} onPress={() => exportLayerFile(store, session.layer, "usda")}>
            Export the session layer
          </Button>
        )}
        <Button variant="outline" isDisabled={phase === "saving"} onPress={onClose}>
          Cancel
        </Button>
        {writing.length > 0 && writable !== null && (
          <Button variant="outline" isDisabled={phase === "saving"} onPress={() => void run(false)}>
            Download instead
          </Button>
        )}
        {writing.length > 0 && (
          <Button isDisabled={phase === "saving"} onPress={() => void run(writable !== null)}>
            {writable !== null ? `Write into ${writable.where}` : writing.length === 1 ? "Save and download" : `Save and download ${writing.length} files`}
          </Button>
        )}
      </DialogFooter>
    </>
  );
}

/** A layer of the stage, and a format, to export: the layer the Text tab shows by default. */
function ExportLayerDialog({ store, onClose }: { store: EditorStore; onClose: () => void }) {
  const stage = store.session?.stage;
  const layers = stage?.layers() ?? [];
  const [chosen, setChosen] = useState<Layer | null>(() => store.textLayer);
  if (stage === undefined) return null;
  const run = (format: LayerFormat) => {
    if (chosen === null) return;
    exportLayerFile(store, chosen, format);
    onClose();
  };
  return (
    <>
      <DialogHeader>
        <DialogTitle>Export a layer</DialogTitle>
        <DialogDescription>
          A copy of one layer as it is now, unsaved edits included, as USDA text or USDC binary (<code>Stage.exportLayer</code>). The layer itself is unchanged and stays edited.
        </DialogDescription>
      </DialogHeader>
      <Select
        aria-label="The layer to export"
        value={chosen === null ? null : String(chosen.id)}
        onChange={(key) => setChosen(layers.find((layer) => String(layer.id) === key) ?? null)}
        className="w-full"
      >
        <SelectTrigger className="w-full font-mono text-[11px]">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {layers.map((layer) => (
            <SelectItem key={String(layer.id)} id={String(layer.id)} textValue={layerName(layer.identifier)}>
              <span className="font-mono text-[11px]">{layerName(layer.identifier)}</span>
              {store.isEdited(layer) && <span className="text-muted-foreground">edited</span>}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button variant="outline" isDisabled={chosen === null} onPress={() => run("usdc")}>
          Export .usdc
        </Button>
        <Button isDisabled={chosen === null} onPress={() => run("usda")}>
          Export .usda
        </Button>
      </DialogFooter>
    </>
  );
}

/** A flatten that lost something: what, before it is downloaded. */
function FlattenDialog({ result, name, onDownload }: { result: FlattenResult; name: string; onDownload: () => void }) {
  return (
    <>
      <DialogHeader>
        <DialogTitle>The flattened stage loses {result.lost.length === 1 ? "one thing" : `${result.lost.length} things`}</DialogTitle>
        <DialogDescription>
          Flattening writes every composed prim and value into one layer with no arcs (<code>Stage.exportFlattened</code>). What a layer cannot hold is written as authored and listed here.
        </DialogDescription>
      </DialogHeader>
      <ul className="flex max-h-72 flex-col gap-1 overflow-y-auto">
        {result.lost.map((loss, index) => (
          <li key={index}>
            <span className="font-mono text-[11px]">{loss.path}</span> <span className="text-muted-foreground">{loss.message}</span>
          </li>
        ))}
      </ul>
      <DialogFooter showCloseButton>
        <Button onPress={onDownload}>Download {name} anyway</Button>
      </DialogFooter>
    </>
  );
}

/** A package being made, then what it holds or why it could not be made, and what it leaves out. */
function PackageDialog({ store, result, onClose }: { store: EditorStore; result: PackageResult | null; onClose: () => void }) {
  if (result === null) {
    return (
      <DialogHeader>
        <DialogTitle>Making the package</DialogTitle>
        <DialogDescription className="flex items-center gap-2">
          <Spinner className="size-3.5" /> Fetching the assets the stage has not read yet (textures, the layers of variants not selected)…
        </DialogDescription>
      </DialogHeader>
    );
  }
  const session = store.session;
  const sessionLayer = session?.sessionLayer ?? null;
  const lostEdits = sessionLayer !== null && result.omitted.includes(sessionLayer) && store.isEdited(sessionLayer);
  const name = fileName(session?.root ?? "stage.usda", "usdz");
  const bytes = result.bytes;
  return (
    <>
      <DialogHeader>
        <DialogTitle>{bytes === null ? "The package cannot be made" : lostEdits ? "The package leaves out the session layer" : `Downloaded ${name}`}</DialogTitle>
        <DialogDescription>
          A package holds the root layer and everything it depends on, with their unsaved edits (<code>Stage.exportPackage</code>).
          {bytes !== null && ` This one: ${result.layers} layers and ${result.assets} other assets, ${formatBytes(bytes.byteLength)}.`}
        </DialogDescription>
      </DialogHeader>
      {result.unresolved.length > 0 && (
        <>
          <p>These dependencies could not be read, so nothing was packaged:</p>
          <ul className="flex max-h-60 flex-col gap-1 overflow-y-auto">
            {result.unresolved.map((dependency, index) => (
              <li key={index}>
                <span className="font-mono text-[11px]">@{dependency.assetPath}@</span> <span className="text-muted-foreground">in {layerName(dependency.layer.identifier)}: {dependency.reason}</span>
              </li>
            ))}
          </ul>
        </>
      )}
      {lostEdits && (
        <p>
          The edits are in the session layer, which is in front of the root and not one of its dependencies, so the package holds the scene without them. Export the session layer
          to keep them beside the package, or export the stage flattened to have them in one layer.
        </p>
      )}
      <DialogFooter>
        {lostEdits && sessionLayer !== null && (
          <Button variant="outline" onPress={() => exportLayerFile(store, sessionLayer, "usda")}>
            Export the session layer
          </Button>
        )}
        {lostEdits && bytes !== null && (
          <Button
            variant="outline"
            onPress={() => {
              download(name, bytes);
              onClose();
            }}
          >
            Download the package without them
          </Button>
        )}
        <Button onPress={onClose}>{bytes !== null && !lostEdits ? "Done" : "Close"}</Button>
      </DialogFooter>
    </>
  );
}
