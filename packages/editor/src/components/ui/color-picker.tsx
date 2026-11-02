// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import * as React from "react"
import { cn } from "cn"
import {
  Button as ButtonPrimitive,
  ColorArea as ColorAreaPrimitive,
  ColorField as ColorFieldPrimitive,
  ColorPicker as ColorPickerPrimitive,
  ColorSlider as ColorSliderPrimitive,
  ColorSwatch as ColorSwatchPrimitive,
  ColorThumb as ColorThumbPrimitive,
  Dialog as DialogPrimitive,
  DialogTrigger as DialogTriggerPrimitive,
  Input as InputPrimitive,
  parseColor,
  Popover as PopoverPrimitive,
  SliderTrack as SliderTrackPrimitive,
  type Color,
} from "react-aria-components"

/** A swatch of `color` (any CSS colour), on a checkerboard so alpha shows. */
function ColorSwatch({ color, className }: { color: string; className?: string }) {
  return (
    <span
      data-slot="color-swatch"
      className={cn(
        "inline-block size-4 shrink-0 rounded-sm border border-foreground/15 bg-[repeating-conic-gradient(#8884_0_25%,transparent_0_50%)] bg-[length:8px_8px]",
        className,
      )}
    >
      <span className="block size-full rounded-[inherit]" style={{ background: color }} />
    </span>
  )
}

function ColorThumb({ className }: { className?: string }) {
  return (
    <ColorThumbPrimitive
      data-slot="color-thumb"
      className={cn("top-1/2 size-4 rounded-full border-2 border-white shadow-[0_0_0_1px_rgb(0_0_0/0.4)] data-focus-visible:size-5", className)}
    />
  )
}

/**
 * A swatch that opens a colour picker (saturation and brightness, hue,
 * hex) on press. `value` is an sRGB `#rrggbb`; `onCommit` hears the
 * picked colour once, when the picker closes having changed it, so a
 * drag through many colours is one edit.
 */
function ColorPickerButton({
  value,
  onCommit,
  label,
  className,
  isDisabled,
}: {
  value: string
  onCommit: (hex: string) => void
  label: string
  className?: string
  isDisabled?: boolean
}) {
  const [color, setColor] = React.useState<Color>(() => parseColor(value).toFormat("hsb"))
  const [open, setOpen] = React.useState(false)
  return (
    <DialogTriggerPrimitive
      isOpen={open}
      onOpenChange={(next) => {
        if (next) setColor(parseColor(value).toFormat("hsb"))
        else if (color.toString("hex").toLowerCase() !== value.toLowerCase()) onCommit(color.toString("hex"))
        setOpen(next)
      }}
    >
      <ButtonPrimitive
        aria-label={label}
        isDisabled={isDisabled}
        data-slot="color-picker-trigger"
        className={cn(
          "flex h-6 w-7 shrink-0 items-center justify-center rounded-md border border-input bg-input/20 outline-none data-disabled:opacity-60 data-focus-visible:border-ring data-focus-visible:ring-2 data-focus-visible:ring-ring/30 dark:bg-input/30",
          className,
        )}
      >
        <ColorSwatch color={value} />
      </ButtonPrimitive>
      <PopoverPrimitive
        placement="bottom start"
        offset={4}
        className="z-50 rounded-lg bg-popover p-2 text-popover-foreground shadow-md ring-1 ring-foreground/10 outline-none"
      >
        <DialogPrimitive aria-label={label} className="outline-none">
          <ColorPickerPrimitive value={color} onChange={setColor}>
            <div className="flex w-48 flex-col gap-2">
              <ColorAreaPrimitive colorSpace="hsb" xChannel="saturation" yChannel="brightness" className="h-32 w-full rounded-md">
                <ColorThumb />
              </ColorAreaPrimitive>
              <ColorSliderPrimitive colorSpace="hsb" channel="hue" className="w-full">
                <SliderTrackPrimitive className="h-4 w-full rounded-md">
                  <ColorThumb />
                </SliderTrackPrimitive>
              </ColorSliderPrimitive>
              <div className="flex items-center gap-2">
                <ColorSwatchPrimitive className="size-6 shrink-0 rounded-md border border-foreground/15" />
                <ColorFieldPrimitive aria-label="Hex" className="min-w-0 flex-1">
                  <InputPrimitive className="h-6 w-full rounded-md border border-input bg-input/20 px-1.5 font-mono text-[11px] outline-none focus-visible:border-ring dark:bg-input/30" />
                </ColorFieldPrimitive>
              </div>
            </div>
          </ColorPickerPrimitive>
        </DialogPrimitive>
      </PopoverPrimitive>
    </DialogTriggerPrimitive>
  )
}

export { ColorPickerButton, ColorSwatch }
