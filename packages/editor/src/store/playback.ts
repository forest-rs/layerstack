// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The time everything is read at, the stage's range, and playing through
// it a frame at a time. It knows nothing of the draws: it is told what to
// do when the time moves.
import type { Stage } from "@forest-rs/layerstack";
import { rememberedLoop } from "../model/playbackLoop.ts";

/** The stage's frames, as its metadata gives them. */
export interface TimeRange {
  /** `startTimeCode` and `endTimeCode`. */
  readonly start: number;
  readonly end: number;
  /** `timeCodesPerSecond`: how fast time codes pass when playing. */
  readonly timeCodesPerSecond: number;
}

/** The stage's time range from its metadata, `null` when it authors no span (`endTimeCode` not after `startTimeCode`). */
export function timeRange(stage: Stage): TimeRange | null {
  const number = (key: string, fallback: number) => {
    const value = stage.metadata(key);
    return value?.kind === "scalar" && typeof value.value === "number" ? value.value : fallback;
  };
  const start = number("startTimeCode", 0);
  const end = number("endTimeCode", 0);
  if (!(end > start)) return null;
  return { start, end, timeCodesPerSecond: number("timeCodesPerSecond", 24) };
}

/** The time, and playback through the stage's range. */
export class Playback {
  /**
   * The time everything is read at: a time code, or `undefined` for the
   * default time (a stage without a time range).
   */
  time: number | undefined = undefined;
  /** The stage's time range; `null` when it authors none. */
  range: TimeRange | null = null;
  /** Whether time is playing. */
  playing = false;
  /** Whether playing starts over at the start of the range after its end, or stops at the end; this viewer's choice, remembered. */
  loop = rememberedLoop();
  readonly #moved: () => void;
  readonly #toggled: () => void;
  #frame = 0;

  /**
   * `moved` is called after the time changes (it brings the draws to it
   * and tells the panels); `toggled` after playing starts.
   */
  constructor(moved: () => void, toggled: () => void) {
    this.#moved = moved;
    this.#toggled = toggled;
  }

  /** Stops, and starts over at the start of `range`: another scene. Tells no one; the caller does. */
  reset(range: TimeRange | null): void {
    this.stop();
    this.range = range;
    this.time = range?.start;
  }

  /**
   * Takes the stage's range as it is now (an edit to its metadata, or an
   * undo, changed it): the time is brought into it, to its start when
   * there was no range before, and to the default time when there is
   * none now. Tells no one; returns whether the range or the time
   * changed.
   */
  setRange(range: TimeRange | null): boolean {
    const was = this.range;
    if (was?.start === range?.start && was?.end === range?.end && was?.timeCodesPerSecond === range?.timeCodesPerSecond) return false;
    this.range = range;
    if (range === null) {
      this.stop();
      this.time = undefined;
    } else {
      this.time = this.time === undefined ? range.start : Math.min(range.end, Math.max(range.start, this.time));
    }
    return true;
  }

  /** Reads everything at `time` (clamped to the range). */
  setTime(time: number | undefined): void {
    const range = this.range;
    const clamped = time === undefined || range === null ? time : Math.min(range.end, Math.max(range.start, time));
    if (clamped === this.time) return;
    this.time = clamped;
    this.#moved();
  }

  /**
   * Plays the range from the current time at `timeCodesPerSecond`, then
   * starts over at its start ({@link Playback.loop}) or stops at its end;
   * playing from the end, not looping, starts at the start.
   */
  play(): void {
    if (this.range === null || this.playing) return;
    if (!this.loop && (this.time ?? this.range.start) >= this.range.end) this.time = this.range.start;
    this.playing = true;
    let last = performance.now();
    const tick = (now: number) => {
      // The range is read each frame: an edit may have changed it.
      const range = this.range;
      if (!this.playing || range === null) return;
      const advanced = (this.time ?? range.start) + ((now - last) / 1000) * range.timeCodesPerSecond;
      last = now;
      if (advanced > range.end && !this.loop) {
        // Stopped on the last frame, a whole time code as the pause button leaves it.
        this.playing = false;
        this.time = range.end;
        this.#moved();
        this.#toggled();
        return;
      }
      this.time = advanced > range.end ? range.start : advanced;
      this.#moved();
      this.#frame = requestAnimationFrame(tick);
    };
    this.#frame = requestAnimationFrame(tick);
    this.#toggled();
  }

  /** Stops playing, on the nearest whole time code: an edit then authors its sample on a frame. */
  pause(): void {
    if (!this.playing) return;
    this.stop();
    if (this.time !== undefined) this.time = Math.round(this.time);
    this.#moved();
  }

  /** Moves time by `frames` whole time codes, pausing. */
  step(frames: number): void {
    this.pause();
    if (this.range === null) return;
    this.setTime(Math.round(this.time ?? this.range.start) + frames);
  }

  /** Stops playing where it is, telling no one: the module is lost, or the scene is going. */
  stop(): void {
    this.playing = false;
    cancelAnimationFrame(this.#frame);
  }
}
