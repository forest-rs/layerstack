// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the store keeps for authoring composition: the last composition
// edit (what the inspector offers to show in its layer's text), what each
// layer authors about composition, read from its text once per change of
// the stage (`../model/layerSpecs.ts`), and what an arc can be pointed at:
// the layers the source lists and the stage holds, and a layer's default
// and root prims. The actions that author run on the store
// (`../store.ts`), in its turn, as any edit.
import type { HistoryStep, Layer, Layerstack } from "@forest-rs/layerstack";
import { type LayerSpecs, type LayerSummary, readLayerSpecs, summarizeBytes, summaryOf } from "../model/layerSpecs.ts";
import type { LayerTextRead } from "../model/layerText.ts";
import type { Session } from "../model/session.ts";
import type { SceneSource } from "../model/sources.ts";

/** A composition edit made in the store ({@link EditorStore.compositionEdit}): what the inspector offers to show. */
export interface CompositionEdit {
  /** The prim it edited. */
  readonly prim: string;
  /** The action, as History lists it. */
  readonly label: string;
  /** The layer it wrote. */
  readonly layer: Layer;
  /** The step it made: the edit is offered while this is the newest. */
  readonly step: HistoryStep;
  /** The variant it added and selected, when it did: offered to edit inside. */
  readonly variant?: { readonly set: string; readonly variant: string };
}

/**
 * An action that may wait on its assets: its refusal, known at once when
 * it ran now (`undefined` when the stage took it, or when it waits its
 * turn: a refusal is then a notice alone), and its end.
 */
export interface Authoring {
  readonly refusal: string | undefined;
  readonly done: Promise<void>;
}

/** The most bytes of a layer the stage has not read that are read for its default and root prims alone. */
export const SUMMARY_BYTES = 16 * 1024 ** 2;

/** The store's composition state: the last composition edit, and the layers' specs as read. */
export class CompositionState {
  /** The last composition edit made, whichever step is newest now. */
  edit: CompositionEdit | null = null;
  #specs: { session: Session | null; stamp: string; read: Map<Layer, LayerSpecs | null>; large: Set<Layer> } = {
    session: null,
    stamp: "",
    read: new Map(),
    large: new Set(),
  };

  /**
   * What `layer` of `session` authors about composition, read through
   * `read` (its text) once per `stamp` (a change of the stage); `null`
   * when its text is too large to read whole (the Text tab's limit; such
   * a layer is not written as text for this again), or cannot be written.
   */
  specs(session: Session, stamp: string, layer: Layer, read: (layer: Layer) => LayerTextRead | string): LayerSpecs | null {
    if (this.#specs.session !== session) this.#specs = { session, stamp, read: new Map(), large: new Set() };
    else if (this.#specs.stamp !== stamp) this.#specs = { ...this.#specs, stamp, read: new Map() };
    if (this.#specs.large.has(layer)) return null;
    const cached = this.#specs.read.get(layer);
    if (cached !== undefined) return cached;
    const text = read(layer);
    if (typeof text !== "string" && text.truncated) this.#specs.large.add(layer);
    const specs = typeof text === "string" || text.truncated ? null : readLayerSpecs(text.lines.join("\n"));
    this.#specs.read.set(layer, specs);
    return specs;
  }
}

/** The layers an arc can be pointed at: those `source` lists and those the stage holds, by identifier, sorted. */
export async function arcLayers(session: Session, source: SceneSource | null): Promise<string[]> {
  const held = session.stage.layers().flatMap((layer) => (layer.identifier === null || layer.identifier.includes("[") ? [] : [layer.identifier]));
  const listed = (await source?.layers?.().catch(() => [])) ?? [];
  return [...new Set([...listed, ...held])].sort();
}

/**
 * The default prim and root prims of the layer `identifier`: from its
 * specs (`specsOf`) when the stage holds it, else read from `source` (a
 * layer over {@link SUMMARY_BYTES} is not, and the summary says why).
 * Throws what the module throws.
 */
export async function layerSummary(
  session: Session,
  source: SceneSource,
  layerstack: Layerstack,
  identifier: string,
  specsOf: (layer: Layer) => LayerSpecs | null,
): Promise<LayerSummary> {
  const held = session.stage.layers().find((layer) => layer.identifier === identifier);
  if (held !== undefined) {
    const specs = specsOf(held);
    return specs === null ? { kind: "unread", reason: `${identifier} is too large to read for its prims here.` } : summaryOf(specs);
  }
  const size = await source.size(identifier);
  if (size !== null && size > SUMMARY_BYTES) return { kind: "unread", reason: `${identifier} is ${(size / 1024 ** 2).toFixed(1)} MB: too large to read for its prims alone.` };
  const bytes = await source.fetch(identifier);
  if (bytes === null) return { kind: "unread", reason: `The scene's source has no ${identifier}.` };
  if (bytes.byteLength > SUMMARY_BYTES) return { kind: "unread", reason: `${identifier} is too large to read for its prims alone.` };
  return summarizeBytes(layerstack, identifier, bytes);
}
