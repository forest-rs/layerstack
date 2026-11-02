// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Rect lights (`UsdLuxRectLight`, three.js's `RectAreaLight`) without a
// per-object cost per light.
//
// three.js shades a rect light with two lookup tables (LTC textures) that
// `RectAreaLightNode` samples through `texture(...)` nodes of its own, two
// per light. A texture node made without UVs transforms the UVs it is
// later sampled at by its texture's matrix, a `mat3` uniform updated per
// object (`TextureNode._getMatrixUniform`, `onObjectUpdate`), and sampling
// (`TextureNode.sample`) clones the node with that transform on. So each
// rect light added two `mat3` uniforms to every lit object's uniform group,
// recomputed (`Texture.updateMatrix`) and compared on each object on each
// frame: a lit production element of 2,302 draws and 18 rect lights spent
// about 6.5 ms of its 12 ms a frame on them. The tables are fixed data
// looked up at computed coordinates, with no transform to apply, so here
// every rect light samples one shared pair of texture nodes with the
// transform off: no per-object uniforms, and one binding pair however many
// rect lights.

import { RectAreaLight, RectAreaLightNode, TextureNode } from "three/webgpu";
import type { Node, NodeBuilder, Texture } from "three/webgpu";
import type { DirectRectAreaLightData } from "three/src/nodes/lighting/LightsNode.js";
import { RectAreaLightTexturesLib } from "three/addons/lights/RectAreaLightTexturesLib.js";

/** A lookup table's texture node: sampled at the coordinates given, untransformed. */
class LookupTableNode extends TextureNode {
  override sample(uvNode: Node): this {
    const sampled = super.sample(uvNode);
    sampled.updateMatrix = false;
    return sampled;
  }
}

/** The shared table pairs, full and half float, made at first use. */
const tables: { full: readonly [LookupTableNode, LookupTableNode] | null; half: readonly [LookupTableNode, LookupTableNode] | null } = { full: null, half: null };

/** The table pair a builder samples: full float where float textures filter, else half float, as three.js picks. */
function lookupTables(builder: NodeBuilder): readonly [LookupTableNode, LookupTableNode] {
  const lib = RectAreaLightTexturesLib.init();
  const table = (texture: Texture) => {
    const node = new LookupTableNode(texture);
    node.updateMatrix = false;
    return node;
  };
  if ((builder as unknown as { isAvailable(name: string): boolean }).isAvailable("float32Filterable")) return (tables.full ??= [table(lib.LTC_FLOAT_1), table(lib.LTC_FLOAT_2)]);
  return (tables.half ??= [table(lib.LTC_HALF_1), table(lib.LTC_HALF_2)]);
}

/** `RectAreaLightNode` with the shared, untransformed lookup tables. */
class SharedTableRectAreaLightNode extends RectAreaLightNode {
  override setupDirectRectArea(builder: NodeBuilder): DirectRectAreaLightData | undefined {
    const setup = super.setupDirectRectArea(builder);
    if (setup === undefined) return undefined;
    const [ltc1, ltc2] = lookupTables(builder);
    return { ...setup, ltc_1: ltc1, ltc_2: ltc2 };
  }
}

/**
 * Makes three.js shade every `RectAreaLight` with the shared lookup tables
 * (`SharedTableRectAreaLightNode`), from the next shader built. Light node
 * classes are three.js's global registry (`Light.registerNode`), so this
 * holds for every renderer, the material previews' too; calling it again
 * changes nothing.
 */
export function useSharedRectAreaTables(): void {
  const lib = RectAreaLightTexturesLib.init();
  RectAreaLightNode.setLTC(lib);
  (RectAreaLight as unknown as { registerNode(node: unknown): void }).registerNode(SharedTableRectAreaLightNode);
}
