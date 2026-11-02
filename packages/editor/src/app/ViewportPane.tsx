// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport: three.js draws the store's draw list on a canvas (in its
// own chunk, loaded after the editor). A click picks the prim drawn
// there; a drag orbits.
import { useEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import { useStore } from "./context.ts";
import { updateSummary, viewportSummary } from "../model/draws.ts";
import { resolveAsset } from "../model/sources.ts";
import { SKIN_SLOTS } from "../model/skinning.ts";
import { enclosingMaterial } from "../model/network.ts";
import { AssetCard, DrawScopeSwitch, TextureView } from "./AssetView.tsx";
import { formatBytes } from "./format.ts";
import { LoadingOverlay } from "./LoadingOverlay.tsx";
import { MaterialPreview } from "./MaterialPreview.tsx";
import { Timeline } from "./Timeline.tsx";

export function ViewportPane() {
  const store = useStore();
  const canvas = useRef<HTMLCanvasElement>(null);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    let dispose = () => {};
    void import("../viewport/viewport3d.ts").then(
      ({ Viewport }) => {
        if (disposed || canvas.current === null) return;
        try {
          const source = () => store.source;
          const viewport = new Viewport(
            canvas.current,
            async (identifier, anchor) => {
              const scene = source();
              return scene === null ? null : scene.fetch(await resolveAsset(scene, identifier, anchor));
            },
            async (identifier, anchor) => {
              const scene = source();
              return scene === null ? false : scene.exists(await resolveAsset(scene, identifier, anchor));
            },
          );
          store.attachViewport(viewport);
          dispose = () => {
            store.attachViewport(null);
            viewport.dispose();
          };
        } catch (error) {
          setFailed(error instanceof Error ? error.message : String(error));
        }
      },
      (error: unknown) => setFailed(String(error)),
    );
    return () => {
      disposed = true;
      dispose();
    };
  }, [store]);
  // A click picks; a drag (the orbit controls') does not.
  const down = useRef<{ x: number; y: number } | null>(null);
  const summary = viewportSummary(store.scene);
  const update = store.lastUpdate === null ? null : updateSummary(store.lastUpdate);
  const through = store.through;
  // The material view: the selected material (or the one around a selected
  // shader) on a shader ball. By default it shows when the scene draws
  // nothing, as a material library's does; the toggle overrides that.
  const [materialChoice, setMaterialChoice] = useState<boolean | null>(null);
  const stage = store.session?.stage ?? null;
  const material = stage === null || store.selection === null ? null : enclosingMaterial(stage, store.selection);
  // The asset view's texture: the image a selected texture names, in place of the draws.
  const texture = store.drawScope === "selection" && store.focus.kind === "texture" ? store.focus.attribute : null;
  const showMaterial = texture === null && material !== null && (materialChoice ?? summary.drawn.length === 0);
  const textures = store.viewport?.textureStats;
  const skinning = store.scene.skinning;
  const skinned = skinning.gpu + skinning.cpu + skinning.failed.length > 0;
  return (
    <div className="flex h-full w-full flex-col">
      <div className="relative min-h-0 w-full flex-1 overflow-hidden bg-[#20201e]">
        <canvas
          ref={canvas}
          className="block h-full w-full outline-none"
          onPointerDown={(event) => {
            down.current = { x: event.clientX, y: event.clientY };
          }}
          onPointerUp={(event) => {
            const start = down.current;
            down.current = null;
            if (start === null || Math.hypot(event.clientX - start.x, event.clientY - start.y) > 4) return;
            const rect = event.currentTarget.getBoundingClientRect();
            store.select(store.viewport?.pick(event.clientX - rect.left, event.clientY - rect.top) ?? null);
          }}
        />
        {texture !== null && stage !== null && <TextureView store={store} stage={stage} attribute={texture} />}
        {showMaterial && stage !== null && (
        <div className="absolute inset-0">
          <MaterialPreview store={store} stage={stage} material={material} shape="shaderBall" />
        </div>
      )}
      <div className="absolute top-2 right-2 flex flex-col items-end gap-1">
        {material !== null && texture === null && (
          <div className="flex rounded-md bg-black/40 p-0.5 text-[11px] backdrop-blur-sm">
            {(["Scene", "Material"] as const).map((mode) => (
              <button
                key={mode}
                type="button"
                aria-pressed={(mode === "Material") === showMaterial}
                onClick={() => setMaterialChoice(mode === "Material")}
                className={cn("rounded px-2 py-0.5 text-white/70 hover:text-white", (mode === "Material") === showMaterial && "bg-white/15 text-white")}
              >
                {mode}
              </button>
            ))}
          </div>
        )}
        {/* usdview's View > Lights > Enable Default Camera Light: a light at the camera, beside the stage's own. */}
        {store.session !== null && !showMaterial && texture === null && (
          <button
            type="button"
            aria-pressed={store.lightSettings.cameraLight}
            title="A light at the camera, added to the stage's own lights, as usdview's default camera light"
            onClick={() => store.setLightSettings({ ...store.lightSettings, cameraLight: !store.lightSettings.cameraLight })}
            className={cn(
              "rounded-md bg-black/40 px-2 py-1 text-[11px] text-white/70 backdrop-blur-sm hover:text-white",
              store.lightSettings.cameraLight && "text-white",
            )}
          >
            Camera light: {store.lightSettings.cameraLight ? "on" : "off"}
          </button>
        )}
      </div>
      <LoadingOverlay />
      {textures !== undefined && textures.pending > 0 && store.status.kind !== "opening" && (
        <div role="status" className="pointer-events-none absolute bottom-2 left-2 flex items-center gap-2 rounded-md bg-black/40 px-2 py-1 font-mono text-[11px] text-white/80 backdrop-blur-sm">
          <span className="size-1.5 animate-pulse rounded-full bg-sky-400" />
          Loading textures: {textures.images} of {textures.images + textures.pending}
        </div>
      )}
      {failed !== null && (
          <div className="absolute inset-0 flex items-center justify-center text-xs text-muted-foreground">
            The viewport cannot draw here: {failed}
          </div>
        )}
        {store.session !== null && (
          <div className="absolute top-2 left-2 flex flex-col items-start gap-1">
            <DrawScopeSwitch store={store} />
            {!showMaterial && texture === null && (
              <div className="pointer-events-none rounded-md bg-black/40 px-2 py-1 font-mono text-[11px] text-white/80 backdrop-blur-sm">
                {through === null ? "Free camera" : `Looking through ${through}`}
              </div>
            )}
            {store.drawScope === "selection" && !showMaterial && stage !== null && <AssetCard store={store} stage={stage} />}
          </div>
        )}
        {!showMaterial && texture === null && (
        <div className="pointer-events-none absolute right-2 bottom-2 rounded-md bg-black/40 px-2 py-1 text-right font-mono text-[11px] leading-relaxed text-white/80 tabular-nums backdrop-blur-sm">
          <div>
            {summary.drawn.length} prims drawn · {summary.triangles.toLocaleString()} triangles · {summary.lights.length} lights
            {store.cost !== null && store.cost.total.items > 0 && (
              <span title="Bytes the draws upload (f32 attributes, u32 indices, 64 per instance), and the JavaScript arrays they hold; the Draw cost tab lists them by item">
                {" "}· {formatBytes(store.cost.total.gpuBytes)} uploaded · {formatBytes(store.cost.total.cpuBytes)} held
              </span>
            )}
          {textures !== undefined && textures.images > 0 && ` · ${textures.images} images${textures.capped > 0 ? ` (${textures.capped} scaled to ${textures.limit})` : ""}`}
          </div>
          {update !== null && (
            <div className="text-white/60">
              last update: +{update.added} −{update.removed} moved {update.moved} refilled {update.refilled} rebuilt {update.rebuilt} · read{" "}
              {update.itemsRead} draws
              {store.lastUpdate?.timeChanged === true && `, ${update.itemsTimeInvariant} left as they were`}
            </div>
          )}
          {skinned && store.lastUpdate !== null && (
            <div className="text-white/60" title={skinning.failed.join("\n")}>
              skinned: posed {store.lastUpdate.skinWork.gpuDraws} on the GPU ({store.lastUpdate.skinWork.joints.toLocaleString()} joints)
              {store.lastUpdate.skinWork.shapedDraws > 0 &&
                `, blend-shaped ${store.lastUpdate.skinWork.shapedDraws} (${store.lastUpdate.skinWork.shapedPoints.toLocaleString()} points)`}
              , deformed {store.lastUpdate.skinWork.cpuDraws} on the CPU ({store.lastUpdate.skinWork.cpuPoints.toLocaleString()} points)
              {skinning.shaped > 0 &&
                ` · ${skinning.shaped} with blend shapes${skinning.shapedVarying > 0 ? ` (${skinning.shapedVarying} reshaped as time plays)` : ""}`}
              {skinning.cpu > 0 && ` · ${skinning.cpu} with dual quaternions`}
              {skinning.reducedBlocks > 0 && ` · ${skinning.reducedBlocks.toLocaleString()} points kept ${SKIN_SLOTS} joints (dropped ≤ ${skinning.droppedWeight.toFixed(3)})`}
              {skinning.failed.length > 0 && ` · ${skinning.failed.length} drawn at rest`}
            </div>
          )}
        </div>
      )}
      </div>
      <Timeline />
    </div>
  );
}
