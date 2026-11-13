// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where the selected prim's keys are, drawn under the timeline's track: a
// tick for each time any of its attributes has a sample, and the explained
// property's own samples taller and in the accent colour. The times are read
// from the stage once for a selection and a composition (`useKeyTimes`),
// and the ticks are laid out once for a width, so the layer is left alone
// while the timeline plays.
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Button } from "react-aria-components";
import { NO_KEYS, selectionKeys, thinTicks, type KeyTimes } from "../model/keyTimes.ts";
import type { TimeRange } from "../store/playback.ts";
import type { EditorStore } from "../store.ts";
import { useRevision } from "./context.ts";
import { formatTime } from "./format.ts";
import { countRender } from "./renders.ts";

/**
 * The keys of the selected prim and explained property within `range`.
 * Read again only when the selection, the explained property, the
 * composition or the range changes, never as time moves; the result is the
 * same object until then.
 */
export function useKeyTimes(store: EditorStore, range: TimeRange | null): KeyTimes {
  const revision = useRevision(store, "selection", "composition");
  const stage = store.session?.stage;
  const prim = store.selection;
  const property = store.property;
  const start = range?.start;
  const end = range?.end;
  return useMemo(() => {
    if (stage === undefined || start === undefined || end === undefined) return NO_KEYS;
    return selectionKeys(stage, prim, property, { start, end });
    // `revision` stands for the composition the stage is in.
  }, [stage, prim, property, start, end, revision]);
}

/** The pixels the slider's thumb is wide: its centre travels the track less this. */
const THUMB = 12;

/** The ticks under the track; pressing one moves the time to its key. */
export const KeyTicks = memo(function KeyTicks({ keys, range, seek }: { keys: KeyTimes; range: TimeRange; seek: (time: number) => void }) {
  countRender("KeyTicks");
  const root = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const element = root.current;
    if (element === null) return;
    setWidth(element.clientWidth);
    const observer = new ResizeObserver(() => setWidth(element.clientWidth));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const track = Math.max(0, width - THUMB);
  const own = useMemo(() => new Set(keys.property), [keys.property]);
  const ticks = useMemo(() => thinTicks(keys.all, range.start, range.end, track), [keys.all, range.start, range.end, track]);
  return (
    <div ref={root} className="pointer-events-none absolute inset-x-0 bottom-0 h-3">
      {ticks.map((tick) => {
        const property = own.has(tick.time);
        return (
          <Button
            key={tick.x}
            excludeFromTabOrder
            aria-label={`${property ? "Property key" : "Key"} at ${formatTime(tick.time)}`}
            onPress={() => seek(tick.time)}
            style={{ left: THUMB / 2 + tick.x - 2 }}
            className="group pointer-events-auto absolute bottom-0 flex h-3 w-[5px] cursor-pointer items-end justify-center outline-none"
          >
            <span title={`${property ? "Property key" : "Key"} at ${formatTime(tick.time)}: go there`} className={property ? "h-3 w-[2px] rounded-full bg-primary group-hover:bg-foreground" : "h-2 w-px bg-muted-foreground/70 group-hover:bg-foreground"} />
          </Button>
        );
      })}
    </div>
  );
});
