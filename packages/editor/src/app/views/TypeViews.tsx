// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Smaller views of what a prim is, each a panel presentation of its type:
//
// - A light (anything with `LightAPI`): what it emits, `intensity × 2^exposure`
//   (UsdLux's `inputs:intensity` and `inputs:exposure`), its colour and,
//   when enabled, its colour temperature as the blackbody colour it
//   multiplies the colour by (`UsdLuxBlackbodyTemperatureAsRgb`), with the
//   inputs' own editors beside them.
// - A camera: its projection, focal length and apertures in millimetres
//   (USD's lens values are in tenths of a scene unit, millimetres by
//   convention: `UsdGeomCamera`), the fields of view they make, clipping,
//   focus and exposure, and looking through it.
// - A transform's op order, first applied last, as the stage reads it
//   (`UsdGeomXformable::GetOrderedXformOps`).
// - A skeleton's joints as their tree (joint paths nest by `/`), and a
//   skeleton animation's joints and blend shapes.
import { useMemo } from "react";
import type { Attribute, Prim } from "@forest-rs/layerstack";
import { Button } from "@/components/ui/button";
import { ColorSwatch } from "@/components/ui/color-picker";
import { jointTree } from "../../model/joints.ts";
import { formatNumber, releaseValue, shown } from "../../model/values.ts";
import { swatchCss } from "../../presentations/color.ts";
import type { PresentationContext, PresentProps } from "../../presentations/registry.ts";
import { ValueField } from "../fields.tsx";
import { usePrimRevision } from "../presentations/Present.tsx";
import { Section } from "../Section.tsx";
import { useTimeOf } from "../context.ts";
import { countRender } from "../renders.ts";

/** A scalar input's number at `time`, else `fallback`. */
function number(prim: Prim, name: string, time: number | undefined, fallback: number): number {
  const value = prim.attribute(name)?.get(time) ?? null;
  const out = value?.kind === "scalar" && typeof value.value === "number" ? value.value : fallback;
  releaseValue(value);
  return out;
}

/** A tuple input's numbers at `time`, else `fallback`. */
function tuple(prim: Prim, name: string, time: number | undefined, fallback: readonly number[]): readonly number[] {
  const value = prim.attribute(name)?.get(time) ?? null;
  const out = value?.kind === "tuple" ? value.value : fallback;
  releaseValue(value);
  return out;
}

/** A row: a label and what it shows. */
function Row({ label, children, title }: { label: string; children: React.ReactNode; title?: string }) {
  return (
    <>
      <span className="truncate text-muted-foreground" title={title}>
        {label}
      </span>
      <span className="flex min-w-0 items-center gap-1.5">{children}</span>
    </>
  );
}

/** An attribute's editor (its value's `accept` presentation), at the current time when animated. */
function Editor({ context, attribute }: { context: PresentationContext; attribute: Attribute | null }) {
  if (attribute === null) return <span className="text-muted-foreground">none</span>;
  const animated = attribute.mightBeTimeVarying();
  return <ValueField store={context.store} attribute={attribute} shown={shown(attribute.get(context.time))} at={animated ? (context.time ?? null) : null} />;
}

/** The light inputs {@link LightView} reads at the time. */
const LIGHT_READS = { attributes: ["inputs:intensity", "inputs:exposure", "inputs:color", "inputs:enableColorTemperature", "inputs:colorTemperature", "inputs:normalize"] };

/** The lens attributes `UsdGeomCamera` reads at the time: not its transform, which a moving camera animates. */
const CAMERA_READS = {
  attributes: [
    "projection",
    "focalLength",
    "horizontalAperture",
    "verticalAperture",
    "horizontalApertureOffset",
    "verticalApertureOffset",
    "clippingRange",
    "fStop",
    "focusDistance",
    "exposure",
    "exposure:iso",
    "exposure:time",
    "exposure:fStop",
    "exposure:responsivity",
  ],
};

export function LightView({ object, context }: PresentProps<"prim">) {
  countRender("LightView");
  const { prim } = object;
  const time = useTimeOf(context.store, context.stage, prim.path, LIGHT_READS);
  usePrimRevision(context.store, prim.path);
  const intensity = number(prim, "inputs:intensity", time, 1);
  const exposure = number(prim, "inputs:exposure", time, 0);
  const color = tuple(prim, "inputs:color", time, [1, 1, 1]);
  const enabled = prim.attribute("inputs:enableColorTemperature")?.get(time);
  const temperatureOn = enabled?.kind === "scalar" && enabled.value === true;
  const kelvin = number(prim, "inputs:colorTemperature", time, 6500);
  const blackbody = useMemo(() => Array.from(context.stage.layerstack.blackbodyTemperatureRgb(kelvin)), [context.stage, kelvin]);
  const emitted = color.map((channel, at) => channel * (temperatureOn ? blackbody[at]! : 1));
  const normalize = prim.attribute("inputs:normalize")?.get(time);
  return (
    <Section title="Light">
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-x-2 gap-y-1 text-[11px]">
        <Row label="emits" title="intensity × 2^exposure, in the light's colour (× its colour temperature's, when enabled)">
          <ColorSwatch color={swatchCss(emitted)} />
          <span className="font-mono">
            {formatNumber(intensity * 2 ** exposure)} = {formatNumber(intensity)} × 2^{formatNumber(exposure)}
          </span>
        </Row>
        <Row label="intensity">
          <Editor context={context} attribute={prim.attribute("inputs:intensity")} />
        </Row>
        <Row label="exposure">
          <Editor context={context} attribute={prim.attribute("inputs:exposure")} />
        </Row>
        <Row label="color">
          <Editor context={context} attribute={prim.attribute("inputs:color")} />
        </Row>
        <Row label="color temperature" title="The blackbody colour of the temperature: what the colour is multiplied by when enabled">
          <ColorSwatch color={swatchCss(blackbody)} className={temperatureOn ? "" : "opacity-40"} />
          <Editor context={context} attribute={prim.attribute("inputs:colorTemperature")} />
        </Row>
        <Row label="use temperature">
          <Editor context={context} attribute={prim.attribute("inputs:enableColorTemperature")} />
        </Row>
        <Row label="normalize">
          <span className="font-mono">{normalize?.kind === "scalar" && normalize.value === true ? "on: power is independent of size" : "off"}</span>
        </Row>
        <Row label="applied">
          <span className="truncate font-mono text-muted-foreground">{["ShapingAPI", "ShadowAPI"].filter((api) => prim.hasAPI(api)).join(", ") || "no shaping or shadow API"}</span>
        </Row>
      </div>
    </Section>
  );
}

/** Degrees of the angle an aperture spans at a focal length. */
const fov = (aperture: number, focal: number) => (2 * Math.atan(aperture / (2 * focal)) * 180) / Math.PI;

export function CameraView({ object, context }: PresentProps<"prim">) {
  countRender("CameraView");
  const { prim } = object;
  const { store, stage } = context;
  const time = useTimeOf(store, stage, prim.path, CAMERA_READS);
  usePrimRevision(store, prim.path);
  const camera = stage.camera(prim.path, time === undefined ? {} : { time });
  if (camera === null) return null;
  const [horizontal = 0, vertical = 0] = camera.aperture;
  const focal = camera.focalLength;
  const through = store.through === prim.path;
  return (
    <Section
      title="Camera"
      aside={
        <Button variant={through ? "secondary" : "ghost"} size="xs" onPress={() => store.lookThrough(through ? null : prim.path)}>
          {through ? "Looking through it" : "Look through"}
        </Button>
      }
    >
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-x-2 gap-y-1 text-[11px]">
        <Row label="projection">
          <span className="font-mono">{camera.projection}</span>
        </Row>
        <Row label="focal length" title="focalLength, in tenths of a scene unit: millimetres by convention">
          <Editor context={context} attribute={prim.attribute("focalLength")} />
        </Row>
        <Row label="aperture" title="horizontalAperture × verticalAperture, in millimetres by convention">
          <span className="font-mono">
            {formatNumber(horizontal)} × {formatNumber(vertical)} mm
          </span>
        </Row>
        {camera.projection === "perspective" && focal > 0 && (
          <Row label="field of view" title="2·atan(aperture / 2·focal length), horizontal × vertical">
            <span className="font-mono">
              {fov(horizontal, focal).toFixed(1)}° × {fov(vertical, focal).toFixed(1)}°
            </span>
          </Row>
        )}
        <Row label="clipping" title="clippingRange, in scene units">
          <span className="font-mono">
            {formatNumber(camera.clippingRange[0]!)} to {formatNumber(camera.clippingRange[1]!)}
          </span>
        </Row>
        <Row label="focus" title="fStop (0: no depth of field) and focusDistance">
          <span className="font-mono">{camera.fStop > 0 ? `f/${formatNumber(camera.fStop)} at ${formatNumber(camera.focusDistance)}` : "no depth of field"}</span>
        </Row>
        <Row label="exposure" title="UsdGeomCamera::ComputeLinearExposureScale">
          <span className="font-mono">× {formatNumber(camera.linearExposureScale)}</span>
        </Row>
      </div>
    </Section>
  );
}

export function XformOpsView({ object }: PresentProps<"prim">) {
  const xformable = object.prim.xformable();
  if (xformable === null || xformable.ops.length === 0) return null;
  return (
    <Section title="Transform ops" aside={xformable.resetsXformStack ? <span>resets the stack</span> : undefined}>
      <ol className="flex flex-col gap-0.5 font-mono text-[11px]" title="xformOpOrder, as written: the last applies first">
        {xformable.ops.map((op, index) => (
          <li key={`${op.name} ${index}`} className="flex gap-2">
            <span className="w-4 text-right text-muted-foreground">{index + 1}</span>
            <span className="truncate">{op.name}</span>
            <span className="ml-auto text-muted-foreground">{op.type ?? "no op type"}</span>
          </li>
        ))}
      </ol>
    </Section>
  );
}

/** A `token[]` attribute's tokens. */
function tokens(prim: Prim, name: string, time: number | undefined): string[] {
  const array = prim.attribute(name)?.array(time) ?? null;
  if (array === null) return [];
  try {
    return Array.from(array.view() as readonly string[]);
  } finally {
    array.release();
  }
}

/** The most joints listed before the rest are counted. */
const JOINTS_SHOWN = 200;

export function SkeletonView({ object, context }: PresentProps<"prim">) {
  countRender("SkeletonView");
  const { prim } = object;
  const revision = usePrimRevision(context.store, prim.path);
  const time = useTimeOf(context.store, context.stage, prim.path, { attributes: ["joints", "blendShapes"] });
  const joints = useMemo(() => tokens(prim, "joints", time), [prim.path, revision, time]);
  const animation = prim.typeName === "SkelAnimation";
  const shapes = useMemo(() => (animation ? tokens(prim, "blendShapes", time) : []), [prim.path, revision, animation]);
  const tree = useMemo(() => jointTree(joints), [joints]);
  return (
    <Section title={animation ? "Skeleton animation" : "Skeleton"} aside={<span>{joints.length} joints</span>}>
      {animation && shapes.length > 0 && (
        <p className="mb-1 text-[11px] text-muted-foreground">
          {shapes.length} blend shapes: {shapes.slice(0, 8).join(", ")}
          {shapes.length > 8 ? ", …" : ""}
        </p>
      )}
      <ol className="max-h-64 overflow-y-auto font-mono text-[11px]" aria-label="Joints">
        {tree.slice(0, JOINTS_SHOWN).map((joint, index) => (
          <li key={joint.path} className="truncate" style={{ paddingLeft: `${joint.depth * 10}px` }} title={`${index}: ${joint.path}`}>
            <span className="mr-1.5 text-muted-foreground">{index}</span>
            {joint.name}
          </li>
        ))}
        {tree.length > JOINTS_SHOWN && <li className="text-muted-foreground">and {tree.length - JOINTS_SHOWN} more</li>}
      </ol>
    </Section>
  );
}
