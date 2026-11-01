// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The asset view's parts over the viewport: the draw scope switch (Scene,
// Selection, Nothing) and why a scene opened without drawing; what the
// selection drawn alone is (a mesh's triangles, points, primvars, subsets
// and what stagehand made of it; a subtree's items and instances); and a
// texture as its image, with its size, channels and color space.
import { useEffect, useState } from "react";
import type { Stage } from "@forest-rs/layerstack";
import { anchorAssetPath } from "@forest-rs/layerstack_stagehand";
import { cn } from "@/lib/utils";
import type { DrawScopeChoice } from "../model/assetView.ts";
import { imageInfo, type ImageInfo } from "../model/images.ts";
import { enclosingMaterial } from "../model/network.ts";
import { resolveAsset } from "../model/sources.ts";
import type { EditorStore } from "../store.ts";
import { formatBytes } from "./format.ts";

const CHOICES: readonly [DrawScopeChoice, string, string][] = [
  ["scene", "Scene", "Draw the whole scene"],
  ["selection", "Selection", "Draw the selection alone, framed at the origin"],
  ["nothing", "Nothing", "Draw nothing: browse and edit without the geometry"],
];

/** The draw scope switch, and why the scene, or the selection, is not drawn. */
export function DrawScopeSwitch({ store }: { store: EditorStore }) {
  const notes = [store.focusNote, store.scopeNote].filter((note) => note !== null);
  return (
    <div className="flex flex-col items-start gap-1">
      <div role="group" aria-label="Draw" className="flex rounded-md bg-black/40 p-0.5 text-[11px] backdrop-blur-sm">
        {CHOICES.map(([choice, label, title]) => (
          <button
            key={choice}
            type="button"
            title={title}
            aria-pressed={store.drawScope === choice}
            onClick={() => store.setDrawScope(choice)}
            className={cn("rounded px-2 py-0.5 text-white/70 hover:text-white", store.drawScope === choice && "bg-white/15 text-white")}
          >
            {label}
          </button>
        ))}
      </div>
      {notes.map((note) => (
        <p key={note} role="status" className="max-w-md rounded-md bg-amber-950/70 px-2 py-1 text-[11px] text-amber-100 backdrop-blur-sm">
          {note}
        </p>
      ))}
    </div>
  );
}

/** What the selection drawn alone is: a mesh's statistics, or a subtree's draws in brief. */
export function AssetCard({ store, stage }: { store: EditorStore; stage: Stage }) {
  const focus = store.focus;
  const cost = store.cost;
  if (focus.kind !== "geometry" || cost === null) return null;
  const prim = stage.prim(focus.root);
  const total = cost.total;
  const lines: string[] = [];
  const drawn = cost.items.length === 1 ? store.scene.items.find((view) => view.item.id === cost.items[0]!.item) : undefined;
  const mesh = drawn?.buffers.mesh;
  const report = mesh?.report ?? null;
  if (report !== null) {
    lines.push(`${report.faces.toLocaleString()} faces, ${report.points.toLocaleString()} points`);
    lines.push(`${report.triangles.toLocaleString()} triangles, ${report.vertices.toLocaleString()} vertices (${report.normals} normals)`);
    const notes = [
      report.concaveFaces > 0 && `${report.concaveFaces} concave (${report.clippedFaces} ear-clipped)`,
      report.holeFaces > 0 && `${report.holeFaces} holes`,
      report.degenerateFaces > 0 && `${report.degenerateFaces} degenerate`,
      report.subsets.length > 0 && report.unassignedFaces > 0 && `${report.unassignedFaces.toLocaleString()} in no subset`,
    ].filter(Boolean);
    if (notes.length > 0) lines.push(notes.join(", "));
    if (report.topologyProblem !== null) lines.push(`not drawn: ${report.topologyProblem.reason}`);
    const primvars = prim?.primvars() ?? [];
    if (primvars.length > 0) lines.push(`primvars: ${primvars.map((primvar) => `${primvar.name} (${primvar.interpolation})`).join(", ")}`);
    if (report.subsets.length > 0) {
      const ranges = mesh!.ranges.filter((range) => range.subset !== null);
      lines.push(`${report.subsets.length} subsets: ${ranges.map((range) => `${(drawn!.buffers.subsets[range.subset!] ?? "").split("/").at(-1)} ${(range.count / 3).toLocaleString()}`).join(", ")} triangles`);
    }
  } else {
    lines.push(`draws ${total.items.toLocaleString()} over buffers ${total.buffers.toLocaleString()}, instances ${total.instances.toLocaleString()}`);
    lines.push(`${total.trianglesDrawn.toLocaleString()} triangles drawn (${total.triangles.toLocaleString()} held), ${total.vertices.toLocaleString()} vertices`);
  }
  lines.push(`${formatBytes(total.gpuBytes)} uploaded, ${formatBytes(total.cpuBytes)} held`);
  const title = focus.subset === null ? `${focus.root} · ${prim?.typeName ?? "typeless"}` : `${focus.subset} · face subset of ${focus.root}`;
  return (
    <div className="pointer-events-none max-w-md rounded-md bg-black/40 px-2 py-1 font-mono text-[11px] leading-relaxed text-white/80 backdrop-blur-sm">
      <div className="truncate text-white">{title}</div>
      {lines.map((line) => (
        <div key={line}>{line}</div>
      ))}
    </div>
  );
}

/** What the texture view read: the image's address and header, or why it could not. */
type Loaded = { readonly url: string; readonly info: ImageInfo; readonly bytes: number; readonly identifier: string } | { readonly error: string };

/**
 * The image the asset-valued attribute at `attribute` names, filling the
 * viewport, with its size, channels, bits, color space (the shader's
 * `inputs:sourceColorSpace`) and the material reading it. A UDIM set shows
 * its tile 1001; a format the browser cannot show (EXR, HDR, TGA, TIFF)
 * is described from its header alone.
 */
export function TextureView({ store, stage, attribute }: { store: EditorStore; stage: Stage; attribute: string }) {
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const time = store.time;
  useEffect(() => {
    let live = true;
    let url: string | null = null;
    setLoaded(null);
    void (async () => {
      const value = stage.attribute(attribute)?.get(time) ?? null;
      const assetPath = value?.kind === "scalar" && value.type === "asset" && typeof value.value === "string" ? value.value : "";
      const explanation = stage.attribute(attribute)?.explain(time);
      const layer = explanation?.opinions.find((opinion) => opinion.role === "contributed")?.layer.identifier ?? null;
      const identifier = anchorAssetPath(assetPath.replace("<UDIM>", "1001"), layer);
      const source = store.source;
      if (identifier === null || source === null) return { error: assetPath === "" ? "no file authored" : `${assetPath} cannot be anchored` };
      const bytes = await source.fetch(await resolveAsset(source, identifier, layer));
      if (bytes === null) return { error: `${identifier} was not found` };
      url = URL.createObjectURL(new Blob([bytes.slice()]));
      return { url, info: imageInfo(bytes), bytes: bytes.byteLength, identifier };
    })().then(
      (result) => {
        if (live) setLoaded(result);
      },
      (error: unknown) => {
        if (live) setLoaded({ error: error instanceof Error ? error.message : String(error) });
      },
    );
    return () => {
      live = false;
      if (url !== null) URL.revokeObjectURL(url);
    };
  }, [store, stage, attribute, time]);
  const [natural, setNatural] = useState<{ width: number; height: number } | null>(null);
  const prim = attribute.slice(0, attribute.lastIndexOf("."));
  const space = stage.attribute(`${prim}.inputs:sourceColorSpace`)?.get(time) ?? null;
  const colorSpace = space?.kind === "scalar" ? String(space.value) : "auto";
  const material = enclosingMaterial(stage, prim);
  const shown = loaded !== null && "url" in loaded && !["exr", "hdr", "tga", "tiff"].includes(loaded.info.format);
  return (
    <div className="absolute inset-0 flex flex-col bg-[#20201e]">
      <div className="flex min-h-0 flex-1 items-center justify-center bg-[repeating-conic-gradient(#2a2a28_0%_25%,#232321_0%_50%)] bg-[length:24px_24px] p-6">
        {shown ? (
          <img
            src={loaded.url}
            alt={attribute}
            className="h-full w-full object-contain [image-rendering:pixelated]"
            onLoad={(event) => setNatural({ width: event.currentTarget.naturalWidth, height: event.currentTarget.naturalHeight })}
          />
        ) : (
          <p className="text-xs text-white/60">
            {loaded === null ? "Reading the image…" : "error" in loaded ? loaded.error : `A ${loaded.info.format.toUpperCase()} image: the browser cannot show it.`}
          </p>
        )}
      </div>
      {loaded !== null && "url" in loaded && (
        <div className="border-t border-white/10 px-3 py-1.5 font-mono text-[11px] leading-relaxed text-white/80">
          <div className="truncate text-white">{loaded.identifier}</div>
          <div>
            {loaded.info.format.toUpperCase()}
            {(loaded.info.width ?? natural?.width) != null && ` · ${loaded.info.width ?? natural!.width} × ${loaded.info.height ?? natural!.height}`}
            {loaded.info.channels !== null && ` · ${loaded.info.channels}`}
            {loaded.info.bits !== null && ` · ${loaded.info.bits} bits`}
            {` · ${formatBytes(loaded.bytes)} · color space ${colorSpace}`}
          </div>
          <div className="text-white/60">
            {attribute}
            {material !== null && ` · read by ${material}`}
          </div>
        </div>
      )}
    </div>
  );
}
