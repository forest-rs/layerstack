// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The stage's time: its range from `startTimeCode` to `endTimeCode`, the
// time everything is read at, and playback at `timeCodesPerSecond`, looping
// at the end or stopping there. Under the track are the selected prim's
// keys (`KeyTicks`), jumped between with the key buttons. A stage without
// a range is read at the default time, and says so.
import { useCallback } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { KeyframeLeftIcon, KeyframeRightIcon, NextIcon, PauseIcon, PlayIcon, PreviousIcon, RepeatIcon, RepeatOffIcon } from "@hugeicons/core-free-icons";
import { ToggleButton } from "react-aria-components";
import { Button, buttonVariants } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { nextKey } from "../model/keyTimes.ts";
import { NOT_A_NUMBER, parseNumber } from "../model/refusal.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { CommitField } from "./fields.tsx";
import { formatTime } from "./format.ts";
import { KeyTicks, useKeyTimes } from "./KeyTicks.tsx";
import { countRender } from "./renders.ts";

export function Timeline() {
  countRender("Timeline");
  const store = useEditorStore();
  useRevision(store, "time", "composition", "view");
  const range = store.range;
  const keys = useKeyTimes(store, range);
  // Handed to the memoized ticks: the same function while the store is.
  const seek = useCallback((time: number) => store.seek(time), [store]);
  if (store.session === null) return null;
  if (range === null) {
    return (
      <div className="flex h-8 shrink-0 items-center border-t px-3 text-[11px] text-muted-foreground">
        The stage authors no time range: values are read at the default time.
      </div>
    );
  }
  const time = store.time ?? range.start;
  const hasPrevious = nextKey(keys.all, time, -1) !== null;
  const hasNext = nextKey(keys.all, time, 1) !== null;
  const none = store.selection === null ? "Select a prim to jump between its keys" : "The selection has no keys in the range";
  return (
    <div className="flex h-9 shrink-0 items-center gap-1 border-t px-2">
      <TooltipTrigger>
        <Button variant="ghost" size="icon-sm" aria-label="Previous frame" onPress={() => store.step(-1)}>
          <HugeiconsIcon icon={PreviousIcon} />
        </Button>
        <Tooltip>Previous frame (←)</Tooltip>
      </TooltipTrigger>
      <TooltipTrigger>
        <Button variant="ghost" size="icon-sm" aria-label="Previous key" isDisabled={!hasPrevious} onPress={() => store.jumpToKey(-1)}>
          <HugeiconsIcon icon={KeyframeLeftIcon} />
        </Button>
        <Tooltip>{hasPrevious ? "Previous key of the selection (,)" : keys.all.length === 0 ? none : "No key before this time"}</Tooltip>
      </TooltipTrigger>
      <TooltipTrigger>
        <Button variant="ghost" size="icon-sm" aria-label={store.playing ? "Pause" : "Play"} onPress={() => (store.playing ? store.pause() : store.play())}>
          <HugeiconsIcon icon={store.playing ? PauseIcon : PlayIcon} />
        </Button>
        <Tooltip>{store.playing ? "Pause" : "Play"} (Space)</Tooltip>
      </TooltipTrigger>
      <TooltipTrigger>
        <Button variant="ghost" size="icon-sm" aria-label="Next key" isDisabled={!hasNext} onPress={() => store.jumpToKey(1)}>
          <HugeiconsIcon icon={KeyframeRightIcon} />
        </Button>
        <Tooltip>{hasNext ? "Next key of the selection (.)" : keys.all.length === 0 ? none : "No key after this time"}</Tooltip>
      </TooltipTrigger>
      <TooltipTrigger>
        <Button variant="ghost" size="icon-sm" aria-label="Next frame" onPress={() => store.step(1)}>
          <HugeiconsIcon icon={NextIcon} />
        </Button>
        <Tooltip>Next frame (→)</Tooltip>
      </TooltipTrigger>
      <TooltipTrigger>
        <ToggleButton
          aria-label="Loop"
          isSelected={store.loop}
          onChange={(on) => store.setLoop(on)}
          className={({ isSelected }) => cn(buttonVariants({ variant: isSelected ? "secondary" : "ghost", size: "icon-sm" }))}
        >
          <HugeiconsIcon icon={store.loop ? RepeatIcon : RepeatOffIcon} />
        </ToggleButton>
        <Tooltip>{store.loop ? "Playing loops at the end: stop there instead" : "Playing stops at the end: loop instead"}</Tooltip>
      </TooltipTrigger>
      <span className="w-10 text-right font-mono text-[11px] text-muted-foreground tabular-nums">{formatTime(range.start)}</span>
      <div className="relative mx-2 flex h-full min-w-0 flex-1 items-center">
        <Slider
          aria-label="Time"
          minValue={range.start}
          maxValue={range.end}
          step={1}
          value={time}
          onChange={(value) => store.seek(Array.isArray(value) ? value[0] : value)}
          className="min-w-0 flex-1"
        />
        <KeyTicks keys={keys} range={range} seek={seek} />
      </div>
      <span className="w-10 font-mono text-[11px] text-muted-foreground tabular-nums">{formatTime(range.end)}</span>
      <span className="text-[11px] text-muted-foreground" title={`${range.timeCodesPerSecond} time codes per second`}>
        t =
      </span>
      <CommitField
        label="Time code"
        numeric
        disabled={store.playing}
        value={formatTime(store.playing ? Math.round(time * 100) / 100 : time)}
        className="w-16"
        onCommit={(text) => {
          const typed = parseNumber(text);
          if (typed === null) return NOT_A_NUMBER;
          store.seek(typed);
          const landed = store.time ?? typed;
          return landed === typed ? undefined : `Outside the range: moved to ${formatTime(landed)}`;
        }}
      />
    </div>
  );
}
