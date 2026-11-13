// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Opening a scene: the session made and resolved while the editor keeps
// the scene it has, each phase shown before its work holds the page, the
// payloads loaded within a budget, and a way to stop waiting. The store
// is handed the session once it is whole; until then nothing of the open
// scene changes.
import type { Layerstack } from "@forest-rs/layerstack";
import { type LoadBudget, type OpenPhase, OverBudget, Session } from "../model/session.ts";
import type { SceneSource } from "../model/sources.ts";
import type { Signals } from "./signals.ts";
import type { EditorStatus } from "./status.ts";

/**
 * How a scene opens its payloads: all of them, none (each prim with
 * payloads then loads from the prim tree), or `auto`: none, then all of
 * them unless reading them would pass {@link PAYLOAD_BUDGET}.
 */
export type PayloadChoice = "auto" | "all" | "none";

/**
 * The layer bytes an automatic open reads for payloads before it leaves
 * them unloaded: 1 GiB. Composing takes more memory than the layers' bytes
 * (a large public scene's 3.1 GB of layers composes to 5.4 GB natively),
 * and the module has 4 GiB for the stage and its draws, so a scene whose
 * payloads read more than this would not fit with them loaded.
 */
export const PAYLOAD_BUDGET = 1024 ** 3;

/** What an open, or a payload load, is doing, and since when (`performance.now()`). */
export interface BusyPhase {
  readonly phase: OpenPhase;
  /** When the phase began. */
  readonly since: number;
  /** When the open, or the load, began. */
  readonly started: number;
}

/** What {@link OpenController} asks of the store. */
export interface OpenHost {
  /** Whether a scene is open: what a cancelled or failed open goes back to. */
  readonly hasSession: boolean;
  /** Shows where the editor is with its scene. */
  setStatus(status: EditorStatus): void;
  /** Takes the opened session as the scene, freeing the one it had; `note` says why its payloads were left unloaded. */
  commit(session: Session, source: SceneSource, note: string | null): void;
  /** Stops the editor if `error` is a module's trap; whether it was one. */
  lose(error: unknown, label: string | null): boolean;
}

/** Opens scenes, one at a time: a new open supersedes the one in flight. */
export class OpenController {
  /** How the next scene opens its payloads. */
  payloads: PayloadChoice = "auto";
  /** The session being opened, while its assets are fetched. */
  opening: Session | null = null;
  /** What the open is doing; `null` when nothing is opening. */
  phase: BusyPhase | null = null;
  readonly #layerstack: Layerstack;
  readonly #signals: Signals;
  readonly #host: OpenHost;
  /** The open in flight: one that is no longer it is dropped when it comes back. */
  #serial = 0;
  /** When the open, or the payload load, the overlay shows began. */
  #started = 0;
  #frame = 0;

  constructor(layerstack: Layerstack, signals: Signals, host: OpenHost) {
    this.#layerstack = layerstack;
    this.#signals = signals;
    this.#host = host;
  }

  /** The source's root, opened and resolved: everything it depends on is fetched first. */
  async open(source: SceneSource): Promise<void> {
    const serial = ++this.#serial;
    const current = () => serial === this.#serial;
    this.#host.setStatus({ kind: "opening", label: source.label });
    try {
      this.begin();
      const choice = this.payloads;
      const session = await Session.open(this.#layerstack, source.root, source.fetch, {
        open: { load: choice === "all" ? "all" : "none" },
        progress: (pending) => {
          if (!current()) return;
          if (this.opening !== pending) {
            this.opening = pending;
            this.#signals.bump("status");
          }
          this.#progressed();
        },
        // Each phase is shown before its work holds the thread: two frames
        // let the page render and paint it.
        phase: async (phase) => {
          if (!current()) return;
          this.phase = { phase, since: performance.now(), started: this.#started };
          this.#signals.bump("status", "loop");
          await nextFrame();
          await nextFrame();
        },
      });
      if (!current()) {
        session.free();
        return;
      }
      const note = choice === "auto" ? await loadWithin(session, source) : null;
      if (!current()) {
        session.free();
        return;
      }
      this.opening = null;
      this.phase = null;
      this.#host.commit(session, source, note);
    } catch (error) {
      if (!current()) return;
      this.opening = null;
      this.phase = null;
      if (this.#host.lose(error, source.label)) return;
      this.#host.setStatus({ kind: "failed", label: source.label, message: error instanceof Error ? error.message : String(error) });
    }
  }

  /**
   * Stops waiting for the scene being opened: the editor keeps the scene
   * it had. Work already holding the thread finishes, and its result is
   * dropped.
   */
  cancel(): void {
    this.#serial += 1;
    this.opening = null;
    this.phase = null;
    this.#host.setStatus(this.#host.hasSession ? { kind: "ready" } : { kind: "empty" });
  }

  /**
   * Starts the clock the phases are timed from, with no phase yet: an
   * open begins, or a payload load of the open scene, whose rounds the
   * session reports through the same callbacks.
   */
  begin(): void {
    this.#started = performance.now();
    this.phase = null;
  }

  /** Nothing is opening or loading any more. */
  end(): void {
    this.opening = null;
    this.phase = null;
  }

  /** Sets how the next scene opens its payloads. */
  setPayloads(choice: PayloadChoice): void {
    this.payloads = choice;
    this.#signals.bump("status");
  }

  /** Tells the panels the asset loop progressed, once a frame at most: it reports every fetch. */
  #progressed(): void {
    if (this.#frame !== 0) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      this.#signals.bump("loop");
    });
  }
}

/**
 * Loads every payload of `session`, opened with none, unless that would
 * read more than {@link PAYLOAD_BUDGET} of layers (weighed before each
 * round is fetched, from the source's sizes); then they are unloaded
 * again. Returns the note that says why, or `null` when they loaded.
 */
async function loadWithin(session: Session, source: SceneSource): Promise<string | null> {
  if (session.stage.findLoadable().length === 0) return null;
  const budget: LoadBudget = { bytes: PAYLOAD_BUDGET, size: (identifier) => source.size(identifier) };
  try {
    await session.loadPayloads(["/"], [], "withDescendants", budget);
    return null;
  } catch (error) {
    if (!(error instanceof OverBudget)) throw error;
    session.stage.setLoadRules("none");
    return `Payloads are unloaded: loading them all would read ${formatGiB(error.bytes)} of layers, over the ${formatGiB(PAYLOAD_BUDGET)} an automatic open reads. Load prims from the prim tree.`;
  }
}

/** Resolves after the next frame is rendered. */
function nextFrame(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

/** `bytes` in GiB, to one decimal. */
function formatGiB(bytes: number): string {
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}
