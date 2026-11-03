// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Parts of the material view (`ShadingSection.tsx`): a texture an input
// reads, as a thumbnail with its file, colour space and wrap modes; and the
// prims the viewport draws with a material bound, each live.
import { useEffect, useState } from "react";
import type { Stage } from "@forest-rs/layerstack";
import { anchorAssetPath } from "@forest-rs/layerstack_stagehand";
import { primOf } from "../../model/network.ts";
import { readAsset } from "../../model/sources.ts";
import { releaseValue } from "../../model/values.ts";
import type { PresentationContext } from "../../presentations/registry.ts";
import type { EditorStore } from "../../store.ts";
import { Present } from "../presentations/Present.tsx";

/** The most prims the bound-by list names. */
const BOUND_SHOWN = 12;

/** A token or asset input of a shader, as text; `null` when it has none. */
function text(stage: Stage, path: string, time: number | undefined): string | null {
  const value = stage.attribute(path)?.get(time) ?? null;
  const out = value?.kind === "scalar" ? String(value.value) : null;
  releaseValue(value);
  return out;
}

/** The image `file` (an asset input's path) names, as an object URL; `null` while it loads or when it cannot. */
function useImage(store: EditorStore, stage: Stage, file: string, time: number | undefined): { readonly url: string | null; readonly error: string | null } {
  const [state, setState] = useState<{ url: string | null; error: string | null }>({ url: null, error: null });
  useEffect(() => {
    let live = true;
    let url: string | null = null;
    void (async () => {
      const assetPath = text(stage, file, time) ?? "";
      const layer = stage.attribute(file)?.explain(time)?.opinions.find((opinion) => opinion.role === "contributed")?.layer.identifier ?? null;
      const identifier = anchorAssetPath(assetPath.replace("<UDIM>", "1001"), layer);
      const source = store.source;
      if (identifier === null || source === null) return { url: null, error: assetPath === "" ? "no file" : "cannot be anchored" };
      // Formats the browser cannot show are named, not drawn.
      if (/\.(exr|hdr|tga|tiff?)$/i.test(identifier)) return { url: null, error: identifier.split(".").at(-1)!.toUpperCase() };
      const bytes = await readAsset(source, stage, identifier, layer);
      if (bytes === null) return { url: null, error: "not found" };
      url = URL.createObjectURL(new Blob([bytes.slice()]));
      return { url, error: null };
    })().then(
      (result) => live && setState(result),
      (error: unknown) => live && setState({ url: null, error: error instanceof Error ? error.message : String(error) }),
    );
    return () => {
      live = false;
      if (url !== null) URL.revokeObjectURL(url);
    };
  }, [store, stage, file, time]);
  return state;
}

/**
 * What a connected input reads, when it reads a `UsdUVTexture`: its image
 * as a thumbnail, its file (live: a click shows it in the asset view), its
 * `sourceColorSpace` and its `wrapS` and `wrapT`. Nothing for any other
 * source.
 */
export function TextureSource({ context, source }: { context: PresentationContext; source: string }) {
  const { stage, store, time } = context;
  const shader = primOf(source);
  const id = stage.shader(shader)?.id ?? null;
  if (id !== "UsdUVTexture") return null;
  return <Texture context={context} shader={shader} key={shader} store={store} stage={stage} time={time} />;
}

function Texture({ context, shader, store, stage, time }: { context: PresentationContext; shader: string; store: EditorStore; stage: Stage; time: number | undefined }) {
  const file = `${shader}.inputs:file`;
  const image = useImage(store, stage, file, time);
  const attribute = stage.attribute(file);
  const space = text(stage, `${shader}.inputs:sourceColorSpace`, time) ?? "auto";
  const wrapS = text(stage, `${shader}.inputs:wrapS`, time) ?? "useMetadata";
  const wrapT = text(stage, `${shader}.inputs:wrapT`, time) ?? "useMetadata";
  const value = attribute?.get(time) ?? null;
  const shown = { value: value?.kind === "scalar" ? value : null, text: value?.kind === "scalar" ? String(value.value) : "none" } as const;
  releaseValue(value);
  return (
    <div className="mt-0.5 flex min-w-0 items-center gap-1.5">
      <span className="flex size-7 shrink-0 items-center justify-center overflow-hidden rounded border bg-[repeating-conic-gradient(#8884_0_25%,transparent_0_50%)] bg-[length:8px_8px] text-[8px] text-muted-foreground">
        {image.url !== null ? <img src={image.url} alt="" className="size-full object-cover" /> : (image.error ?? "…")}
      </span>
      <span className="flex min-w-0 flex-col leading-tight">
        {attribute !== null ? <Present object={{ kind: "value", attribute, typeName: "asset", shown, at: null }} context={context} /> : <span className="text-[11px] text-muted-foreground">no file</span>}
        <span className="truncate text-[10px] text-muted-foreground" title="inputs:sourceColorSpace · inputs:wrapS × inputs:wrapT">
          {space} · {wrapS === wrapT ? wrapS : `${wrapS} × ${wrapT}`}
        </span>
      </span>
    </div>
  );
}

/**
 * The prims drawn with `material` bound (whole, or a face subset of them),
 * from the draw list: what binds it among what the viewport draws, read
 * from the draws, not by searching the stage.
 */
export function BoundBy({ context, material }: { context: PresentationContext; material: string }) {
  const prims = new Set<string>();
  for (const view of context.store.scene.items) {
    if (view.item.material === material || view.item.subsetMaterials.includes(material)) prims.add(view.item.prim);
  }
  if (prims.size === 0) return <p className="mb-2 text-[11px] text-muted-foreground">No prim the viewport draws is bound to it.</p>;
  const shown = [...prims].sort().slice(0, BOUND_SHOWN);
  return (
    <div className="mb-2 text-[11px]">
      <div className="text-muted-foreground">
        Bound to {prims.size.toLocaleString()} drawn prim{prims.size === 1 ? "" : "s"}
        {prims.size > BOUND_SHOWN && `, the first ${BOUND_SHOWN}`}:
      </div>
      <ul className="flex flex-col">
        {shown.map((path) => (
          <li key={path} className="min-w-0 truncate">
            <Present object={{ kind: "primPath", path }} context={context} />
          </li>
        ))}
      </ul>
    </div>
  );
}
