// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport: three.js draws the store's draw list on a canvas with the
// editor's WebGPU renderer (in its own chunk, loaded after the editor). A
// click picks the prim drawn there; a drag orbits.
import { useEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import { useEditorStore, useRevision } from "./context.ts";
import { updateSummary, viewportSummary } from "../model/draws.ts";
import { assetExists, readAsset } from "../model/sources.ts";
import { SKIN_SLOTS } from "../model/skinning.ts";
import { enclosingMaterial } from "../model/network.ts";
import { AssetCard, DrawScopeSwitch, TextureView } from "./AssetView.tsx";
import { formatBytes } from "./format.ts";
import { LoadingOverlay } from "./LoadingOverlay.tsx";
import { MaterialPreview } from "./MaterialPreview.tsx";
import { Timeline } from "./Timeline.tsx";
import type { Viewport } from "../viewport/viewport3d.ts";
import type { TextureStats } from "../viewport/surfaces.ts";
import { TONE_MAP_CHOICES } from "../viewport/toneMap.ts";
import { countRender } from "./renders.ts";
import type { EditorStore } from "../store.ts";

/**
 * The viewport's texture counts, re-rendering the pane once a frame at
 * most while images arrive. The pane alone: bumping the store for each
 * frame of arrivals rendered the whole editor again, 60 ms a frame on a
 * production stage, while the images waited for the thread.
 */
function useTextureStats(viewport: Viewport | null): TextureStats | undefined {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (viewport === null) return;
    let frame = 0;
    viewport.onImages = () => {
      if (frame !== 0) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        setTick((tick) => tick + 1);
      });
    };
    return () => {
      viewport.onImages = null;
      cancelAnimationFrame(frame);
    };
  }, [viewport]);
  return viewport?.textureStats;
}

export function ViewportPane() {
  countRender("ViewportPane");
  const store = useEditorStore();
  // What is selected and drawn and how it is shown; the counts that move with time are `DrawStats`'s.
  useRevision(store, "selection", "view", "status", "composition", "draws");
  const canvas = useRef<HTMLCanvasElement>(null);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    let dispose = () => {};
    // three.js and the shared WebGPU renderer, loaded and started after the editor.
    const loaded = Promise.all([import("../viewport/viewport3d.ts"), import("../viewport/gpu.ts").then(({ sharedRenderer }) => sharedRenderer())]);
    void loaded.then(
      ([{ Viewport }, renderer]) => {
        if (disposed || canvas.current === null) return;
        try {
          const source = () => store.source;
          const viewport = new Viewport(
            canvas.current,
            renderer,
            async (identifier, anchor) => {
              const scene = source();
              return scene === null ? null : readAsset(scene, store.session?.stage ?? null, identifier, anchor);
            },
            async (identifier, anchor) => {
              const scene = source();
              return scene === null ? false : assetExists(scene, store.session?.stage ?? null, identifier, anchor);
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
  const drawsNothing = !store.scene.items.some((view) => !view.item.hidden);
  const through = store.through;
  // The material view: the selected material (or the one around a selected
  // shader) on a shader ball. By default it shows when the scene draws
  // nothing, as a material library's does; the toggle overrides that.
  const [materialChoice, setMaterialChoice] = useState<boolean | null>(null);
  const stage = store.session?.stage ?? null;
  const material = stage === null || store.selection === null ? null : enclosingMaterial(stage, store.selection);
  // The asset view's texture: the image a selected texture names, in place of the draws.
  const texture = store.drawScope === "selection" && store.focus.kind === "texture" ? store.focus.attribute : null;
  const showMaterial = texture === null && material !== null && (materialChoice ?? drawsNothing);
  const textures = useTextureStats(store.viewport);
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
            const viewport = store.viewport;
            if (viewport === null) return store.select(null);
            // Read from the GPU: the selection follows a frame or so later.
            viewport.pick(event.clientX - rect.left, event.clientY - rect.top).then(
              (path) => {
                store.select(path);
                // An instance of a point instancer: its view's row too.
                const hit = viewport.lastPickedInstance;
                if (hit !== null && hit.instancer === path) store.selectInstanceById(path, hit.id);
              },
              (error: unknown) => console.warn("pick failed", error),
            );
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
        {/* usdview's sRGB colour correction by default; AgX for scenes whose highlights it clips. Previews follow it too. */}
        {store.session !== null && texture === null && (
          <div role="group" aria-label="Tone mapping" className="flex items-center rounded-md bg-black/40 p-0.5 text-[11px] backdrop-blur-sm">
            <span className="px-1.5 text-white/70">Tone mapping:</span>
            {TONE_MAP_CHOICES.map(([choice, label]) => (
              <button
                key={choice}
                type="button"
                aria-pressed={store.toneMap === choice}
                title={choice === "srgb" ? "Light encoded as sRGB with no tone curve, as usdview shows it by default: past white clips" : "AgX's tone curve: highlights roll off instead of clipping"}
                onClick={() => store.setToneMap(choice)}
                className={cn("rounded px-2 py-0.5 text-white/70 hover:text-white", store.toneMap === choice && "bg-white/15 text-white")}
              >
                {label}
              </button>
            ))}
          </div>
        )}
      </div>
      <LoadingOverlay />
      {/* Why the last request to frame framed nothing, for a few seconds. */}
      {store.frameNote !== null && (
        <div
          key={store.frameNote.id}
          role="status"
          className="pointer-events-none absolute top-2 left-1/2 max-w-[60%] -translate-x-1/2 truncate rounded-md bg-black/60 px-2 py-1 font-mono text-[11px] text-white/90 backdrop-blur-sm"
        >
          {store.frameNote.message}
        </div>
      )}
      {textures !== undefined && textures.pending > 0 && store.status.kind !== "opening" && (
        <div role="status" className="pointer-events-none absolute bottom-2 left-2 flex items-center gap-2 rounded-md bg-black/40 px-2 py-1 font-mono text-[11px] text-white/80 backdrop-blur-sm">
          <span className="size-1.5 animate-pulse rounded-full bg-sky-400" />
          Loading textures: {textures.images} of {textures.images + textures.pending}
          {textures.decoding > 0 && ` · ${textures.decoding} decoding in ${textures.decoders} workers`}
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
              <div className={cn("rounded-md bg-black/40 px-2 py-1 font-mono text-[11px] text-white/80 backdrop-blur-sm", through === null && "pointer-events-none")}>
                {through === null ? (
                  "Free camera"
                ) : (
                  <>
                    <div>Looking through {through}</div>
                    <div className="text-white/60">The stage camera's view: orbit and zoom are off.</div>
                    <button type="button" onClick={() => store.lookThrough(null)} className="mt-0.5 rounded bg-white/15 px-1.5 py-0.5 text-white hover:bg-white/25">
                      Return to the free camera
                    </button>
                  </>
                )}
              </div>
            )}
            {store.drawScope === "selection" && !showMaterial && stage !== null && <AssetCard store={store} stage={stage} />}
          </div>
        )}
        {!showMaterial && texture === null && <DrawStats store={store} textures={textures} />}
      </div>
      <Timeline />
    </div>
  );
}

/**
 * What the viewport draws, in numbers, and what the last sync did: the
 * part of the pane that changes with every frame of playback, so it
 * listens for the time and the rest of the pane does not.
 */
function DrawStats({ store, textures }: { store: EditorStore; textures: TextureStats | undefined }) {
  countRender("DrawStats");
  useRevision(store, "time", "draws");
  const summary = viewportSummary(store.scene);
  const update = store.lastUpdate === null ? null : updateSummary(store.lastUpdate);
  const skinning = store.scene.skinning;
  const skinned = skinning.gpu + skinning.cpu + skinning.failed.length > 0;
  return (
    <div className="pointer-events-none absolute right-2 bottom-2 rounded-md bg-black/40 px-2 py-1 text-right font-mono text-[11px] leading-relaxed text-white/80 tabular-nums backdrop-blur-sm">
      <div>
        {summary.drawn.length} prims drawn · {summary.triangles.toLocaleString()} triangles · {summary.lights.length} lights
        {store.cost !== null && store.cost.total.items > 0 && (
          <span title="Bytes the draws upload (compact vertices: 16 per vertex with a position, normal and st; u16 or u32 indices; 64 per instance), and the JavaScript arrays they still hold (a static buffer's are dropped once uploaded); the Draw cost tab lists them by item">
            {" "}· {formatBytes(store.cost.total.gpuBytes)} uploaded · {formatBytes(store.cost.total.cpuBytes)} held
          </span>
        )}
      {textures !== undefined && textures.images > 0 && ` · ${textures.images} images${textures.capped > 0 ? ` (${textures.capped} scaled to ${textures.limit})` : ""}`}
      </div>
      {store.viewport !== null && <ClusterLine viewport={store.viewport} />}
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
  );
}

/**
 * The clustered draws, when there are any: what they hold (every level of
 * their sets, once per mesh) and what the GPU's cut drew in a recent frame,
 * read back every few frames, so it is polled rather than rendered with
 * the store.
 */
function ClusterLine({ viewport }: { viewport: Viewport }) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => setTick((tick) => tick + 1), 500);
    return () => clearInterval(timer);
  }, []);
  const residency = viewport.clusterResidency;
  if (residency.draws === 0) return null;
  const stats = viewport.clusterStats;
  const { updates } = residency;
  const last = updates.last;
  return (
    <div
      className="text-white/60"
      title="Meshes with a prebuilt cluster set are culled and cut on the GPU each frame: the triangles drawn are the cut's, at the pixel error shown; hidden are the clusters (and whole instances) occlusion culling left out; held are every level's, once per mesh. Their buffers are packed whole when a change outgrows them, and otherwise each change writes its own records in place"
    >
      clustered: {residency.meshes} meshes, {residency.draws} draws, {residency.instances.toLocaleString()} instances · {stats === null ? "…" : stats.triangles.toLocaleString()} triangles
      drawn at {viewport.clusterView.pixelError} px{stats !== null && stats.occluded > 0 ? `, ${stats.occluded.toLocaleString()} hidden` : ""} ({residency.triangles.toLocaleString()} held,{" "}
      {formatBytes(residency.gpuBytes)}; packed {updates.full}× (the last in {residency.packMs.toFixed(0)} ms{updates.compactions > 0 ? `, ${updates.compactions} to compact` : ""}),{" "}
      {updates.partial} {updates.partial === 1 ? "change" : "changes"} written in place{last?.kind === "partial" ? ` (the last ${formatBytes(last.bytes)} in ${last.ms.toFixed(1)} ms)` : ""})
    </div>
  );
}
