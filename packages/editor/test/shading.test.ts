// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The shading model the editor reads: a material whose network it
// inherits from a class, as MaterialX libraries author them.
import assert from "node:assert/strict";
import { test } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import { connectedOnly, hasAuthoredLayout, layoutNetwork, networkRoot, readNetwork, type ShadingNetwork } from "../src/model/network.ts";
import { readOpenPbr } from "../src/model/openpbr.ts";
import { clampToBounds, groupInputs, sliderRange } from "../src/model/shadingInputs.ts";

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

test("inputs are grouped, labelled and ordered by their metadata", () => {
  const groups = groupInputs(stage, "/Stone", stage.material("/Stone")!.inputs());
  assert.deepEqual(
    groups.map((group) => [group.name, group.inputs.map((input) => [input.label, input.order, input.own])]),
    [
      ["Base", [["color", 0, false], ["Color File", 2, true]]],
      ["Specular", [["Roughness", 1, true]]],
    ],
  );
  const roughness = groups[1]!.inputs[0]!;
  assert.deepEqual(roughness.bounds, { min: 0, max: 1, softMin: null, softMax: 0.8 });
  assert.deepEqual(sliderRange(roughness.bounds), [0, 0.8]);
  assert.equal(clampToBounds(1.5, roughness.bounds), 1);
  assert.equal(sliderRange({ min: null, max: 1, softMin: null, softMax: null }), null);
});

test("an OpenPBR surface reads its image through the interface", () => {
  const surface = readOpenPbr(stage, "/Stone", undefined);
  assert.ok(surface !== null);
  assert.equal(surface.shader, "/Stone/surface");
  const texture = surface.baseColor.texture;
  assert.ok(texture !== null);
  assert.equal(texture.file.assetPath, "textures/stone.png");
  assert.equal(texture.file.layer, "looks/stone.usda");
  assert.equal(texture.colorSpace, "sRGB");
  assert.deepEqual(texture.coordinates.transform?.scale, [2, 3]);
  assert.deepEqual(surface.baseColor.value, [0.8, 0.8, 0.8].map(Math.fround));
  assert.deepEqual(surface.specularRoughness, { value: 0.25, texture: null });
  assert.equal(surface.normal, null);
  assert.equal(readOpenPbr(stage, "/base", undefined), null);
});

/** A chain `a → b → out` with an unconnected port on each shader, placed by hand. */
function chain(positions: ReadonlyMap<string, readonly [number, number]>): ShadingNetwork {
  const port = (path: string, kind: "input" | "output", connections: string[] = []) => ({ path, name: path.split(":").pop()!, kind, typeName: "float", connections });
  return {
    root: "/M",
    nodes: [
      { key: "/M/A", path: "/M/A", name: "A", role: "shader", detail: "ND_a", ports: [port("/M/A.inputs:unused", "input"), port("/M/A.outputs:out", "output")] },
      { key: "/M/B", path: "/M/B", name: "B", role: "shader", detail: "ND_b", ports: [port("/M/B.inputs:in", "input", ["/M/A.outputs:out"]), port("/M/B.inputs:spare", "input"), port("/M/B.outputs:out", "output")] },
      { key: "/M#outputs", path: "/M", name: "M", role: "outputs", detail: "Material", ports: [port("/M.outputs:surface", "output", ["/M/B.outputs:out"])] },
    ],
    edges: [
      { from: "/M/A.outputs:out", to: "/M/B.inputs:in" },
      { from: "/M/B.outputs:out", to: "/M.outputs:surface" },
    ],
    positions,
  };
}

test("unconnected ports fold away, and say how many", () => {
  const { network, hidden } = connectedOnly(chain(new Map()));
  assert.equal(hidden, 2);
  assert.deepEqual(
    network.nodes.map((node) => node.ports.map((port) => port.name)),
    [["out"], ["in", "out"], ["surface"]],
  );
});

test("authored positions order the columns and rows; without them the layout is automatic", () => {
  // Authored right to left: B before A, so the authored layout swaps their columns.
  const authored = chain(new Map([["/M/A", [5, 0]], ["/M/B", [0, 0]]]));
  assert.equal(hasAuthoredLayout(authored), true);
  const column = (layout: ReturnType<typeof layoutNetwork>, key: string) => layout.nodes.find((placed) => placed.node.key === key)!.column;
  const followed = layoutNetwork(authored, true);
  assert.ok(column(followed, "/M/B") < column(followed, "/M/A"), "B left of A, as authored");
  assert.ok(column(followed, "/M#outputs") > column(followed, "/M/A"), "outputs last");
  const automatic = layoutNetwork(authored, false);
  assert.ok(column(automatic, "/M/A") < column(automatic, "/M/B"), "A feeds B, so A is left of it");
  // One shader without a position: the automatic layout.
  const partial = chain(new Map([["/M/A", [5, 0]]]));
  assert.equal(hasAuthoredLayout(partial), false);
});
