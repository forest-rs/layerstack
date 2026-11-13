// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector: the selected prim as the stage composes it. Its identity
// (name, path, type, specifier, kind), then every panel presentation that
// applies to it (`presentations/registry.ts`), by priority: its state,
// its transform, a view of what it is (a mesh's preview and topology, a
// point instancer's instances, a material's network), its variant sets,
// its properties (what layers author, then what its schemas define with
// their fallbacks), its bound material, and the arcs it composes from.
// Panels are presentations of the prim's types (its schema type and
// ancestors, its applied API schemas), registered as a plugin's would be;
// a panel that throws is replaced by a line saying so, and the rest still
// show. Every editor authors through the store.
import { Component, type ErrorInfo, type ReactNode } from "react";
import { Button as AriaButton } from "react-aria-components";
import type { Prim } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { FocusPointIcon } from "@hugeicons/core-free-icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { presentations } from "../presentations/registry.ts";
import type { EditorStore } from "../store.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { primIcon } from "./icons.ts";
import { PrimActions } from "./PrimActions.tsx";
import { askPrim } from "./primDialogRequests.ts";
import { presentationContext } from "./presentations/Present.tsx";
import { countRender } from "./renders.ts";

export function Inspector() {
  countRender("Inspector");
  const store = useEditorStore();
  // The selection, the stage, and what its views show of the draws and the viewport. Not the time: a view that reads at the time asks for it (`useTime`).
  useRevision(store, "selection", "composition", "draws", "view");
  const context = presentationContext(store);
  const prim = context === null || store.selection === null ? null : context.stage.prim(store.selection);
  if (context === null || prim === null) {
    return (
      <div className="flex h-full items-center justify-center p-6 text-center text-xs text-muted-foreground">
        Select a prim in the hierarchy or the viewport to inspect it.
      </div>
    );
  }
  const object = { kind: "prim", prim } as const;
  return (
    <div className="flex h-full flex-col overflow-y-auto">
      <Header store={store} prim={prim} />
      {presentations.panels(object, context).map((panel) => {
        const Panel = panel.panel!;
        return (
          <PanelBoundary key={`${panel.id} ${prim.path}`} title={panel.title ?? panel.id}>
            <Panel object={object} context={context} />
          </PanelBoundary>
        );
      })}
    </div>
  );
}

/** A panel, or, when it throws, a line naming it and why: one view's failure leaves the others. */
class PanelBoundary extends Component<{ title: string; children: ReactNode }, { error: string | null }> {
  override state = { error: null as string | null };

  static getDerivedStateFromError(error: unknown) {
    return { error: error instanceof Error ? error.message : String(error) };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error(`the ${this.props.title} view failed`, error, info.componentStack);
  }

  override render() {
    if (this.state.error === null) return this.props.children;
    return (
      <p className="border-b px-3 py-2 text-[11px] text-destructive">
        The {this.props.title} view failed: {this.state.error}
      </p>
    );
  }
}

function Header({ store, prim }: { store: EditorStore; prim: Prim }) {
  const kind = prim.metadata("kind");
  return (
    <div className="border-b px-3 py-2.5">
      <div className="flex items-center gap-2">
        <HugeiconsIcon icon={primIcon(prim.typeName)} className="size-4 text-muted-foreground" />
        <span className="truncate text-sm font-semibold">{prim.name || "/"}</span>
        <TooltipTrigger>
          <Button variant="ghost" size="icon-xs" className="ml-auto" aria-label="Frame in the viewport" onPress={() => store.frameSelection()}>
            <HugeiconsIcon icon={FocusPointIcon} />
          </Button>
          <Tooltip>Frame in the viewport (F)</Tooltip>
        </TooltipTrigger>
        <PrimActions store={store} path={prim.path} />
      </div>
      <div className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground" title={prim.path}>
        {prim.path}
      </div>
      <div className="mt-1.5 flex flex-wrap gap-1">
        <span title="Retype it: author its typeName where edits go" className="flex">
          <AriaButton
            aria-label={`Type ${prim.typeName ?? "none"}: retype`}
            isDisabled={prim.instanceProxy || prim.path === "/"}
            onPress={() => askPrim({ kind: "retype", path: prim.path })}
            className="rounded-sm outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-default"
          >
            <Badge variant="secondary">{prim.typeName ?? "typeless"}</Badge>
          </AriaButton>
        </span>
        <Badge variant="outline">{prim.specifier}</Badge>
        {kind?.kind === "scalar" && <Badge variant="outline">kind {String(kind.value)}</Badge>}
        {prim.instance && <Badge variant="outline">instance</Badge>}
        {prim.instanceProxy && <Badge variant="outline">instance proxy</Badge>}
        {prim.abstract && <Badge variant="outline">abstract</Badge>}
      </div>
    </div>
  );
}

