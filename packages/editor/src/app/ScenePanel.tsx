// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The scene browser: the prims, the layers the stage holds, and the
// cameras the viewport can look through.
import { HugeiconsIcon } from "@hugeicons/react";
import { Video01Icon } from "@hugeicons/core-free-icons";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { useStore } from "./context.ts";
import { layerName } from "./format.ts";
import { PrimTree } from "./PrimTree.tsx";

export function ScenePanel() {
  return (
    <Tabs className="flex h-full flex-col gap-0">
      <TabsList variant="line" className="w-full justify-start border-b px-2">
        <TabsTrigger className="flex-none px-2.5" id="prims">Prims</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="layers">Layers</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="cameras">Cameras</TabsTrigger>
      </TabsList>
      <TabsContent id="prims" className="min-h-0">
        <PrimTree />
      </TabsContent>
      <TabsContent id="layers" className="min-h-0 overflow-y-auto">
        <LayerList />
      </TabsContent>
      <TabsContent id="cameras" className="min-h-0 overflow-y-auto">
        <CameraList />
      </TabsContent>
    </Tabs>
  );
}

/**
 * The layers the stage holds, as it reports them: the root layer stack
 * first, then the assets its arcs read. Muting one recomposes the stage
 * without its opinions; it is the stage's state, not an edit.
 */
function LayerList() {
  const store = useStore();
  const session = store.session;
  if (session === null) return null;
  const stage = session.stage;
  const root = stage.layers()[0];
  return (
    <ul className="flex flex-col p-1">
      {stage.layers().map((layer) => {
        const muted = stage.isMuted(layer);
        const local = stage.hasLocalLayer(layer);
        return (
          <li key={String(layer.id)} className="flex items-center gap-2 rounded-md px-2 py-1 text-xs hover:bg-muted">
            <span className={cn("min-w-0 flex-1 truncate font-mono text-[11px]", muted && "text-muted-foreground line-through")} title={layerName(layer.identifier)}>
              {layerName(layer.identifier)}
            </span>
            {stage.isDirty(layer) && <Badge variant="secondary">edited</Badge>}
            {local && <Badge variant="outline">stack</Badge>}
            {layer.readOnly && <Badge variant="outline">read-only</Badge>}
            {layer !== root && (
              <Switch
                size="sm"
                aria-label={muted ? `Unmute ${layerName(layer.identifier)}` : `Mute ${layerName(layer.identifier)}`}
                isSelected={!muted}
                onChange={(on) => void store.setMuted(layer, !on)}
              />
            )}
          </li>
        );
      })}
    </ul>
  );
}

/** The stage's cameras; choosing one looks through it, as its `GfFrustum` is computed. */
function CameraList() {
  const store = useStore();
  const cameras = store.scene.cameras;
  const row = (path: string | null, label: string) => (
    <li key={path ?? ""}>
      <button
        type="button"
        onClick={() => store.lookThrough(path)}
        className={cn("flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-xs hover:bg-muted", store.through === path && "bg-accent text-accent-foreground")}
      >
        <HugeiconsIcon icon={Video01Icon} className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="truncate">{label}</span>
      </button>
    </li>
  );
  return (
    <ul className="flex flex-col p-1">
      {row(null, "Free camera")}
      {cameras.map((camera) => row(camera.path, camera.path))}
      {cameras.length === 0 && <li className="px-2 py-1 text-xs text-muted-foreground">The stage has no cameras.</li>}
    </ul>
  );
}
