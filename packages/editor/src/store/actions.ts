// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// One action on the stage at a time, in the order asked. An action that
// fetches (an undo that brings back a reference, a variant, a payload)
// waits on the host between steps on the stage; another action run in
// that wait would edit the stage under it and run a second asset loop
// into the first. So actions queue: one asked for while another waits
// runs when that one is done.
//
// A queue rather than refusing while busy, because what is asked for is
// then never lost: a second Ctrl+Z pressed while the first undo fetches
// undoes a second step, where a busy flag would drop it, or have every
// control disabled for a wait the person cannot see the reason of. An
// action that does not wait (an edit) runs at once when nothing is
// running, before `run` returns, so the common case is as direct as a
// call.

/** One action waiting its turn. */
interface Waiting {
  readonly label: string;
  readonly job: (signal: AbortSignal) => void | Promise<void>;
  /** Called once the job has run, or been dropped. */
  readonly done: () => void;
}

/** The store's actions, run one at a time in the order asked. */
export class ActionQueue {
  readonly #waiting: Waiting[] = [];
  #running: { readonly label: string; readonly abort: AbortController } | null = null;
  #idle: (() => void)[] = [];
  readonly #changed: () => void;

  /** `changed` is called when an action starts or stops waiting on the host, or one queues behind it ({@link ActionQueue.running} or {@link ActionQueue.waiting} changed). */
  constructor(changed: () => void) {
    this.#changed = changed;
  }

  /** The label of the action waiting on the host; `null` when none is. */
  get running(): string | null {
    return this.#running?.label ?? null;
  }

  /** How many actions wait behind the one running. */
  get waiting(): number {
    return this.#waiting.length;
  }

  /**
   * Runs `job` when its turn comes: now, before this returns, when no
   * action is running; else after those asked for before it. The promise
   * resolves once it has run (or was dropped by {@link ActionQueue.cancel}).
   * `job` is given a signal that aborts when it is cancelled, handles its
   * own failures, and must not throw.
   */
  run(label: string, job: (signal: AbortSignal) => void | Promise<void>): Promise<void> {
    return new Promise((done) => {
      this.#waiting.push({ label, job, done });
      this.#drain();
      // Queued behind the action waiting on the host.
      if (this.#waiting.length > 0) this.#changed();
    });
  }

  /** Aborts the action running, if any, leaving those waiting behind it. */
  cancelRunning(): void {
    this.#running?.abort.abort(new DOMException(`${this.#running.label} was cancelled`, "AbortError"));
  }

  /**
   * Aborts the action running and drops those waiting (the scene they
   * were for is going), and resolves once nothing is running.
   */
  cancel(): Promise<void> {
    for (const { done } of this.#waiting.splice(0)) done();
    this.cancelRunning();
    if (this.#running === null) return Promise.resolve();
    return new Promise((resolve) => this.#idle.push(resolve));
  }

  /** Runs the waiting actions until one waits on the host, or none is left. */
  #drain(): void {
    while (this.#running === null) {
      const next = this.#waiting.shift();
      if (next === undefined) break;
      const abort = new AbortController();
      this.#running = { label: next.label, abort };
      let waits: void | Promise<void>;
      try {
        waits = next.job(abort.signal);
      } catch (error) {
        this.#running = null;
        next.done();
        throw error;
      }
      if (!(waits instanceof Promise)) {
        this.#running = null;
        next.done();
        continue;
      }
      this.#changed();
      const finish = () => {
        this.#running = null;
        next.done();
        this.#drain();
        // After the drain, so what it ran of the queue is counted.
        this.#changed();
      };
      waits.then(finish, finish);
      return;
    }
    if (this.#running === null) for (const resolve of this.#idle.splice(0)) resolve();
  }
}
