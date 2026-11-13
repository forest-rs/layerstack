// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The scene browser: the prims, the layer stack and the layers the stage
// holds, the stage's metadata, and the cameras the viewport can look
// through.
import { HugeiconsIcon } from "@hugeicons/react";
import { Video01Icon } from "@hugeicons/core-free-icons";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { SCENE_TABS } from "../store/view.ts";
import { useEditorStore, useRevision, useStoreSelector } from "./context.ts";
import { LayerStackView } from "./LayerStackView.tsx";
import { PrimTree } from "./PrimTree.tsx";
import { countRender } from "./renders.ts";
import { StageView } from "./StageView.tsx";

export function ScenePanel() {
  const store = useEditorStore();
  // The tab in front is the store's, so a command elsewhere (a layer's) can bring one forward.
  const tab = useStoreSelector(store, ["view"], (at) => at.sceneTab);
  return (
    <Tabs
      className="flex h-full flex-col gap-0"
      selectedKey={tab}
      onSelectionChange={(key) => {
        const chosen = SCENE_TABS.find((each) => each === key);
        if (chosen !== undefined) store.showSceneTab(chosen);
      }}
    >
      <TabsList variant="line" className="w-full justify-start border-b px-2">
        <TabsTrigger className="flex-none px-2.5" id="prims">Prims</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="layers">Layers</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="stage">Stage</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="cameras">Cameras</TabsTrigger>
      </TabsList>
      <TabsContent id="prims" className="min-h-0">
        <PrimTree />
      </TabsContent>
      <TabsContent id="layers" className="min-h-0 overflow-y-auto">
        <LayerStackView />
      </TabsContent>
      <TabsContent id="stage" className="min-h-0 overflow-y-auto">
        <StageView />
      </TabsContent>
      <TabsContent id="cameras" className="min-h-0 overflow-y-auto">
        <CameraList />
      </TabsContent>
    </Tabs>
  );
}

/** The stage's cameras; choosing one looks through it, as its `GfFrustum` is computed. */
function CameraList() {
  countRender("CameraList");
  const store = useEditorStore();
  useRevision(store, "draws", "view");
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
