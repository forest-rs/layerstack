// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the store keeps for authoring composition: the last composition
// edit (what the inspector offers to show in its layer's text), the specs
// the inspector asks for, each read once per change of the stage
// (`Stage.layerSpec`, `../model/layerSpecs.ts`), and what an arc can be
// pointed at: the layers the source lists and the stage holds, and a
// layer's default and root prims. The actions that author run on the
// store (`../store.ts`), in its turn, as any edit.
import type { HistoryStep, Layer, Layerstack } from "@forest-rs/layerstack";
import { type LayerSummary, layerSummary as summarizeLayer, SpecCache, summarizeBytes } from "../model/layerSpecs.ts";
import type { StrongerSelection } from "../model/composition.ts";
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
  /**
   * The variant it added, its selection authored where it went, when it
   * did: offered to edit inside; or, when a stronger layer's selection of
   * the set still wins (`outranked`), offered to select there.
   */
  readonly variant?: { readonly set: string; readonly variant: string; readonly outranked?: StrongerSelection };
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

/** The store's composition state: the last composition edit, and the specs read since the stage last changed. */
export class CompositionState {
  /** The last composition edit made, whichever step is newest now. */
  edit: CompositionEdit | null = null;
  #specs: { readonly session: Session; readonly stamp: string; readonly cache: SpecCache } | null = null;

  /**
   * The layers' specs of `session` as read since `stamp` (a change of the
   * stage): each layer and path read once, the same object each time it
   * is asked until the stamp moves on.
   */
  specs(session: Session, stamp: string): SpecCache {
    let specs = this.#specs;
    if (specs === null || specs.session !== session || specs.stamp !== stamp) this.#specs = specs = { session, stamp, cache: new SpecCache(session.stage) };
    return specs.cache;
  }
}

/** The layers an arc can be pointed at: those `source` lists and those the stage holds, by identifier, sorted. */
export async function arcLayers(session: Session, source: SceneSource | null): Promise<string[]> {
  const held = session.stage.layers().flatMap((layer) => (layer.identifier === null || layer.identifier.includes("[") ? [] : [layer.identifier]));
  const listed = (await source?.layers?.().catch(() => [])) ?? [];
  return [...new Set([...listed, ...held])].sort();
}

/**
 * The default prim and root prims of the layer `identifier`: its own
 * specs when the stage holds it, else read from `source` (a layer over
 * {@link SUMMARY_BYTES} is not, and the summary says why). Throws what the
 * module throws.
 */
export async function layerSummary(session: Session, source: SceneSource, layerstack: Layerstack, identifier: string): Promise<LayerSummary> {
  const held = session.stage.layers().find((layer) => layer.identifier === identifier);
  if (held !== undefined) return summarizeLayer(session.stage, held);
  const size = await source.size(identifier);
  if (size !== null && size > SUMMARY_BYTES) return { kind: "unread", reason: `${identifier} is ${(size / 1024 ** 2).toFixed(1)} MB: too large to read for its prims alone.` };
  const bytes = await source.fetch(identifier);
  if (bytes === null) return { kind: "unread", reason: `The scene's source has no ${identifier}.` };
  if (bytes.byteLength > SUMMARY_BYTES) return { kind: "unread", reason: `${identifier} is too large to read for its prims alone.` };
  return summarizeBytes(layerstack, identifier, bytes);
}
