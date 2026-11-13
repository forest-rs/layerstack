// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Opening a scene: the session made and resolved while the editor keeps
// the scene it has, each phase shown before its work holds the page, the
// payloads loaded within a budget, and a way to stop: cancelling aborts
// the open's fetches and its asset loop, and frees what it had made. The
// store is handed the session once it is whole; until then nothing of
// the open scene changes.
import type { Layerstack } from "@forest-rs/layerstack";
import { type LoadBudget, type OpenPhase, OverBudget, Session } from "../model/session.ts";
import type { SceneSource } from "../model/sources.ts";
import type { EditLayerChoice } from "./files.ts";
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
  /**
   * Ends what is running on the scene the editor has (an action waiting
   * on its assets is aborted, those behind it dropped) and resolves once
   * nothing is: the scene can then be let go.
   */
  settle(): Promise<void>;
  /** Takes the opened session as the scene, freeing the one it had; `note` says why its payloads were left unloaded. */
  commit(session: Session, source: SceneSource, note: string | null): void;
  /** Stops the editor if `error` is a module's trap; whether it was one. */
  lose(error: unknown, label: string | null): boolean;
}

/** Opens scenes, one at a time: a new open supersedes the one in flight. */
export class OpenController {
  /** How the next scene opens its payloads. */
  payloads: PayloadChoice = "auto";
  /** Where the next scene's edits go ({@link EditLayerChoice}). */
  editLayer: EditLayerChoice = "root";
  /** The session being opened, while its assets are fetched. */
  opening: Session | null = null;
  /** What the open is doing; `null` when nothing is opening. */
  phase: BusyPhase | null = null;
  readonly #layerstack: Layerstack;
  readonly #signals: Signals;
  readonly #host: OpenHost;
  /** The open in flight, by what aborts it; `null` when none is. */
  #inFlight: AbortController | null = null;
  /** The open that made the scene the editor has: its session reports the phases of later loads through the same callbacks. */
  #committed: AbortController | null = null;
  /** When the open, or the payload load, the overlay shows began. */
  #started = 0;
  #frame = 0;

  constructor(layerstack: Layerstack, signals: Signals, host: OpenHost) {
    this.#layerstack = layerstack;
    this.#signals = signals;
    this.#host = host;
  }

  /**
   * The source's root, opened and resolved: everything it depends on is
   * fetched first. An open in flight is aborted: this one supersedes it.
   */
  async open(source: SceneSource): Promise<void> {
    this.#inFlight?.abort(new DOMException("another scene is being opened", "AbortError"));
    const abort = new AbortController();
    this.#inFlight = abort;
    const signal = abort.signal;
    // The session's callbacks are heard while it is the one opening, or the scene.
    const heard = () => this.#inFlight === abort || this.#committed === abort;
    this.begin();
    this.#host.setStatus({ kind: "opening", label: source.label });
    let session: Session | null = null;
    try {
      const choice = this.payloads;
      session = await Session.open(this.#layerstack, source.root, source.fetch, {
        open: { load: choice === "all" ? "all" : "none", editLayer: this.editLayer },
        signal,
        progress: (pending) => {
          if (!heard()) return;
          if (this.#inFlight === abort && this.opening !== pending) {
            this.opening = pending;
            this.#signals.bump("status");
          }
          this.#progressed();
        },
        // Each phase is shown before its work holds the thread: two frames
        // let the page render and paint it.
        phase: async (phase) => {
          if (!heard()) return;
          this.phase = { phase, since: performance.now(), started: this.#started };
          this.#signals.bump("status", "loop");
          await nextFrame();
          await nextFrame();
        },
      });
      const note = choice === "auto" ? await loadWithin(session, source, signal) : null;
      signal.throwIfAborted();
      // Nothing may wait on the scene that is about to be freed.
      await this.#host.settle();
      signal.throwIfAborted();
      this.#inFlight = null;
      this.#committed = abort;
      this.opening = null;
      this.phase = null;
      // The scene's from here on: not this open's to free.
      const made = session;
      session = null;
      this.#host.commit(made, source, note);
    } catch (error) {
      // An open that made its session and was then stopped lets it go.
      try {
        session?.free();
      } catch (freeing) {
        this.#host.lose(freeing, source.label);
      }
      // A trap is the module's end whether or not anyone still waits for this open.
      if (this.#host.lose(error, source.label)) return;
      // Cancelled or superseded: whoever did it said where the editor is now.
      if (this.#inFlight !== abort) return;
      this.#inFlight = null;
      this.opening = null;
      this.phase = null;
      this.#host.setStatus({ kind: "failed", label: source.label, message: error instanceof Error ? error.message : String(error) });
    }
  }

  /**
   * Stops the scene being opened: its fetches are aborted, its asset loop
   * stops where it waits, and what it made is freed; the editor keeps the
   * scene it had. Work already holding the thread finishes first.
   */
  cancel(): void {
    this.abort();
    this.#host.setStatus(this.#host.hasSession ? { kind: "ready" } : { kind: "empty" });
  }

  /** Aborts the open in flight, if any, saying nothing of where the editor is: the module is lost. */
  abort(): void {
    this.#inFlight?.abort(new DOMException("the open was cancelled", "AbortError"));
    this.#inFlight = null;
    this.opening = null;
    this.phase = null;
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

  /** Sets where the next scene's edits go. */
  setEditLayer(choice: EditLayerChoice): void {
    this.editLayer = choice;
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
 * `signal` aborts the loading, which then rejects.
 */
async function loadWithin(session: Session, source: SceneSource, signal: AbortSignal): Promise<string | null> {
  if (session.stage.findLoadable().length === 0) {
    // Nothing to weigh: the rules say so, as a scene opened loading all has them. Rules of none refuse a rename.
    session.stage.setLoadRules("all");
    return null;
  }
  const budget: LoadBudget = { bytes: PAYLOAD_BUDGET, size: (identifier) => source.size(identifier) };
  try {
    await session.loadPayloads(["/"], [], "withDescendants", budget, signal);
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
