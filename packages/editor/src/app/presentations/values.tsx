// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The built-in presentations of values: what a value is shown as, inline,
// and the editor that accepts a new one, by the value's presentation type
// (`registry.ts`'s `valueTypes`). A colour is a swatch (linear values
// painted in sRGB, `color.ts`) and a picker; a colour array a strip of its
// first elements, `displayOpacity` as their alpha; a token the schema
// restricts a menu of its tokens; a `bool` a switch; a number or a tuple
// fields that commit on Enter; an asset path its file, live (a click shows
// the image). Each `accept` authors through the callback the host gives,
// one edit where edits go.
import type { Attribute, ValueInput } from "@forest-rs/layerstack";
import { ColorPickerButton, ColorSwatch } from "@/components/ui/color-picker";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { formatNumber, releaseValue } from "../../model/values.ts";
import { hexToLinear, linearToHex, swatchCss, swatches } from "../../presentations/color.ts";
import { type AcceptProps, type PresentProps, registerCommand, registerPresentation } from "../../presentations/registry.ts";
import { allowedTokens, CommitField } from "../fields.tsx";
import { Live } from "./Present.tsx";

/** The most swatches a colour array's strip shows. */
const STRIP = 8;

/** A value's label for assistive technology: its attribute's name, else its type. */
const labelOf = (props: PresentProps<"value">) => props.object.attribute?.name ?? props.object.typeName;

/** Its tuple's numbers, when it is one. */
const tupleOf = (props: PresentProps<"value">): readonly number[] | null => (props.object.shown.value?.kind === "tuple" ? props.object.shown.value.value : null);

function Text({ text }: { text: string }) {
  return (
    <span className="truncate font-mono text-[11px] text-muted-foreground" title={text}>
      {text}
    </span>
  );
}

function ValueText(props: PresentProps<"value">) {
  return <Text text={props.object.shown.text} />;
}

/** Fields of a tuple's numbers (and, for a colour, its picker), each committing the tuple with one number changed. */
function TupleFields({ props, accept, color }: { props: PresentProps<"value">; accept: (value: ValueInput) => void; color: boolean }) {
  const channels = tupleOf(props);
  if (channels === null || channels.length > 4) return <ValueText {...props} />;
  const type = props.object.typeName;
  const label = labelOf(props);
  return (
    <div className="flex w-full items-center gap-1">
      {color && (
        <ColorPickerButton
          label={`${label} color`}
          value={linearToHex(channels)}
          onCommit={(hex) => {
            const rgb = hexToLinear(hex);
            if (rgb !== null) accept({ type, value: [...rgb, ...channels.slice(3)] });
          }}
        />
      )}
      {channels.map((channel, index) => (
        <CommitField
          key={index}
          label={`${label} ${index}`}
          numeric
          value={formatNumber(channel)}
          className="w-0 flex-1"
          onCommit={(text) => {
            const number = Number(text);
            if (text.trim() === "" || !Number.isFinite(number)) return;
            accept({ type, value: channels.map((old, at) => (at === index ? number : old)) });
          }}
        />
      ))}
    </div>
  );
}

function ColorInline(props: PresentProps<"value">) {
  const channels = tupleOf(props);
  if (channels === null) return <ValueText {...props} />;
  return (
    <span className="flex min-w-0 items-center gap-1.5">
      <ColorSwatch color={swatchCss(channels, channels.length > 3 ? channels[3] : 1)} />
      <Text text={props.object.shown.text} />
    </span>
  );
}

/** The `primvars:displayOpacity` beside a `primvars:displayColor`, read for the swatches' alpha; `null` when none. */
function opacities(attribute: Attribute, time: number | undefined): Float32Array | null {
  if (attribute.name !== "primvars:displayColor") return null;
  const prim = attribute.path.slice(0, attribute.path.lastIndexOf("."));
  const array = attribute.stage.attribute(`${prim}.primvars:displayOpacity`)?.array(time) ?? null;
  if (array === null) return null;
  try {
    return Float32Array.from(array.view() as ArrayLike<number>);
  } finally {
    array.release();
  }
}

/** A colour array read for its strip: its first elements' swatches, its length, its interpolation. */
function colorArray(props: PresentProps<"value">): { readonly shown: ReturnType<typeof swatches>; readonly length: number; readonly interpolation: string | null } | null {
  const attribute = props.object.attribute;
  if (attribute === null) return null;
  const array = attribute.array(props.context.time);
  if (array === null) return null;
  try {
    const view = array.view() as ArrayLike<number>;
    const alpha = opacities(attribute, props.context.time);
    const interpolation = attribute.metadata("interpolation");
    const text = interpolation?.kind === "scalar" ? String(interpolation.value) : null;
    releaseValue(interpolation);
    return { shown: swatches(view, array.components, STRIP, alpha), length: array.length, interpolation: text };
  } finally {
    array.release();
  }
}

function ColorArrayInline(props: PresentProps<"value">) {
  const read = colorArray(props);
  if (read === null) return <ValueText {...props} />;
  const { shown, length, interpolation } = read;
  return (
    <span className="flex min-w-0 items-center gap-1" title={props.object.shown.text}>
      <span className="flex shrink-0 overflow-hidden rounded-sm border border-foreground/15">
        {shown.map((swatch, index) => (
          <span key={index} className="block h-4 w-3 bg-[repeating-conic-gradient(#8884_0_25%,transparent_0_50%)] bg-[length:8px_8px]">
            <span className="block size-full" style={{ background: swatchCss(swatch.rgb, swatch.alpha) }} />
          </span>
        ))}
      </span>
      <span className="truncate text-[11px] text-muted-foreground">
        {length === 1 ? (interpolation ?? "one") : `${length}${length > STRIP ? `, first ${STRIP}` : ""}`}
        {length !== 1 && interpolation !== null && ` · ${interpolation}`}
      </span>
    </span>
  );
}

/**
 * A three-channel colour array of one element (a constant `displayColor`)
 * edits that element; a longer one, or a `color4` one (whose alpha the
 * strip shows times `displayOpacity`), is shown.
 */
function ColorArrayAccept(props: AcceptProps<"value">) {
  const read = /^color3/.test(props.object.typeName) ? colorArray(props) : null;
  const first = read?.shown[0];
  if (read === null || read.length !== 1 || first === undefined) return <ColorArrayInline {...props} />;
  return (
    <span className="flex min-w-0 items-center gap-1.5">
      <ColorPickerButton
        label={`${labelOf(props)} color`}
        value={linearToHex(first.rgb)}
        onCommit={(hex) => {
          const rgb = hexToLinear(hex);
          if (rgb !== null) props.accept({ type: props.object.typeName, value: [rgb] });
        }}
      />
      <ColorArrayInline {...props} />
    </span>
  );
}

function AllowedTokenAccept(props: AcceptProps<"value">) {
  const attribute = props.object.attribute;
  const allowed = attribute === null ? null : allowedTokens(attribute);
  const value = props.object.shown.value;
  const current = value?.kind === "scalar" ? String(value.value) : "";
  if (allowed === null || allowed.length === 0) return <TextAccept {...props} />;
  const options = allowed.includes(current) || current === "" ? allowed : [current, ...allowed];
  const type = props.object.typeName;
  return (
    <Select aria-label={labelOf(props)} value={current} onChange={(key) => key !== null && key !== current && props.accept({ type, value: String(key) })} className="w-full">
      <SelectTrigger size="sm" className="font-mono text-[11px]">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((token) => (
          <SelectItem key={token} id={token} textValue={token} className="font-mono text-[11px]">
            {token}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

function BoolAccept(props: AcceptProps<"value">) {
  const value = props.object.shown.value;
  if (value !== null && value.kind !== "scalar") return <ValueText {...props} />;
  return <Switch size="sm" aria-label={labelOf(props)} isSelected={value?.value === true} onChange={(on) => props.accept({ type: props.object.typeName, value: on })} />;
}

function NumberAccept(props: AcceptProps<"value">) {
  const value = props.object.shown.value;
  if (value?.kind !== "scalar" || (typeof value.value !== "number" && typeof value.value !== "bigint")) return <ValueText {...props} />;
  return (
    <CommitField
      label={labelOf(props)}
      numeric
      value={formatNumber(value.value)}
      className="w-full"
      onCommit={(text) => {
        const number = Number(text);
        if (text.trim() !== "" && Number.isFinite(number)) props.accept({ type: props.object.typeName, value: number });
      }}
    />
  );
}

function TextAccept(props: AcceptProps<"value">) {
  const value = props.object.shown.value;
  if (value?.kind !== "scalar" || typeof value.value !== "string") return <ValueText {...props} />;
  return <CommitField label={labelOf(props)} value={value.value} className="w-full" onCommit={(text) => props.accept({ type: props.object.typeName, value: text })} />;
}

/** An asset path's file name, live: a click shows the image (or what the asset view makes of it). */
function AssetInline(props: PresentProps<"value">) {
  const value = props.object.shown.value;
  const path = value?.kind === "scalar" && typeof value.value === "string" ? value.value : "";
  if (path === "") return <Text text="none" />;
  return (
    <Live object={props.object} context={props.context} title={`${path}: click to show it, right-click for more`}>
      {path.slice(path.lastIndexOf("/") + 1)}
    </Live>
  );
}

/** An asset path: its field, and beside it the file, live. */
function AssetAccept(props: AcceptProps<"value">) {
  const value = props.object.shown.value;
  if (value?.kind !== "scalar" || typeof value.value !== "string") return <ValueText {...props} />;
  return (
    <span className="flex w-full min-w-0 items-center gap-1">
      <CommitField label={labelOf(props)} value={value.value} className="min-w-0 flex-1" onCommit={(text) => props.accept({ type: props.object.typeName, value: text })} />
      {value.value !== "" && (
        <Live object={props.object} context={props.context} className="max-w-[40%] shrink-0" title={`${value.value}: click to show it`}>
          show
        </Live>
      )}
    </span>
  );
}

/** Registers the built-in value presentations and the commands on asset values. */
export function registerValuePresentations(): void {
  registerPresentation({ id: "builtin.value", kind: "value", type: "value", inline: ValueText });
  registerPresentation({ id: "builtin.tuple", kind: "value", type: "tuple", accept: (props) => <TupleFields props={props} accept={props.accept} color={false} /> });
  registerPresentation({ id: "builtin.number", kind: "value", type: "number", accept: NumberAccept });
  registerPresentation({ id: "builtin.bool", kind: "value", type: "bool", accept: BoolAccept });
  registerPresentation({ id: "builtin.string", kind: "value", type: "string", accept: TextAccept });
  registerPresentation({ id: "builtin.token", kind: "value", type: "token", accept: TextAccept });
  registerPresentation({ id: "builtin.allowed-token", kind: "value", type: "allowedToken", accept: AllowedTokenAccept });
  registerPresentation({ id: "builtin.asset", kind: "value", type: "asset", inline: AssetInline, accept: AssetAccept });
  registerPresentation({
    id: "builtin.color",
    kind: "value",
    type: "color",
    inline: ColorInline,
    accept: (props) => <TupleFields props={props} accept={props.accept} color />,
  });
  registerPresentation({ id: "builtin.color-array", kind: "value", type: "color[]", inline: ColorArrayInline, accept: ColorArrayAccept });

  const assetPath = (object: { readonly shown: { readonly value: { readonly kind: string; readonly value: unknown } | null } }) =>
    object.shown.value?.kind === "scalar" && typeof object.shown.value.value === "string" ? object.shown.value.value : "";
  registerCommand({
    id: "builtin.asset.show",
    kind: "value",
    type: "asset",
    title: "Show it in the asset view",
    applies: (object) => object.attribute !== null && assetPath(object) !== "",
    run: (object, { store }) => {
      const attribute = object.attribute!;
      store.select(attribute.path.slice(0, attribute.path.lastIndexOf(".")));
      store.explain(attribute.path);
      store.setDrawScope("selection");
    },
  });
  registerCommand({
    id: "builtin.asset.explain",
    kind: "value",
    type: "value",
    title: "Explain its value",
    applies: (object) => object.attribute !== null,
    run: (object, { store }) => store.explain(object.attribute!.path),
  });
  registerCommand({
    id: "builtin.asset.copy",
    kind: "value",
    type: "asset",
    title: "Copy the asset path",
    applies: (object) => assetPath(object) !== "",
    run: (object) => void navigator.clipboard?.writeText(assetPath(object)),
  });
}
