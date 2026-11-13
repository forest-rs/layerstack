// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The paths a move or delete left naming what it moved
// (`EditReceipt.unrepaired`), as the Health tab lists them: each the layer that
// authors it, the spec holding it (as a stage path, its variant
// selections dropped, so a click explains or selects it), the field, and
// the old path it still names, each live.
import type { Layer } from "@forest-rs/layerstack";
import type { HealthUnrepaired } from "../model/health.ts";
import { specStagePath } from "../model/primAuthoring.ts";
import type { EditorStore } from "../store.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";

/** A stage path as the object presented: a property's (`/World/Robot.mirrors`) or a prim's. */
function pathObject(path: string): { readonly kind: "propertyPath"; readonly path: string } | { readonly kind: "primPath"; readonly path: string } {
  return path.includes(".") ? { kind: "propertyPath", path } : { kind: "primPath", path };
}

/** Unrepaired paths, each live; nothing when there are none. */
export function UnrepairedPaths({ store, unrepaired }: { store: EditorStore; unrepaired: readonly HealthUnrepaired<Layer>[] }) {
  const context = presentationContext(store);
  if (context === null || unrepaired.length === 0) return null;
  return (
    <ul className="mt-0.5 flex flex-col gap-0.5">
      {unrepaired.map((each, index) => {
        const spec = specStagePath(each.spec);
        return (
          <li key={index} className="flex flex-wrap items-center gap-x-1">
            <Present object={{ kind: "layer", layer: each.layer }} context={context} />
            <Present object={pathObject(spec)} context={context} />
            {each.spec !== spec && (
              <span className="font-mono text-muted-foreground" title="The spec is inside this variant, which no prim selects">
                in {each.spec.match(/\{[^}]*\}/g)?.join("") ?? ""}
              </span>
            )}
            <span className="text-muted-foreground">
              {each.field}
              {each.metadata === null || each.metadata === undefined ? "" : ` ${each.metadata}`} still names
            </span>
            <Present object={pathObject(each.path)} context={context} />
          </li>
        );
      })}
    </ul>
  );
}
