// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The traces a renderer takes through materials: each mesh's bound
// material (subsets included), its shader network's inputs and their
// sources, and the coordinate systems a shader may name.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { LayerstackError, Material, Shader, loadLayerstack } from "../dist/index.js";
import type { Edit, Layerstack, Stage } from "../dist/index.js";
import { mockSource } from "./fixtures.ts";

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

export function openShaded(source = shaded, editLayer: "root" | "session" = "root"): Stage {
  return layerstack.open({ identifier: "scene.usda", bytes: text.encode(source) }, { editLayer });
}

const decoder = new TextDecoder();

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

describe("an editor authoring materials", () => {
  /** A new clay material with a surface shader, bound to the lamp and to a new subset of the rock, in one undoable apply. */
  const newMaterial: Edit[] = [
    { type: "definePrim", path: "/Looks/Clay", typeName: "Material" },
    { type: "definePrim", path: "/Looks/Clay/Surface", typeName: "Shader" },
    { type: "setShaderInput", path: "/Looks/Clay/Surface.inputs:roughness", value: { type: "float", value: 0.25 } },
    {
      type: "connectToSource",
      path: "/Looks/Clay.outputs:surface",
      source: "/Looks/Clay/Surface.outputs:surface",
      typeName: "token",
    },
    { type: "setSdrMetadata", path: "/Looks/Clay/Surface", key: "role", value: "surface" },
    { type: "setBaseMaterial", path: "/Looks/Clay", base: "/Looks/Stone" },
    { type: "bindMaterial", path: "/World/Lamp", material: "/Looks/Clay", strength: "strongerThanDescendants" },
    { type: "createMaterialSubset", path: "/World/Rock", name: "Bottom", indices: [0] },
    { type: "bindMaterial", path: "/World/Rock/Bottom", material: "/Looks/Clay" },
    { type: "bindCoordSys", path: "/World/Rock", name: "decal", target: "/World/Projector" },
  ];

  it("builds, binds and undoes a material through the session layer", () => {
    using stage = openShaded(shaded, "session");
    const session = stage.editLayer!;
    const before = decoder.decode(stage.exportLayer(session, "usda"));
    const heard: string[][] = [];
    stage.onChange((changes) => heard.push([...changes.resynced]));
    const receipt = stage.apply(newMaterial);
    assert.equal(receipt.target.layer, session);
    assert.ok(heard.length > 0, "listeners hear the edits");

    assert.equal(stage.prim("/World/Lamp")!.boundMaterial()?.material, "/Looks/Clay");
    assert.equal(stage.prim("/World/Lamp")!.boundMaterial()?.strength, "strongerThanDescendants");
    const subsets = stage.prim("/World/Rock")!.materialSubsets();
    assert.deepEqual(
      subsets.subsets.map(({ path, binding }) => [path, binding?.material]),
      [
        ["/World/Rock/Top", "/Looks/Moss"],
        ["/World/Rock/Bottom", "/Looks/Clay"],
      ],
    );
    assert.deepEqual(subsets.problems, [], "the bottom face completes the partition");
    const clay = stage.material("/Looks/Clay")!;
    assert.equal(clay.baseMaterial, "/Looks/Stone");
    assert.equal(clay.surface().shader, "/Looks/Clay/Surface");
    const surface = stage.shader("/Looks/Clay/Surface")!;
    assert.equal(surface.sdrMetadata.get("role"), "surface");
    assert.deepEqual(surface.input("roughness")?.get(), { kind: "scalar", type: "float", value: 0.25 });
    assert.deepEqual(
      stage.prim("/World/Rock")!.coordSysBindings({ inherited: false }).map(({ name }) => name),
      ["decal"],
    );

    const redo = receipt.undo();
    assert.equal(decoder.decode(stage.exportLayer(session, "usda")), before);
    assert.equal(stage.material("/Looks/Clay"), null);
    redo.undo();
    assert.equal(stage.prim("/World/Lamp")!.boundMaterial()?.material, "/Looks/Clay");
  });

  it("connects, disconnects and unbinds", () => {
    using stage = openShaded();
    stage.apply([
      { type: "disconnectSource", path: "/Looks/Stone/Surface.inputs:diffuseColor" },
      { type: "unbindMaterial", path: "/World/Rock/Top" },
      { type: "setSubsetFamilyType", path: "/World/Rock", familyName: "materialBind", familyType: "nonOverlapping" },
      { type: "blockCoordSys", path: "/World/Rock", name: "paint" },
      // Any existing prim binds, whatever its type, as in OpenUSD.
      { type: "bindMaterial", path: "/World/Lamp", material: "/World/Projector" },
    ]);
    const lamp = stage.prim("/World/Lamp")!.boundMaterial();
    assert.equal(lamp?.material, null, "a prim that is not a material resolves none");
    assert.equal(lamp?.target, "/World/Projector");
    assert.deepEqual(stage.port("/Looks/Stone/Surface.inputs:diffuseColor")?.connections, []);
    assert.equal(stage.prim("/World/Rock/Top")!.boundMaterial()?.material, "/Looks/Stone");
    assert.equal(stage.prim("/World/Rock")!.subsetFamilyType("materialBind"), "nonOverlapping");
    assert.deepEqual(stage.relationship("/World/Rock.coordSys:paint:binding")?.targets(), []);
    stage.apply([
      {
        type: "connectToSource",
        path: "/Looks/Stone/Surface.inputs:diffuseColor",
        source: "/Looks/Stone/Texture.outputs:rgb",
        position: "append",
      },
      { type: "setSdrMetadata", path: "/Looks/Stone/Surface.inputs:roughness", key: "page", value: null },
      {
        type: "createGeomSubset",
        path: "/World/Rock",
        name: "Corners",
        elementType: "point",
        indices: new Int32Array([0, 5]),
        familyName: "pins",
        familyType: "nonOverlapping",
      },
    ]);
    assert.deepEqual(stage.port("/Looks/Stone/Surface.inputs:diffuseColor")?.connections, [
      "/Looks/Stone/Texture.outputs:rgb",
    ]);
    assert.equal(stage.port("/Looks/Stone/Surface.inputs:roughness")?.sdrMetadata.size, 0);
    const [corners] = stage.prim("/World/Rock")!.geomSubsets({ familyName: "pins" });
    assert.deepEqual([...(corners?.indices?.copy() as Int32Array)], [0, 5]);
    assert.equal(stage.prim("/World/Rock")!.subsetFamilyType("pins"), "nonOverlapping");
  });

  it("refuses invalid edits with their codes, naming the edit, and changes nothing", () => {
    using stage = openShaded();
    const before = decoder.decode(stage.exportLayer(stage.root, "usda"));
    const refused = (edit: Edit): LayerstackError => {
      const error = failure(() => stage.apply([{ type: "setActive", path: "/World/Projector", active: false }, edit]));
      assert.equal(error.edit, 1, error.message);
      return error;
    };
    const missing = refused({ type: "bindMaterial", path: "/World/Rock", material: "/Looks/Missing" });
    assert.equal(missing.code, "material.not_found");
    assert.equal(missing.path, "/World/Rock");
    assert.match(missing.message, /no prim is at the material path \/Looks\/Missing/);
    const unconnected = refused({
      type: "connectToSource",
      path: "/Looks/Stone/Surface.inputs:metallic",
      source: "/Looks/Stone/Texture.outputs:rgb",
    });
    assert.equal(unconnected.code, "shading.no_port");
    assert.equal(refused({ type: "disconnectSource", path: "/Looks/Stone/Surface.inputs:metallic" }).code, "shading.no_port");
    assert.equal(
      refused({ type: "setShaderInput", path: "/Looks/Stone/Surface.inputs:roughness", value: { type: "double", value: 1 } }).code,
      "shading.type_mismatch",
    );
    assert.equal(refused({ type: "setBaseMaterial", path: "/World/Rock", base: "/Looks/Stone" }).code, "material.not_material");
    assert.equal(refused({ type: "createMaterialSubset", path: "/World/Rock", name: "Far", indices: [7] }).code, "subset.indices");
    assert.equal(refused({ type: "clearCoordSys", path: "/World/Rock", name: "decal" }).code, "coord_sys.no_binding");
    assert.equal(refused({ type: "setSdrMetadata", path: "/Looks/Stone", key: "role", value: "x" }).code, "shading.not_shader");
    assert.equal(
      refused({ type: "bindMaterial", path: "/World/Rock", material: "/Looks/Stone", strength: "strongest" as "weakerThanDescendants" }).code,
      "argument.invalid",
    );
    assert.equal(
      refused({ type: "createGeomSubset", path: "/World/Rock", name: "Big", elementType: "face", indices: [2 ** 40] }).code,
      "argument.invalid",
    );
    assert.equal(decoder.decode(stage.exportLayer(stage.root, "usda")), before);
  });

  it("binds a material inside a referenced asset through the reference's edit target", async () => {
    using stage = layerstack.open({
      identifier: "scene.usda",
      bytes: text.encode(`#usda 1.0
def Xform "World" {
    def "Robot" (
        prepend references = @./robot.usda@</Robot>
    ) {
    }
}
`),
    });
    await stage.resolve(
      mockSource({
        "robot.usda": `#usda 1.0
def Xform "Robot" {
    def Cube "Body" {
    }
    def Material "Paint" {
    }
}
`,
      }),
    );
    const arc = stage.prim("/World/Robot")!.compositionArcs().find((found) => found.arc === "references")!;
    const target = arc.editTarget();
    const receipt = stage.apply(
      [
        { type: "bindMaterial", path: "/World/Robot/Body", material: "/World/Robot/Paint" },
        { type: "setShaderInput", path: "/World/Robot/Paint.inputs:tint", value: { type: "color3f", value: [1, 0, 0] } },
      ],
      target,
    );
    const asset = stage.layers().find((layer) => layer.identifier === "robot.usda")!;
    const written = decoder.decode(stage.exportLayer(asset, "usda"));
    assert.match(written, /rel material:binding = <\/Robot\/Paint>/, "the target is mapped into the asset");
    assert.match(written, /color3f inputs:tint = \(1, 0, 0\)/);
    assert.equal(stage.prim("/World/Robot/Body")!.boundMaterial()?.material, "/World/Robot/Paint");
    receipt.undo();
    assert.equal(stage.prim("/World/Robot/Body")!.boundMaterial(), null);
  });
});

describe("an editor checking a connection source path", () => {
  // `connection_source_info_oracle.py`: OpenUSD 26.11's
  // `UsdShadeConnectionSourceInfo(stage, sourcePath)` for each path text.
  const fixtures = new URL("../../../layerstack_conformance/fixtures/connection_source_info/", import.meta.url);
  const oracle = JSON.parse(readFileSync(new URL("oracle.json", fixtures), "utf8")) as {
    sources: { path: string; valid: boolean; name: string; type: string; type_name: string; source: string }[];
  };
  // OpenUSD reads this relational attribute path as `inputs:y` of
  // `/Mat/Source`, an attribute the path does not name; layerstack reads no
  // property path there.
  const relational = "/Mat/Source.outputs:out[/X].inputs:y";

  it("reads each path as OpenUSD does", () => {
    using stage = layerstack.open({ identifier: "scene.usda", bytes: readFileSync(new URL("scene.usda", fixtures)) });
    assert.ok(oracle.sources.length >= 30);
    for (const expected of oracle.sources) {
      const info = stage.connectionSourceInfo(expected.path);
      if (expected.path === relational) {
        assert.ok(expected.valid);
        assert.deepEqual(info, { prim: null, name: "", kind: null, typeName: null, source: null, valid: false });
        continue;
      }
      assert.deepEqual(
        {
          valid: info.valid,
          name: info.name,
          type: info.kind === "input" ? "Input" : info.kind === "output" ? "Output" : "Invalid",
          type_name: info.typeName ?? "",
          source: info.prim ?? "",
        },
        {
          valid: expected.valid,
          name: expected.name,
          type: expected.type,
          type_name: expected.type_name,
          source: expected.source,
        },
        expected.path,
      );
      assert.equal(
        info.source,
        expected.valid ? `${expected.source}.${expected.type === "Input" ? "inputs" : "outputs"}:${expected.name}` : null,
        expected.path,
      );
    }
  });

  it("connects to the source a valid info names", () => {
    using stage = openShaded();
    const info = stage.connectionSourceInfo("/Looks/Stone/Texture.outputs:rgb");
    assert.deepEqual(info, {
      prim: "/Looks/Stone/Texture",
      name: "rgb",
      kind: "output",
      typeName: "float3",
      source: "/Looks/Stone/Texture.outputs:rgb",
      valid: true,
    });
    assert.ok(Object.isFrozen(info));
    assert.equal(stage.connectionSourceInfo("/Looks/Gone.outputs:rgb").valid, false);
    stage.apply([
      {
        type: "connectToSource",
        path: "/Looks/Stone/Surface.inputs:roughness",
        source: info.source!,
      },
    ]);
    assert.deepEqual(stage.port("/Looks/Stone/Surface.inputs:roughness")?.connections, [
      "/Looks/Stone/Texture.outputs:rgb",
    ]);
  });
});
