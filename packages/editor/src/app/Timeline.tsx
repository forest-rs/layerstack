// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The stage's time: its range from `startTimeCode` to `endTimeCode`, the
// time everything is read at, and playback at `timeCodesPerSecond`. A
// stage without a range is read at the default time, and says so.
import { HugeiconsIcon } from "@hugeicons/react";
import { NextIcon, PauseIcon, PlayIcon, PreviousIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { useStore } from "./context.ts";
import { formatTime } from "./format.ts";

export function Timeline() {
  const store = useStore();
  const range = store.range;
  if (store.session === null) return null;
  if (range === null) {
    return (
      <div className="flex h-8 shrink-0 items-center border-t px-3 text-[11px] text-muted-foreground">
        The stage authors no time range: values are read at the default time.
      </div>
    );
  }
  const time = store.time ?? range.start;
  return (
    <div className="flex h-9 shrink-0 items-center gap-1 border-t px-2">
      <TooltipTrigger>
        <Button variant="ghost" size="icon-sm" aria-label="Previous frame" onPress={() => store.step(-1)}>
          <HugeiconsIcon icon={PreviousIcon} />
        </Button>
        <Tooltip>Previous frame (←)</Tooltip>
      </TooltipTrigger>
      <TooltipTrigger>
        <Button variant="ghost" size="icon-sm" aria-label={store.playing ? "Pause" : "Play"} onPress={() => (store.playing ? store.pause() : store.play())}>
          <HugeiconsIcon icon={store.playing ? PauseIcon : PlayIcon} />
        </Button>
        <Tooltip>{store.playing ? "Pause" : "Play"} (Space)</Tooltip>
      </TooltipTrigger>
      <TooltipTrigger>
        <Button variant="ghost" size="icon-sm" aria-label="Next frame" onPress={() => store.step(1)}>
          <HugeiconsIcon icon={NextIcon} />
        </Button>
        <Tooltip>Next frame (→)</Tooltip>
      </TooltipTrigger>
      <span className="w-10 text-right font-mono text-[11px] text-muted-foreground tabular-nums">{formatTime(range.start)}</span>
      <Slider
        aria-label="Time"
        minValue={range.start}
        maxValue={range.end}
        step={1}
        value={time}
        onChange={(value) => {
          store.pause();
          store.setTime(Array.isArray(value) ? value[0] : value);
        }}
        className="mx-2 min-w-0 flex-1"
      />
      <span className="w-10 font-mono text-[11px] text-muted-foreground tabular-nums">{formatTime(range.end)}</span>
      <span className="w-24 text-right font-mono text-[11px] tabular-nums" title={`${range.timeCodesPerSecond} time codes per second`}>
        t = {formatTime(store.playing ? Math.round(time * 100) / 100 : time)}
      </span>
    </div>
  );
}
