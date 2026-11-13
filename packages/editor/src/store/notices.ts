// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the editor tells the person that is not the scene: what an action
// reported (a refused edit, a failed fetch), kept until dismissed, and why
// a request to frame framed nothing, shown for a few seconds.
import type { UnrepairedPath } from "@forest-rs/layerstack";
import type { Signals } from "./signals.ts";

/** Something an action reported that the person should see: a refused edit, a failed fetch. */
export interface Notice {
  readonly id: number;
  readonly action: string;
  readonly message: string;
  /** The error's code, for a `LayerstackError` (`edit.invalid`, `value.kind`). */
  readonly code: string | null;
  /** The paths a move or delete left naming what it moved (`EditReceipt.unrepaired`); empty for other notices. */
  readonly unrepaired: readonly UnrepairedPath[];
}

/** What actions reported, newest last; bumps `notices` as it changes. */
export class Notices {
  /** The notices not yet dismissed, oldest first. */
  readonly list: Notice[] = [];
  readonly #signals: Signals;
  #next = 0;

  constructor(signals: Signals) {
    this.#signals = signals;
  }

  /** Adds a notice that `action` reported `message` (with the paths it left unrepaired, if any); returns it. */
  add(action: string, message: string, code: string | null, unrepaired: readonly UnrepairedPath[] = []): Notice {
    const notice: Notice = { id: ++this.#next, action, message, code, unrepaired };
    this.list.push(notice);
    this.#signals.bump("notices");
    return notice;
  }

  /** Adds a notice of the `error` that `action` threw: its message, and its code when it has one; returns it. */
  report(action: string, error: unknown): Notice {
    const code = typeof error === "object" && error !== null && "code" in error ? String((error as { code: unknown }).code) : null;
    return this.add(action, error instanceof Error ? error.message : String(error), code);
  }

  /** Forgets the notice with `id`. */
  dismiss(id: number): void {
    const index = this.list.findIndex((notice) => notice.id === id);
    if (index >= 0) this.list.splice(index, 1);
    this.#signals.bump("notices");
  }
}

/** Why a request to frame framed nothing, shown briefly in the viewport. */
export interface FrameNote {
  /** Distinct for each request, so asking again shows it again. */
  readonly id: number;
  /** `Nothing to frame: /World/Looks/Checker has no bounds`. */
  readonly message: string;
}

/** How long a {@link FrameNote} is shown, in milliseconds. */
export const FRAME_NOTE_MS = 3000;

/** The note of the last request to frame that framed nothing; bumps `view` as it comes and goes. */
export class FrameNotes {
  /** The note shown; `null` when the last request framed, or the note has faded. */
  note: FrameNote | null = null;
  readonly #signals: Signals;
  #timer: ReturnType<typeof setTimeout> | undefined = undefined;
  #next = 0;

  constructor(signals: Signals) {
    this.#signals = signals;
  }

  /**
   * Shows why a request to frame framed nothing (`what` has no bounds, and
   * a `hint`) for {@link FRAME_NOTE_MS}, or clears the last note when it
   * framed (or `what` is `null`: nothing to say). Returns `framed`.
   */
  show(framed: boolean, what: string | null, hint: string | null = null): boolean {
    clearTimeout(this.#timer);
    this.#timer = undefined;
    const message = `Nothing to frame: ${what} has no bounds${hint === null ? "" : `; ${hint}`}`;
    const note = framed || what === null ? null : { id: ++this.#next, message };
    if (note === null && this.note === null) return framed;
    this.note = note;
    if (note !== null) {
      this.#timer = setTimeout(() => {
        this.#timer = undefined;
        this.note = null;
        this.#signals.bump("view");
      }, FRAME_NOTE_MS);
    }
    this.#signals.bump("view");
    return framed;
  }
}
