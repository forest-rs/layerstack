// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The three.js nodes that draw a material's texture lookups
// (`lookups.ts`): each `texel * scale + bias`, per channel, as a
// `UsdUVTexture` lookup is.
//
// three.js's own maps multiply a texel by a factor (`roughness`,
// `aoMapIntensity`, ...) and read fixed channels, packed as glTF packs
// occlusion, roughness and metalness: roughness `g`, metalness `b`,
// ambient occlusion `r`, clearcoat `r`, clearcoat roughness `g`,
// displacement `r` (and its node materials an alpha map's `r`). A
// `UsdPreviewSurface` input names its own channel (stagehand's
// `MaterialValue.channel`: a float connected to a texture's `rgb` reads
// `r`, as Storm does), and adds a bias. So each lookup a material draws is
// given as the material's input node (`colorNode`, `emissiveNode`,
// `specularColorNode`, `metalnessNode`, `roughnessNode`, `aoNode`,
// `opacityNode`, `clearcoatNode`, `clearcoatRoughnessNode`, `normalNode`):
// the map's texel times its scale plus its bias, with the input's channel
// swizzled in. The nodes read the material's map and its scale and bias
// (`<map>Scale`, `<map>Bias`, fields of {@link SurfaceStandardMaterial}
// and {@link SurfacePhysicalMaterial}) by reference (`materialReference`),
// so they are made once per map and channel and shared by every material:
// a material's shader is keyed by its nodes, materials reading the same
// maps and channels share one pipeline, and a scale or bias is a uniform,
// so changing one builds no shader. The nodes are set as fixed nodes
// (`fixedNodes.ts`): a material is made again when its record changes,
// never changed in place.
//
// Coordinates: a lookup samples its map at `<map>Transform · (uv, 1)`,
// the matrix `lookups.ts`'s `coordinateMatrix` makes (the texture's
// `UsdTransform2d`, and a UDIM atlas's tile grid), a uniform by reference
// like the scale and bias, of the uv set its texture reads (`uv`, `uv1`,
// ...), named by the node itself. three.js would take both from the
// texture instead, its `matrix` and `channel`, when it builds the shader:
// but the shader is shared by every material of the shape, and the
// texture a shared node holds while one is built is whichever material it
// last drew, so a material reading another set than the first one built
// would sample that one's. So each uv set has its nodes, and a matrix is
// set per material.
//
// A map's texture is read through a reference three.js does not merge
// with another reading the same texture: it merges texture uniforms by
// their texture when it builds a shader, so the shader every material of
// the shape shares would otherwise read one texture for two maps that held
// the same one (a packed occlusion-roughness-metallic image) in the first
// material built.
//
// Normals: a lookup that is exactly a normal map's decoding
// (`decodesNormal`) is left to three.js's normal map, which decodes `* 2 -
// 1` itself; another is drawn as its scaled and biased value, a
// tangent-space normal (`normalMap` of the value encoded back into [0,
// 1]), as Storm reads `normalize(lookup.xyz)`.
//
// Displacement moves vertices before instancing in three.js
// (`NodeMaterial.setupPosition`, `x * displacementScale +
// displacementBias`), and a material's `positionNode` replaces the
// position after it; a displacement reading another channel than `r` is
// drawn through `positionNode`, so on an instanced draw its offset is in
// instance space, scaled by the instance's matrix.
import { materialReference, nodeObject, normalLocal, normalMap, positionLocal, texture, uv, vec3, vec4 } from "three/tsl";
import { MaterialReferenceNode, Matrix3, MeshPhysicalNodeMaterial, MeshStandardNodeMaterial, Vector4 } from "three/webgpu";
import type { Texture } from "three/webgpu";
import { setFixedNode } from "./fixedNodes.ts";
import type { FixedNodeProperty } from "./fixedNodes.ts";
import { LOOKUP_MAPS } from "./lookups.ts";
import type { LookupChannel, LookupMap, Mat3, SurfaceLookups, TextureLookup } from "./lookups.ts";

/** Anything TSL takes: the node types are too loose to be worth spelling here. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyNode = any;

/** A lookup's scale, bias and coordinate matrix on the material, by map: `mapScale`, `mapBias`, `mapTransform`, ... */
export type LookupFields = Record<`${LookupMap}${"Scale" | "Bias"}`, Vector4> & Record<`${LookupMap}Transform`, Matrix3>;

/** Gives `material` each map's scale (1), bias (0) and coordinate matrix (identity), as fields three.js copies with the material. */
function addLookupFields(material: object): void {
  for (const map of LOOKUP_MAPS) {
    Object.assign(material, { [`${map}Scale`]: new Vector4(1, 1, 1, 1), [`${map}Bias`]: new Vector4(0, 0, 0, 0), [`${map}Transform`]: new Matrix3() });
  }
}

/**
 * `MeshStandardNodeMaterial` with each map's scale, bias and coordinate matrix
 * ({@link LookupFields}): fields of every instance, so a copy (`clone`)
 * has them.
 */
export class SurfaceStandardMaterial extends MeshStandardNodeMaterial {
  constructor() {
    super();
    addLookupFields(this);
  }
}

/** `MeshPhysicalNodeMaterial` with each map's scale, bias and coordinate matrix, as {@link SurfaceStandardMaterial}. */
export class SurfacePhysicalMaterial extends MeshPhysicalNodeMaterial {
  constructor() {
    super();
    addLookupFields(this);
  }
}

/** A surface material, standard or physical. */
export type SurfaceMaterial = SurfaceStandardMaterial | SurfacePhysicalMaterial;

/**
 * A material's map by reference, sampled at `coordinates` rather than at
 * its texture's channel moved by its texture's matrix, and never merged
 * with another map's texture (see the header).
 */
class MapReferenceNode extends MaterialReferenceNode {
  readonly #coordinates: AnyNode;

  constructor(property: string, coordinates: AnyNode) {
    super(property, "texture");
    this.#coordinates = coordinates;
  }

  override setNodeType(uniformType: string): void {
    super.setNodeType(uniformType);
    const node = this.node as unknown as { mergeable: boolean; uvNode: unknown; setUpdateMatrix(value: boolean): void };
    node.mergeable = false;
    node.uvNode = this.#coordinates;
    node.setUpdateMatrix(false);
  }
}

/** A reference to the material's `property`, of `type`. */
function ref(property: string, type: string): AnyNode {
  return materialReference(property, type);
}

/** Each map's lookup and input nodes, made at their first use. */
const NODES = new Map<string, AnyNode>();

function cached(key: string, make: () => AnyNode): AnyNode {
  let node = NODES.get(key);
  if (node === undefined) {
    node = make();
    NODES.set(key, node);
  }
  return node;
}

/**
 * `map`'s lookup of uv set `set` (three.js's `uv`, `uv1`, ...): its texel
 * at `<map>Transform · (uv, 1)`, times its scale plus its bias, a `vec4`.
 */
function lookupNode(map: LookupMap, set: number): AnyNode {
  return cached(`${map}@${set}`, () => {
    const coordinates = ref(`${map}Transform`, "mat3").mul(vec3(uv(set), 1)).xy;
    return (nodeObject(new MapReferenceNode(map, coordinates)) as AnyNode).mul(ref(`${map}Scale`, "vec4")).add(ref(`${map}Bias`, "vec4"));
  });
}

/** The input node `lookup` (of uv set `set`) is drawn as, and the material property it is set as; `null` for one three.js draws itself. */
function inputNode(lookup: TextureLookup, set: number, surface: SurfaceLookups): { property: FixedNodeProperty; node: AnyNode } | null {
  const { map } = lookup;
  const channel = lookup.channel as LookupChannel;
  const value = lookupNode(map, set);
  const key = `${map}@${set}`;
  const one = (property: FixedNodeProperty, make: () => AnyNode = () => value[channel]) => ({ property, node: cached(`${key}.${channel}`, make) });
  switch (map) {
    case "map":
      // Opacity is the lookup's `a` when it reads the diffuse texture's,
      // else a lookup of its own (or a constant) multiplies 1.
      return surface.opacityInMap ? { property: "colorNode", node: value } : { property: "colorNode", node: cached(`${key}.rgb1`, () => vec4(value.rgb, 1)) };
    case "emissiveMap":
      // Times the emissive color and intensity, which highlight a selection.
      return one("emissiveNode", () => value.rgb.mul(ref("emissive", "color")).mul(ref("emissiveIntensity", "float")));
    case "specularColorMap":
      return one("specularColorNode", () => value.rgb);
    case "metalnessMap":
      return one("metalnessNode");
    case "roughnessMap":
      return one("roughnessNode");
    case "aoMap":
      return one("aoNode");
    case "alphaMap":
      // Times the material's opacity, which ghosts a selection's neighbors.
      return one("opacityNode", () => ref("opacity", "float").mul(value[channel]));
    case "clearcoatMap":
      return one("clearcoatNode");
    case "clearcoatRoughnessMap":
      return one("clearcoatRoughnessNode");
    case "normalMap":
      if (surface.normalDecodes) return null;
      return one("normalNode", () => normalMap(value.xyz.mul(0.5).add(0.5)));
    case "displacementMap":
      // Drawn by `drawLookups`.
      return null;
  }
}

/**
 * How a lookup samples on a draw: its texture, the uv set its
 * coordinates are (0 for `uv`, `st`), and the matrix moving them
 * (`coordinateMatrix`).
 */
export interface LookupSampling {
  readonly texture: Texture;
  readonly set: number;
  readonly matrix: Mat3;
}

/**
 * Draws `surface`'s lookups on `material`: each map set to its texture
 * (`samplingOf`), its scale, bias and coordinate matrix set, and its input
 * node set as the header of this file describes. Set before the material
 * is first drawn.
 */
export function drawLookups(material: SurfaceMaterial, surface: SurfaceLookups, samplingOf: (lookup: TextureLookup) => LookupSampling): void {
  const fields = material as unknown as LookupFields & Record<LookupMap, Texture | null>;
  for (const lookup of surface.lookups) {
    const sampling = samplingOf(lookup);
    fields[lookup.map] = sampling.texture;
    fields[`${lookup.map}Scale`].set(...lookup.scale);
    fields[`${lookup.map}Bias`].set(...lookup.bias);
    fields[`${lookup.map}Transform`].set(...sampling.matrix);
    if (lookup.map === "displacementMap") {
      if (lookup.channel === "r") {
        // three.js's own displacement is `r * scale + bias`.
        material.displacementScale = lookup.scale[0];
        material.displacementBias = lookup.bias[0];
      } else {
        // Drawn by the position node instead (see the header), reading the texture directly.
        const offset = (texture(fields.displacementMap!) as AnyNode)[lookup.channel]
          .mul(ref("displacementMapScale", "vec4")[lookup.channel])
          .add(ref("displacementMapBias", "vec4")[lookup.channel]);
        fields.displacementMap = null;
        setFixedNode(material, "positionNode", positionLocal.add(normalLocal.normalize().mul(offset)));
      }
      continue;
    }
    const input = inputNode(lookup, sampling.set, surface);
    if (input !== null) setFixedNode(material, input.property, input.node);
  }
}
