// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The shading model the editor reads: a material whose network it
// inherits from a class, as MaterialX libraries author them.
import assert from "node:assert/strict";
import { test } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import { layoutNetwork, networkRoot, readNetwork } from "../src/model/network.ts";

const LAYER = `#usda 1.0
class "base"
{
    color3f inputs:color = (0.8, 0.8, 0.8) (
        displayGroup = "Base"
        sdrMetadata = { string uiorder = "0" }
    )
    asset inputs:color_file = @@ (
        displayGroup = "Base"
        displayName = "Color File"
        sdrMetadata = { string uiorder = "2" }
    )
    float inputs:roughness = 0.5 (
        displayGroup = "Specular"
        displayName = "Roughness"
        sdrMetadata = {
            string uimax = "1"
            string uimin = "0"
            string uiorder = "1"
            string uisoftmax = "0.8"
        }
    )
    token outputs:mtlx:surface.connect = </base/surface.outputs:out>

    def Shader "image"
    {
        uniform token info:id = "ND_tiledimage_color3"
        color3f inputs:default.connect = </base.inputs:color>
        asset inputs:file.connect = </base.inputs:color_file>
        float2 inputs:uvtiling = (2, 3)
        color3f outputs:out
    }

    def Shader "surface"
    {
        uniform token info:id = "ND_open_pbr_surface_surfaceshader"
        color3f inputs:base_color.connect = </base/image.outputs:out>
        float inputs:specular_roughness.connect = </base.inputs:roughness>
        token outputs:out
    }
}

def Material "Stone" (
    inherits = </base>
)
{
    float inputs:roughness = 0.25
    asset inputs:color_file = @textures/stone.png@
}
`;

const layerstack = await loadLayerstack();
const stage = layerstack.open({ identifier: "looks/stone.usda", bytes: new TextEncoder().encode(LAYER) });

test("a shader's network is its material's, at the material's paths", () => {
  assert.equal(networkRoot(stage, "/Stone/image"), "/Stone");
  assert.equal(networkRoot(stage, "/Stone"), "/Stone");
  const network = readNetwork(stage, "/Stone");
  assert.deepEqual(
    network.nodes.map((node) => [node.key, node.role, node.detail]),
    [
      ["/Stone#inputs", "inputs", "Material"],
      ["/Stone#outputs", "outputs", "Material"],
      ["/Stone/image", "shader", "ND_tiledimage_color3"],
      ["/Stone/surface", "shader", "ND_open_pbr_surface_surfaceshader"],
    ],
  );
  const edges = network.edges.map((edge) => `${edge.from} -> ${edge.to}`);
  assert.ok(edges.includes("/Stone.inputs:color -> /Stone/image.inputs:default"));
  assert.ok(edges.includes("/Stone/image.outputs:out -> /Stone/surface.inputs:base_color"));
  assert.ok(edges.includes("/Stone/surface.outputs:out -> /Stone.outputs:mtlx:surface"));
});

test("the layout runs left to right, ending in the material's outputs", () => {
  const layout = layoutNetwork(readNetwork(stage, "/Stone"));
  const column = new Map(layout.nodes.map((placed) => [placed.node.key, placed.column]));
  assert.deepEqual(
    ["/Stone#inputs", "/Stone/image", "/Stone/surface", "/Stone#outputs"].map((key) => column.get(key)),
    [0, 1, 2, 3],
  );
  for (const edge of layout.edges) assert.ok(edge.x1 < edge.x2, `${edge.edge.from} runs left to right`);
  for (const placed of layout.nodes) assert.ok(placed.y >= 0 && placed.y + placed.height <= layout.height);
});
