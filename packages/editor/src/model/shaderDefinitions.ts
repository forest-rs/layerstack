// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The shader definitions the editor knows, by `info:id`: the
// `UsdPreviewSurface` family, as the UsdPreviewSurface specification
// (openusd.org, "UsdPreviewSurface Specification", version 2.6) defines
// its nodes: each input's name, type, fallback and allowed tokens, and
// each output.
//
// The stage has no shader registry (`SdrRegistry`): a shader's ports, as
// it reads them, are those its prim authors. This table is what the
// editor knows besides, so the shading view and the inspector can show
// an input the prim does not author yet with its fallback (and an edit
// authors it, `UsdShadeConnectableAPI::CreateInput`), and "Add shader"
// offers these ids. Shaders of other ids (MaterialX `ND_*` nodes) show
// their authored ports only.
import type { Value } from "@forest-rs/layerstack";

/** A value a definition gives: a scalar or a tuple, as the stage reads one. */
export type DefinedValue = Value & { readonly kind: "scalar" | "tuple" };

/** An input of a shader definition. */
export interface DefinedInput {
  readonly name: string;
  /** Its value type name (`color3f`). */
  readonly typeName: string;
  /** What a renderer reads when nothing is authored or connected. */
  readonly fallback: DefinedValue;
  /** The tokens its value may be, for a `token` input that restricts them; `null` otherwise. */
  readonly allowedTokens: readonly string[] | null;
}

/** An output of a shader definition. */
export interface DefinedOutput {
  readonly name: string;
  readonly typeName: string;
}

/** A shader definition: what a `Shader` prim with this `info:id` has. */
export interface ShaderDefinition {
  /** Its `info:id` (`UsdUVTexture`). */
  readonly id: string;
  /** What it is for, in a few words, for a picker. */
  readonly summary: string;
  /** Its family, for grouping a picker. */
  readonly family: "Surface" | "Texture" | "Primvar reader" | "Transform";
  /** The name a new prim of it is given (`UVTexture`), made unique among its siblings. */
  readonly primName: string;
  readonly inputs: readonly DefinedInput[];
  readonly outputs: readonly DefinedOutput[];
}

const scalar = (type: string, value: number | string): DefinedValue => ({ kind: "scalar", type, value });
const tuple = (type: string, ...value: number[]): DefinedValue => ({ kind: "tuple", type, value });
const input = (name: string, fallback: DefinedValue, allowedTokens: readonly string[] | null = null): DefinedInput => ({
  name,
  typeName: fallback.type,
  fallback,
  allowedTokens,
});

/** `UsdPrimvarReader_<suffix>`: reads the primvar `varname`, else `fallback`, as `result` of `typeName`. */
function primvarReader(suffix: string, fallback: DefinedValue): ShaderDefinition {
  return {
    id: `UsdPrimvarReader_${suffix}`,
    summary: `reads a ${fallback.type} primvar`,
    family: "Primvar reader",
    primName: `PrimvarReader_${suffix}`,
    inputs: [input("varname", scalar("string", "")), input("fallback", fallback)],
    outputs: [{ name: "result", typeName: fallback.type }],
  };
}

const WRAP = ["black", "clamp", "repeat", "mirror", "useMetadata"] as const;

/** The definitions, in the order a picker lists them. */
export const SHADER_DEFINITIONS: readonly ShaderDefinition[] = [
  {
    id: "UsdPreviewSurface",
    summary: "a physically based surface",
    family: "Surface",
    primName: "PreviewSurface",
    inputs: [
      input("diffuseColor", tuple("color3f", 0.18, 0.18, 0.18)),
      input("emissiveColor", tuple("color3f", 0, 0, 0)),
      input("useSpecularWorkflow", scalar("int", 0)),
      input("specularColor", tuple("color3f", 0, 0, 0)),
      input("metallic", scalar("float", 0)),
      input("roughness", scalar("float", 0.5)),
      input("clearcoat", scalar("float", 0)),
      input("clearcoatRoughness", scalar("float", 0.01)),
      input("opacity", scalar("float", 1)),
      input("opacityMode", scalar("token", "transparent"), ["transparent", "presence"]),
      input("opacityThreshold", scalar("float", 0)),
      input("ior", scalar("float", 1.5)),
      input("normal", tuple("normal3f", 0, 0, 1)),
      input("displacement", scalar("float", 0)),
      input("occlusion", scalar("float", 1)),
    ],
    outputs: [
      { name: "surface", typeName: "token" },
      { name: "displacement", typeName: "token" },
    ],
  },
  {
    id: "UsdUVTexture",
    summary: "reads a texture at texture coordinates",
    family: "Texture",
    primName: "UVTexture",
    inputs: [
      input("file", scalar("asset", "")),
      input("st", tuple("float2", 0, 0)),
      input("wrapS", scalar("token", "useMetadata"), WRAP),
      input("wrapT", scalar("token", "useMetadata"), WRAP),
      input("fallback", tuple("float4", 0, 0, 0, 1)),
      input("scale", tuple("float4", 1, 1, 1, 1)),
      input("bias", tuple("float4", 0, 0, 0, 0)),
      input("sourceColorSpace", scalar("token", "auto"), ["raw", "sRGB", "auto"]),
    ],
    outputs: [
      { name: "r", typeName: "float" },
      { name: "g", typeName: "float" },
      { name: "b", typeName: "float" },
      { name: "a", typeName: "float" },
      { name: "rgb", typeName: "float3" },
    ],
  },
  primvarReader("float", scalar("float", 0)),
  primvarReader("float2", tuple("float2", 0, 0)),
  primvarReader("float3", tuple("float3", 0, 0, 0)),
  primvarReader("float4", tuple("float4", 0, 0, 0, 0)),
  primvarReader("int", scalar("int", 0)),
  primvarReader("string", scalar("string", "")),
  primvarReader("normal", tuple("normal3f", 0, 0, 0)),
  primvarReader("point", tuple("point3f", 0, 0, 0)),
  primvarReader("vector", tuple("vector3f", 0, 0, 0)),
  primvarReader("matrix", tuple("matrix4d", 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)),
  {
    id: "UsdTransform2d",
    summary: "rotates, scales and moves texture coordinates",
    family: "Transform",
    primName: "Transform2d",
    inputs: [
      input("in", tuple("float2", 0, 0)),
      input("rotation", scalar("float", 0)),
      input("scale", tuple("float2", 1, 1)),
      input("translation", tuple("float2", 0, 0)),
    ],
    outputs: [{ name: "result", typeName: "float2" }],
  },
];

const BY_ID = new Map(SHADER_DEFINITIONS.map((definition) => [definition.id, definition]));

/** The definition of the shader id `id`; `null` for an id the editor does not know (or none). */
export function shaderDefinition(id: string | null): ShaderDefinition | null {
  return id === null ? null : (BY_ID.get(id) ?? null);
}

/** The definition's input `name`, or `null`. */
export function definedInput(definition: ShaderDefinition | null, name: string): DefinedInput | null {
  return definition?.inputs.find((each) => each.name === name) ?? null;
}

/** The definition's output `name`, or `null`. */
export function definedOutput(definition: ShaderDefinition | null, name: string): DefinedOutput | null {
  return definition?.outputs.find((each) => each.name === name) ?? null;
}
