// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the store reads of the stage as a whole for the Health tab and the
// Stage tab, once per change of the stage rather than per render: the
// stage's health (`Stage.status`, the asset loop's record of who wanted
// each asset, the last move or delete's unrepaired paths) and which layer
// holds each stage metadata opinion (the root's and the session layer's
// own fields, `Stage.layerSpec(layer, "/")`). Each is read when first asked for after a change, and kept
// until the stage reports another, the asset loop takes a step, an undo
// or redo moves the history, an action settles, or another scene opens;
// a time change never reads either.
import type { HistoryStep, Layer } from "@forest-rs/layerstack";
import { type HealthReport, healthReport, wantingPrims } from "../model/health.ts";
import type { Session } from "../model/session.ts";
import { fieldSource, type FieldSource, STAGE_FIELDS } from "../model/stageMetadata.ts";

/**
 * What a report was read at: its session, the changes it had heard, the
 * asset loop's steps (an asset confirmed missing changes no prim, so no
 * change is heard of it), and the newest undo step.
 */
interface Mark {
  readonly session: Session;
  readonly changes: number;
  readonly loop: number;
  readonly step: HistoryStep | null;
}

function same(a: Mark | null, b: Mark): boolean {
  return a !== null && a.session === b.session && a.changes === b.changes && a.loop === b.loop && a.step === b.step;
}

/** Reads the stage's health and its metadata's sources, once per change. */
export class StageReports {
  #healthMark: Mark | null = null;
  #health: HealthReport<Layer> | null = null;
  #sourcesMark: Mark | null = null;
  #sources: ReadonlyMap<string, FieldSource> = new Map();

  /**
   * Forgets what was read: the store calls it as each action settles, for
   * a read made while the action ran may be of a stage half composed (the
   * asset loop reads what was supplied, then composes it, and an asset
   * confirmed missing changes no prim, so no change says the read is old).
   */
  forget(): void {
    this.#healthMark = null;
    this.#sourcesMark = null;
  }

  /** The stage's health ({@link healthReport}), read again only after a change. */
  health(session: Session): HealthReport<Layer> {
    const mark = markOf(session);
    if (same(this.#healthMark, mark) && this.#health !== null) return this.#health;
    this.#healthMark = mark;
    this.#health = readHealth(session);
    return this.#health;
  }

  /** Which layer each of {@link STAGE_FIELDS} is read from, read again only after a change. */
  metadataSources(session: Session): ReadonlyMap<string, FieldSource> {
    const mark = markOf(session);
    if (same(this.#sourcesMark, mark)) return this.#sources;
    this.#sourcesMark = mark;
    const stage = session.stage;
    const sessionLayer = session.sessionLayer;
    const root = fieldsOf(session, stage.root);
    const front = sessionLayer === null ? null : fieldsOf(session, sessionLayer);
    this.#sources = new Map(STAGE_FIELDS.map((field) => [field.key, fieldSource(field.key, front, root)]));
    return this.#sources;
  }
}

function markOf(session: Session): Mark {
  return { session, changes: session.changeLog.length, loop: session.loopLog.length, step: session.history.nextUndo };
}

/** The metadata fields `layer` itself authors (its pseudo-root's, by their OpenUSD keys): one spec read, whatever the layer's size. */
function fieldsOf(session: Session, layer: Layer): Set<string> {
  return new Set(session.stage.layerSpec(layer, "/").fields);
}

/** Reads what {@link healthReport} sorts: the stage's status, the loop's record, the last move or delete. */
function readHealth(session: Session): HealthReport<Layer> {
  const stage = session.stage;
  const status = stage.status();
  const last = session.lastNamespaceEdit();
  return healthReport<Layer>({
    prims: status.prims,
    errors: status.compositionErrors,
    unresolved: status.unresolved.map(({ assetPath, anchor, usedBy, identifier, reason }) => ({ assetPath, anchor, usedBy, identifier, reason })),
    unavailable: status.unavailable.map(({ assetPath, anchor, usedBy, identifier, prims }) => ({ assetPath, anchor, usedBy, identifier, reason: null, prims })),
    wanting: wantingPrims(session.loops.runs),
    lastNamespaceEdit:
      last === null
        ? null
        : {
            label: last.label,
            unrepaired: last.receipt.unrepaired.map(({ layer, spec, field, path, metadata }) => ({ layer, spec, field, path, metadata })),
          },
    layers: stage.layers().length,
  });
}
