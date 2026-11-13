// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The Health tab: what is wrong with the stage now (`model/health.ts`,
// read by the store once per change). Composition errors by prim, each
// prim live (a click selects it); the assets that contribute nothing,
// with the layer that authors each and the prims composed without it;
// the assets not supplied yet; and the paths the last move or delete
// left naming the old path. With nothing wrong, it says what it checked.
import { HugeiconsIcon } from "@hugeicons/react";
import { Alert02Icon, CheckmarkCircle02Icon } from "@hugeicons/core-free-icons";
import type { Layer } from "@forest-rs/layerstack";
import { Badge } from "@/components/ui/badge";
import { issueKind } from "../model/health.ts";
import type { PresentationContext } from "../presentations/registry.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { layerName } from "./format.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";
import { countRender } from "./renders.ts";
import { UnrepairedPaths } from "./UnrepairedPaths.tsx";

/** A section's heading. */
function Heading({ children, count }: { children: React.ReactNode; count: number }) {
  return (
    <h3 className="flex items-center gap-2 px-3 pt-3 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
      {children}
      <Badge variant="outline">{count}</Badge>
    </h3>
  );
}

/** A prim path, live when there is a stage. */
function PrimLink({ context, path }: { context: PresentationContext | null; path: string }) {
  return context === null ? <span className="font-mono">{path}</span> : <Present object={{ kind: "primPath", path }} context={context} />;
}

/** A layer, live when there is a stage. */
function LayerLink({ context, layer }: { context: PresentationContext | null; layer: Layer }) {
  return context === null ? <span className="font-mono">{layerName(layer.identifier)}</span> : <Present object={{ kind: "layer", layer }} context={context} />;
}

/** The stage's health. */
export function HealthView() {
  countRender("HealthView");
  const store = useEditorStore();
  // Read again once per change of the stage or the undo stack: never as time moves.
  useRevision(store, "composition", "history", "status");
  const health = store.health;
  if (health === null) return <p className="p-3 text-xs text-muted-foreground">Nothing is open.</p>;
  const context = presentationContext(store);
  const unrepaired = health.namespaceEdit?.unrepaired ?? [];
  if (health.count === 0 && health.missing.length === 0) {
    return (
      <div className="flex items-start gap-2 p-3 text-xs">
        <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
        <p>
          <span className="font-medium">Nothing wrong.</span> <span className="text-muted-foreground">{health.checked}</span>
        </p>
      </div>
    );
  }
  return (
    <div className="pb-3 text-xs">
      {health.groups.length > 0 && (
        <section aria-label="Composition errors">
          <Heading count={health.groups.reduce((sum, group) => sum + group.issues.length, 0)}>Composition errors</Heading>
          <ul>
            {health.groups.map((group) => (
              <li key={group.prim ?? ""} className="border-b px-3 py-1.5">
                <div className="flex items-center gap-1.5">
                  <HugeiconsIcon icon={Alert02Icon} className="size-3.5 shrink-0 text-amber-600 dark:text-amber-400" />
                  {group.prim === null ? <span className="text-muted-foreground">In a layer stack</span> : <PrimLink context={context} path={group.prim} />}
                </div>
                <ul className="mt-0.5 flex flex-col gap-0.5 pl-5 text-[11px]">
                  {group.issues.map((issue, index) => (
                    <li key={index} className="flex items-baseline gap-2">
                      <Badge variant="outline" title={issue.code}>
                        {issueKind(issue.code)}
                      </Badge>
                      <span className="min-w-0 text-muted-foreground">{issue.message}</span>
                    </li>
                  ))}
                </ul>
              </li>
            ))}
          </ul>
        </section>
      )}
      {health.missing.length > 0 && (
        <section aria-label="Missing assets">
          <Heading count={health.missing.length}>Missing assets</Heading>
          <ul>
            {health.missing.map((asset, index) => (
              <li key={index} className="flex flex-col gap-0.5 border-b px-3 py-1.5 text-[11px]">
                <div className="flex flex-wrap items-baseline gap-x-2">
                  <span className="font-mono">@{asset.assetPath}@</span>
                  <span className="text-muted-foreground">
                    a {asset.usedBy} authored in <LayerLink context={context} layer={asset.anchor} />
                  </span>
                  {asset.reason !== null && <Badge variant="outline">{asset.reason}</Badge>}
                </div>
                <div className="text-muted-foreground">
                  {asset.identifier !== null && (
                    <>
                      looked for as <span className="font-mono text-foreground">{asset.identifier}</span>
                    </>
                  )}
                  {asset.prims.length > 0 && (
                    <>
                      {" "}
                      · composed without it:{" "}
                      {asset.prims.slice(0, 6).map((prim, at) => (
                        <span key={prim}>
                          {at > 0 && ", "}
                          {prim === "/" ? "the stage's layer stack" : <PrimLink context={context} path={prim} />}
                        </span>
                      ))}
                      {asset.prims.length > 6 && ` and ${asset.prims.length - 6} more`}
                    </>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </section>
      )}
      {health.waiting.length > 0 && (
        <section aria-label="Not supplied yet">
          <Heading count={health.waiting.length}>Not supplied yet</Heading>
          <p className="px-3 pb-1 text-[11px] text-muted-foreground">The stage asked for these and the asset loop has not supplied them: an edit made them wanted, or a fetch is still running.</p>
          <ul>
            {health.waiting.map((asset, index) => (
              <li key={index} className="flex flex-wrap items-baseline gap-x-2 border-b px-3 py-1.5 text-[11px]">
                <span className="font-mono">@{asset.assetPath}@</span>
                <span className="text-muted-foreground">
                  a {asset.usedBy} authored in <LayerLink context={context} layer={asset.anchor} />
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}
      {unrepaired.length > 0 && health.namespaceEdit !== null && (
        <section aria-label="Paths left unrepaired">
          <Heading count={unrepaired.length}>Left naming the old path</Heading>
          <p className="px-3 pb-1 text-[11px] text-muted-foreground">
            {health.namespaceEdit.label} left these naming what moved or went: a path in a variant no prim selects, which the stage does not compose, is not repaired (as in OpenUSD); selecting that variant would compose it dangling.
          </p>
          <div className="px-3 pb-2 text-[11px]">
            <UnrepairedPaths store={store} unrepaired={unrepaired} />
          </div>
        </section>
      )}
    </div>
  );
}
