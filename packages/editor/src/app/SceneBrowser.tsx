// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Browsing the local scene directory the dev server serves
// (`/@scenes/list.json?dir=`), folder by folder, to open any layer in it
// as the stage's root: a material library three folders down as easily as
// a scene's entry layer.
import { useEffect, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { File01Icon, Folder01Icon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import type { SceneSource } from "../model/sources.ts";
import { localScene } from "./scenes.ts";

/** One folder as the dev server lists it. */
interface Listing {
  readonly folders: readonly string[];
  readonly layers: readonly string[];
}

export function SceneBrowser({ isOpen, onClose, onOpen }: { isOpen: boolean; onClose: () => void; onOpen: (source: SceneSource) => void }) {
  const [dir, setDir] = useState("");
  const [listing, setListing] = useState<Listing | null>(null);
  useEffect(() => {
    if (!isOpen) return;
    let current = true;
    setListing(null);
    void fetch(`/@scenes/list.json?dir=${encodeURIComponent(dir)}`)
      .then((response) => (response.ok ? (response.json() as Promise<Listing>) : { folders: [], layers: [] }))
      .then((next) => current && setListing(next));
    return () => {
      current = false;
    };
  }, [dir, isOpen]);
  const parts = dir === "" ? [] : dir.split("/");
  const row = "w-full justify-start gap-2 font-mono text-xs";
  return (
    <Dialog isOpen={isOpen} onOpenChange={(open) => open || onClose()} className="sm:max-w-xl">
      <DialogHeader>
        <DialogTitle>Local scenes</DialogTitle>
        <DialogDescription>
          The development server's scene directory. Choose a layer to open it as the stage's root; its assets resolve from its folder.
        </DialogDescription>
      </DialogHeader>
      <nav aria-label="Folder" className="flex flex-wrap items-center gap-1 text-xs">
        <button type="button" onClick={() => setDir("")} className="text-muted-foreground hover:text-foreground">
          scenes
        </button>
        {parts.map((part, index) => (
          <span key={index} className="flex items-center gap-1">
            <span className="text-muted-foreground">/</span>
            <button type="button" onClick={() => setDir(parts.slice(0, index + 1).join("/"))} className="font-mono hover:underline">
              {part}
            </button>
          </span>
        ))}
      </nav>
      <div className="flex max-h-96 flex-col overflow-y-auto rounded-md border p-1">
        {listing === null && <p className="p-2 text-xs text-muted-foreground">Reading the folder…</p>}
        {listing?.folders.map((folder) => (
          <Button key={`d:${folder}`} variant="ghost" className={row} onPress={() => setDir(dir === "" ? folder : `${dir}/${folder}`)}>
            <HugeiconsIcon icon={Folder01Icon} className="text-muted-foreground" />
            {folder}
          </Button>
        ))}
        {listing?.layers.map((layer) => (
          <Button
            key={`f:${layer}`}
            variant="ghost"
            className={row}
            onPress={() => {
              onOpen(localScene(dir === "" ? layer : `${dir}/${layer}`));
              onClose();
            }}
          >
            <HugeiconsIcon icon={File01Icon} className="text-sky-400" />
            {layer}
          </Button>
        ))}
        {listing !== null && listing.folders.length === 0 && listing.layers.length === 0 && (
          <p className="p-2 text-xs text-muted-foreground">No folders or layers here.</p>
        )}
      </div>
    </Dialog>
  );
}
