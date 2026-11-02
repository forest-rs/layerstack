// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The built-in presentations of paths and layers, and the commands on
// them. A prim path is live wherever it is shown (a relationship's
// targets, a connection's source, the changes panel, an opinion's spec):
// a click selects the prim (which opens its ancestors in the tree), and
// its context menu frames it, draws it alone, selects its material or
// copies the path. A property path explains its value; a layer is where
// edits go, or muted.
import { layerName } from "../format.ts";
import { type PresentProps, registerCommand, registerPresentation } from "../../presentations/registry.ts";
import { Live } from "./Present.tsx";

/** The prim a property path is of: `/Ball` for `/Ball.radius`, `/Ball` for `/Ball.inputs:x[/Other]`. */
export function primOfProperty(path: string): string {
  const dot = path.indexOf(".", path.lastIndexOf("/"));
  return dot < 0 ? path : path.slice(0, dot);
}

function PrimPathInline({ object, context }: PresentProps<"primPath">) {
  return (
    <Live object={object} context={context} title={`${object.path}: click to select, right-click for more`}>
      {object.path}
    </Live>
  );
}

function PropertyPathInline({ object, context }: PresentProps<"propertyPath">) {
  return (
    <Live object={object} context={context} title={`${object.path}: click to explain, right-click for more`}>
      {object.path}
    </Live>
  );
}

function LayerInline({ object, context }: PresentProps<"layer">) {
  const name = layerName(object.layer.identifier);
  return (
    <Live object={object} context={context} className="text-foreground" title={`${object.layer.identifier ?? "anonymous layer"}: right-click for what to do with it`}>
      {name}
    </Live>
  );
}

const copy = (text: string) => void navigator.clipboard?.writeText(text);

/** A prim path's tooltip: what is there (its type, specifier, whether it is an instance or inactive, its children), and what a click does. */
function PrimPathTooltip({ object, context }: PresentProps<"primPath">) {
  let prim = null;
  try {
    prim = context.stage.prim(object.path);
  } catch {
    // Not a prim path: said below.
  }
  const facts =
    prim === null
      ? ["no prim is here"]
      : [
          prim.typeName ?? "typeless",
          prim.specifier,
          prim.instance ? "instance" : null,
          prim.instanceProxy ? "instance proxy" : null,
          prim.active ? null : "inactive",
          `${prim.children().length} children`,
        ].filter((fact) => fact !== null);
  return (
    <div className="flex flex-col gap-0.5 text-left">
      <div className="font-mono break-all">{object.path}</div>
      <div className="opacity-70">{facts.join(" · ")}</div>
      <div className="opacity-70">Click to select it; right-click for more.</div>
    </div>
  );
}

/** A layer's tooltip: its identifier, whether it is muted, edited or read-only, and what a right-click offers. */
function LayerTooltip({ object, context }: PresentProps<"layer">) {
  const { layer } = object;
  const facts = [layer.format, layer.readOnly ? "read-only" : null, context.stage.isMuted(layer) ? "muted" : null, context.stage.isDirty(layer) ? "edited" : null].filter((fact) => fact !== null);
  return (
    <div className="flex flex-col gap-0.5 text-left">
      <div className="font-mono break-all">{layer.identifier ?? "anonymous layer"}</div>
      <div className="opacity-70">{facts.join(" · ")}</div>
      <div className="opacity-70">Right-click to edit in it, mute it or copy its identifier.</div>
    </div>
  );
}

/** Registers the built-in presentations of paths and layers, and their commands. */
export function registerObjectPresentations(): void {
  registerPresentation({ id: "builtin.prim-path", kind: "primPath", type: "primPath", inline: PrimPathInline, tooltip: PrimPathTooltip });
  registerPresentation({ id: "builtin.property-path", kind: "propertyPath", type: "propertyPath", inline: PropertyPathInline });
  registerPresentation({ id: "builtin.layer", kind: "layer", type: "layer", inline: LayerInline, tooltip: LayerTooltip });

  const exists = (path: string, stage: { prim(path: string): unknown }) => {
    try {
      return stage.prim(path) !== null;
    } catch {
      return false;
    }
  };
  registerCommand({
    id: "builtin.prim-path.select",
    kind: "primPath",
    type: "primPath",
    title: "Select (and reveal in the tree)",
    applies: (object, { stage }) => exists(object.path, stage),
    run: (object, { store }) => store.select(object.path),
  });
  registerCommand({
    id: "builtin.prim-path.frame",
    kind: "primPath",
    type: "primPath",
    title: "Frame in the viewport",
    applies: (object, { stage }) => exists(object.path, stage),
    run: (object, { store }) => {
      store.select(object.path);
      store.frameSelection();
    },
  });
  registerCommand({
    id: "builtin.prim-path.alone",
    kind: "primPath",
    type: "primPath",
    title: "Draw it alone (asset view)",
    applies: (object, { stage }) => exists(object.path, stage),
    run: (object, { store }) => {
      store.select(object.path);
      store.setDrawScope("selection");
    },
  });
  registerCommand({
    id: "builtin.prim-path.material",
    kind: "primPath",
    type: "primPath",
    title: "Select its bound material",
    applies: (object, { stage }) => exists(object.path, stage) && stage.boundMaterials([object.path])[0] != null,
    run: (object, { store, stage }) => {
      const binding = stage.boundMaterials([object.path])[0];
      if (binding != null) store.select(binding.material);
    },
  });
  registerCommand({ id: "builtin.prim-path.copy", kind: "primPath", type: "primPath", title: "Copy the path", run: (object) => copy(object.path) });

  registerCommand({
    id: "builtin.property-path.explain",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Explain its value",
    applies: (object, { stage }) => exists(primOfProperty(object.path), stage),
    run: (object, { store }) => {
      store.select(primOfProperty(object.path));
      store.explain(object.path);
    },
  });
  registerCommand({
    id: "builtin.property-path.prim",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Select its prim",
    applies: (object, { stage }) => exists(primOfProperty(object.path), stage),
    run: (object, { store }) => store.select(primOfProperty(object.path)),
  });
  registerCommand({ id: "builtin.property-path.copy", kind: "propertyPath", type: "propertyPath", title: "Copy the path", run: (object) => copy(object.path) });

  registerCommand({
    id: "builtin.layer.target",
    kind: "layer",
    type: "layer",
    title: "Edit in this layer",
    applies: (object) => !object.layer.readOnly,
    run: (object, { store }) => store.setTarget(object.layer),
  });
  registerCommand({
    id: "builtin.layer.mute",
    kind: "layer",
    type: "layer",
    title: "Mute",
    applies: (object, { stage }) => object.layer !== stage.root && !stage.isMuted(object.layer),
    run: (object, { store }) => void store.setMuted(object.layer, true),
  });
  registerCommand({
    id: "builtin.layer.unmute",
    kind: "layer",
    type: "layer",
    title: "Unmute",
    applies: (object, { stage }) => stage.isMuted(object.layer),
    run: (object, { store }) => void store.setMuted(object.layer, false),
  });
  registerCommand({
    id: "builtin.layer.copy",
    kind: "layer",
    type: "layer",
    title: "Copy the identifier",
    applies: (object) => object.layer.identifier !== null,
    run: (object) => copy(object.layer.identifier!),
  });
}
