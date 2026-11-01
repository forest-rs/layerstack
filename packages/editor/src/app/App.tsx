// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's frame: the toolbar, the scene browser on the left (prims,
// layers, cameras), the viewport, the inspector on the right, and the
// panel of what happened at the bottom (explain, changes, history, the
// asset loop).
import { useEffect } from "react";
import { usePanelRef } from "react-resizable-panels";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";
import { BottomPanel } from "./BottomPanel.tsx";
import { useStore } from "./context.ts";
import { Inspector } from "./Inspector.tsx";
import { ModuleLost } from "./ModuleLost.tsx";
import { ScenePanel } from "./ScenePanel.tsx";
import { Toolbar } from "./Toolbar.tsx";
import { ViewportPane } from "./ViewportPane.tsx";

/** The share of the window the bottom panel takes, at least, while it draws a shading network. */
const GRAPH_PANEL = 45;

export function App() {
  const store = useStore();
  const bottom = usePanelRef();
  // A network wants height: the Shading tab grows a short panel, never shrinks one.
  const onTab = (tab: string) => {
    const panel = bottom.current;
    if (tab === "shading" && panel !== null && panel.getSize().asPercentage < GRAPH_PANEL) panel.resize(`${GRAPH_PANEL}`);
  };
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, [contenteditable]")) return;
      const mod = event.metaKey || event.ctrlKey;
      if (mod && event.key.toLowerCase() === "z") {
        event.preventDefault();
        void (event.shiftKey ? store.redo() : store.undo());
      } else if (!mod && event.key === "f") {
        store.frameSelection();
      } else if (event.key === "Escape") {
        store.select(null);
      } else if (event.key === " " && store.range !== null) {
        event.preventDefault();
        if (store.playing) store.pause();
        else store.play();
      } else if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
        if (target?.closest('[role="tree"], [role="slider"]')) return;
        event.preventDefault();
        store.step(event.key === "ArrowLeft" ? -1 : 1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [store]);
  // A trapped module is dead: nothing may call into it, so no panel draws.
  if (store.status.kind === "lost") return <ModuleLost status={store.status} />;
  return (
    <div className="flex h-screen flex-col bg-background text-foreground">
      <Toolbar />
      <ResizablePanelGroup orientation="vertical" className="min-h-0 flex-1">
        <ResizablePanel defaultSize="72" minSize="30">
          <ResizablePanelGroup orientation="horizontal">
            <ResizablePanel defaultSize="20" minSize="12">
              <ScenePanel />
            </ResizablePanel>
            <ResizableHandle />
            <ResizablePanel defaultSize="54" minSize="25">
              <ViewportPane />
            </ResizablePanel>
            <ResizableHandle />
            <ResizablePanel defaultSize="26" minSize="16">
              <Inspector />
            </ResizablePanel>
          </ResizablePanelGroup>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel defaultSize="28" minSize="10" panelRef={bottom}>
          <BottomPanel onTab={onTab} />
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  );
}
