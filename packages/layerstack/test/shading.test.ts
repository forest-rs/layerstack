// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The traces a renderer takes through materials: each mesh's bound
// material (subsets included), its shader network's inputs and their
// sources, and the coordinate systems a shader may name.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError, Material, Shader, loadLayerstack } from "../dist/index.js";
import type { Layerstack, Stage } from "../dist/index.js";

const layerstack: Layerstack = await loadLayerstack();
const text = new TextEncoder();

/** A rock bound to stone through its parent, a moss face subset, a lamp bound to nothing that exists, and a projector space. */
export const shaded = `#usda 1.0
def Xform "World" (
    prepend apiSchemas = ["MaterialBindingAPI", "CoordSysAPI:paint"]
) {
    rel material:binding = </Looks/Stone>
    rel coordSys:paint:binding = </World/Projector>
    def Xform "Projector" {
    }
    def Mesh "Rock" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    ) {
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 2, 3, 3, 2, 4, 5]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0), (1, 2, 0), (0, 2, 0)]
        uniform token subsetFamily:materialBind:familyType = "partition"
        def GeomSubset "Top" (
            prepend apiSchemas = ["MaterialBindingAPI"]
        ) {
            uniform token elementType = "face"
            uniform token familyName = "materialBind"
            int[] indices = [1]
            rel material:binding = </Looks/Moss>
        }
    }
    def Mesh "Lamp" {
        rel material:binding = </Looks/Missing>
    }
}
def Scope "Looks" {
    def Material "Stone" {
        token outputs:surface.connect = </Looks/Stone/Surface.outputs:surface>
        def Shader "Surface" (
            sdrMetadata = {
                string role = "surface"
            }
        ) {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Stone/Texture.outputs:rgb>
            float inputs:roughness = 0.75 (
                sdrMetadata = {
                    string page = "basic"
                }
            )
            token outputs:surface
        }
        def Shader "Texture" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @stone.png@
            float3 outputs:rgb
        }
    }
    def Material "Moss" (
        specializes = </Looks/Stone>
    ) {
    }
}
`;

export function openShaded(source = shaded): Stage {
  return layerstack.open({ identifier: "scene.usda", bytes: text.encode(source) });
}

/** Runs `f` and returns the `LayerstackError` it throws. */
function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

describe("a renderer resolving materials", () => {
  it("finds each mesh's bound material, inherited, legacy or missing", () => {
    using stage = openShaded();
    const [rock, lamp] = stage.boundMaterials(["/World/Rock", "/World/Lamp"]);
    assert.deepEqual(rock, {
      material: "/Looks/Stone",
      target: "/Looks/Stone",
      relationship: "/World.material:binding",
      kind: "direct",
      collection: null,
      strength: "weakerThanDescendants",
      purpose: "",
      withoutBindingApi: false,
    });
    // A binding to a path where no material is: nothing to shade with, and
    // the binding still says where it came from.
    assert.equal(lamp?.material, null);
    assert.equal(lamp?.target, "/Looks/Missing");
    assert.equal(lamp?.withoutBindingApi, true);
    assert.equal(stage.prim("/World/Rock")?.boundMaterial({ purpose: "preview" })?.purpose, "");
    assert.equal(stage.prim("/Looks")?.boundMaterial(), null);
    const error = failure(() => stage.boundMaterials(["/World/Gone"]));
    assert.equal(error.code, "prim.not_found");
    assert.equal(error.path, "/World/Gone");
  });

  it("draws a mesh by its material subsets, with the family's problems beside them", () => {
    using stage = openShaded();
    const subsets = stage.prim("/World/Rock")!.materialSubsets();
    assert.equal(subsets.familyType, "partition");
    assert.equal(subsets.fallback?.material, "/Looks/Stone");
    assert.equal(subsets.subsets.length, 1);
    const [top] = subsets.subsets;
    assert.equal(top?.path, "/World/Rock/Top");
    assert.equal(top?.binding?.material, "/Looks/Moss");
    assert.ok(top?.indices.view() instanceof Int32Array);
    assert.deepEqual([...(top?.indices.copy() as Int32Array)], [1]);
    top?.indices.release();
    assert.deepEqual(
      subsets.problems.map(({ prim, kind, time }) => ({ prim, kind, time })),
      [{ prim: "/World/Rock", kind: "incompletePartition", time: null }],
    );
    const error = failure(() => stage.prim("/World")!.materialSubsets());
    assert.equal(error.code, "subset.geometry");
    assert.match(error.message, /\/World is not a mesh/);
  });

  it("walks a material's network to its inputs' values and sources", () => {
    using stage = openShaded();
    const material = stage.material("/Looks/Moss");
    assert.ok(material instanceof Material);
    assert.equal(material.baseMaterial, "/Looks/Stone");
    const surface = material.surface();
    assert.equal(surface.shader, "/Looks/Moss/Surface");
    assert.equal(surface.context, "");
    assert.equal(stage.material("/World/Rock"), null);

    const shader = stage.shader("/Looks/Stone/Surface");
    assert.ok(shader instanceof Shader);
    assert.equal(shader.id, "UsdPreviewSurface");
    assert.equal(shader.implementationSource, "id");
    assert.deepEqual([...shader.sdrMetadata], [["role", "surface"]]);
    assert.deepEqual(
      shader.inputs().map((input) => [input.name, input.typeName, input.connections]),
      [
        ["diffuseColor", "color3f", ["/Looks/Stone/Texture.outputs:rgb"]],
        ["roughness", "float", []],
      ],
    );
    const roughness = shader.input("roughness")!;
    assert.deepEqual([...roughness.sdrMetadata], [["page", "basic"]]);
    assert.deepEqual(roughness.get(), { kind: "scalar", type: "float", value: 0.75 });
    // Unconnected: its own value is its source.
    assert.deepEqual(roughness.valueSources().sources, [
      { attribute: roughness.path, kind: "authoredValue", chain: [roughness.path] },
    ]);
    const diffuse = shader.input("diffuseColor")!;
    assert.equal(diffuse.get(), null);
    assert.deepEqual(
      diffuse.valueSources().sources.map(({ attribute, kind }) => [attribute, kind]),
      [["/Looks/Stone/Texture.outputs:rgb", "shaderOutput"]],
    );
    assert.equal(shader.input("glow"), null);
    assert.equal(stage.port("/Looks/Stone/Surface.info:id"), null);
    assert.deepEqual(
      stage.prim("/Looks/Stone/Texture")!.outputs().map((output) => output.path),
      ["/Looks/Stone/Texture.outputs:rgb"],
    );
    assert.equal(failure(() => material.terminal("glow" as "surface")).code, "argument.invalid");
  });

  it("reads geometry subsets and coordinate systems", () => {
    using stage = openShaded();
    const rock = stage.prim("/World/Rock")!;
    const [top] = rock.geomSubsets({ elementType: "face" });
    assert.equal(top?.path, "/World/Rock/Top");
    assert.equal(top?.familyName, "materialBind");
    assert.deepEqual([...(top?.indices?.copy() as Int32Array)], [1]);
    assert.deepEqual(rock.geomSubsets({ familyName: "other" }), []);
    assert.equal(rock.subsetFamilyType("materialBind"), "partition");
    assert.equal(rock.validateSubsetFamily("materialBind", "face", { time: 0 })[0]?.kind, "incompletePartition");
    assert.equal(failure(() => rock.geomSubsets({ elementType: "corner" as "face" })).code, "argument.invalid");
    assert.deepEqual(rock.coordSysBindings(), [
      { name: "paint", relationship: "/World.coordSys:paint:binding", target: "/World/Projector" },
    ]);
    assert.deepEqual(rock.coordSysBindings({ inherited: false }), []);
  });
});
