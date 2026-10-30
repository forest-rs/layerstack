// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The draw list's materials and connected light inputs, against the
// OpenUSD oracle of `layerstack_conformance` (`stagehand_materials`), with
// the same diffs and work counts as `layerstack_stagehand`'s tests.
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Changes, Edit, Stage } from "@forest-rs/layerstack";
import { anchorAssetPath, loadStagehand, udimTiles } from "../dist/index.js";
import type { DrawDiff, DrawList, MaterialItem, MaterialNoteKind, MaterialValue } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();
const base = new URL("../../../layerstack_conformance/fixtures/stagehand_materials/", import.meta.url);
const oracle = JSON.parse(readFileSync(new URL("oracle.json", base), "utf8"));
const bytes = readFileSync(new URL("scene.usda", base));

type Json = any; // eslint-disable-line @typescript-eslint/no-explicit-any

function open(contexts: readonly string[] = [], time?: number): { stage: Stage; list: DrawList; first: DrawDiff; edit(edits: Edit[]): DrawDiff } {
  const stage = layerstack.open({ identifier: "scene.usda", bytes });
  const heard: Changes[] = [];
  stage.onChange((changes) => heard.push(changes));
  const list = stagehand.drawList(stage, { renderContexts: contexts, ...(time === undefined ? {} : { time }) });
  const first = list.refresh();
  return {
    stage,
    list,
    first,
    edit(edits) {
      stage.apply(edits, stage.root);
      const diffs = heard.splice(0).map((changes) => list.applyChanges(changes));
      assert.equal(diffs.length, 1);
      return diffs[0]!;
    },
  };
}

const f32 = (value: number) => Math.fround(value);
const floats = (value: Json): number[] => (Array.isArray(value) ? value.map(f32) : [f32(value)]);
const strip = (path: string | null) => (path === null ? null : path.replace(/^\.\//, ""));

/** OpenUSD's value for a shader's input: its one producing attribute's value at `key`, else the registry default. */
function valueOf(node: Json, id: string, name: string, key: string): Json {
  const producing = node.inputs[name];
  return producing?.length === 1 && producing[0][key] !== undefined && producing[0][key] !== null ? producing[0][key] : oracle.defaults[id][name];
}

function checkValue(at: string, row: Json, item: MaterialItem, name: string, key: string, ours: MaterialValue<unknown>): void {
  const where = `${at} ${name}`;
  const fallback = floats(oracle.defaults.UsdPreviewSurface[name]);
  const producing: Json[] = row.surface.inputs[name] ?? [];
  const constant = (value: MaterialValue<unknown>) => (value.kind === "constant" ? floats(value.value).map(f32) : null);
  if (producing.length === 1 && producing[0].shader !== undefined) {
    const one = producing[0];
    const node = row.nodes[one.attribute.split(".")[0]];
    if (one.shader === "UsdUVTexture") {
      assert.equal(ours.kind, "texture", where);
      if (ours.kind === "texture") assert.equal(ours.channel, one.output, where);
    } else if (String(one.shader).startsWith("UsdPrimvarReader_")) {
      assert.equal(ours.kind, "primvar", where);
      if (ours.kind === "primvar") {
        assert.equal(ours.name, valueOf(node, one.shader, "varname", key), where);
        assert.deepEqual(floats(ours.fallback).map(f32), floats(valueOf(node, one.shader, "fallback", key)), where);
      }
    } else {
      assert.deepEqual(constant(ours), fallback, where);
      assert.ok(item.notes.some((note) => note.kind === "unsupportedShader" && note.id === one.shader), where);
    }
  } else if (producing.length === 1 && producing[0][key] !== null && producing[0][key] !== undefined) {
    assert.deepEqual(constant(ours), floats(producing[0][key]), where);
  } else {
    assert.deepEqual(constant(ours), fallback, where);
    if (producing.length > 1) assert.ok(item.notes.some((note) => note.kind === "multipleSources"), where);
  }
}

describe("materials", () => {
  it("match OpenUSD", async () => {
    const fixtureDir = fileURLToPath(base);
    for (const [contexts, key, time] of [
      [[], "value", undefined],
      [["mtlx"], "value", undefined],
      [[], "value10", 10],
    ] as const) {
      const { stage, list } = open(contexts, time);
      try {
        const rows = oracle.materials.filter((row: Json) => row.contexts.length === contexts.length);
        assert.equal(rows.length, 9);
        for (const row of rows) {
          const at = `${row.path} ${JSON.stringify(contexts)} ${key}`;
          const item = list.material(row.path);
          assert.ok(item !== null, `${at} is not held`);
          if (row.id !== "UsdPreviewSurface") {
            assert.equal(item.surface, null, at);
            const kind = row.id === null ? "noSurface" : "unsupportedSurface";
            assert.ok(item.notes.some((note) => note.kind === kind && (row.id === null || note.id === row.id)), at);
            continue;
          }
          assert.equal(item.surface, row.shader, at);
          assert.equal(item.context, "", at);
          for (const name of ["diffuseColor", "emissiveColor", "specularColor", "normal", "metallic", "roughness", "clearcoat", "clearcoatRoughness", "opacity", "ior", "displacement", "occlusion"] as const) {
            checkValue(at, row, item, name, key, item[name] as MaterialValue<unknown>);
          }
          assert.equal(item.opacityThreshold, f32(valueOf(row.surface, "UsdPreviewSurface", "opacityThreshold", key)), at);
          assert.equal(item.opacityPresence, valueOf(row.surface, "UsdPreviewSurface", "opacityMode", key) === "presence", at);
          assert.equal(item.useSpecularWorkflow, valueOf(row.surface, "UsdPreviewSurface", "useSpecularWorkflow", key) === 1, at);
          for (const texture of item.textures) {
            const node = row.nodes[texture.shader];
            const where = `${at} ${texture.shader}`;
            const tex = (name: string) => valueOf(node, "UsdUVTexture", name, key);
            if (node.file === undefined) {
              assert.equal(texture.file.assetPath, "", where);
              assert.equal(texture.file.identifier, null, where);
              assert.ok(item.notes.some((note) => note.kind === "missingTexture"), where);
            } else {
              assert.equal(texture.file.assetPath, node.file.authored, where);
              assert.equal(strip(texture.file.identifier), node.file.anchored, where);
              assert.equal(texture.file.udim, node.file.udim, where);
              const tiles = await udimTiles(texture.file, (identifier) => existsSync(`${fixtureDir}/${strip(identifier)}`));
              assert.deepEqual(
                tiles.map((tile) => [tile.tile, strip(tile.identifier)]),
                node.file.tiles,
                where,
              );
            }
            assert.equal(texture.wrapS, tex("wrapS"), where);
            assert.equal(texture.wrapT, tex("wrapT"), where);
            assert.equal(texture.colorSpace, tex("sourceColorSpace"), where);
            for (const name of ["fallback", "scale", "bias"] as const) assert.deepEqual(texture[name].map(f32), floats(tex(name)), where);
            let st: Json[] = node.inputs.st ?? [];
            const transform = texture.coordinates.transform;
            if (transform !== null) {
              const place = st[0].attribute.split(".")[0];
              assert.equal(transform.shader, place, where);
              const t = (name: string) => floats(valueOf(row.nodes[place], "UsdTransform2d", name, key));
              assert.deepEqual([f32(transform.rotation)], t("rotation"), where);
              assert.deepEqual(transform.scale.map(f32), t("scale"), where);
              assert.deepEqual(transform.translation.map(f32), t("translation"), where);
              st = row.nodes[place].inputs.in ?? [];
            }
            if (st.length === 1 && st[0].shader !== undefined) {
              const reader = row.nodes[st[0].attribute.split(".")[0]];
              assert.equal(texture.coordinates.primvar, valueOf(reader, "UsdPrimvarReader_float2", "varname", key), where);
              assert.deepEqual(texture.coordinates.fallback.map(f32), floats(valueOf(reader, "UsdPrimvarReader_float2", "fallback", key)), where);
            } else {
              assert.equal(texture.coordinates.primvar, null, where);
              assert.deepEqual(texture.coordinates.fallback.map(f32), floats(tex("st")), where);
            }
          }
        }
        // A light's connected inputs read what they produce.
        for (const row of oracle.lights) {
          const light = list.light(row.path)!;
          const produced = (name: string) => row.inputs[name][0][key];
          assert.equal(light.emission.intensity, f32(produced("intensity")));
          assert.deepEqual(light.emission.color.map(f32), floats(produced("color")));
          assert.equal(light.emission.exposure, f32(produced("exposure")));
          assert.equal(light.geometry.kind === "sphere" ? light.geometry.radius : null, f32(produced("radius") ?? 0.5));
        }
      } finally {
        list.dispose();
        stage.free();
      }
    }
  });

  it("note what they cannot represent, per input", () => {
    const { stage, list } = open();
    try {
      const noted = (item: MaterialItem, input: string): MaterialNoteKind[] => item.notes.filter((note) => note.input === input).map((note) => note.kind);
      const broken = list.material("/Looks/Broken")!;
      assert.deepEqual(noted(broken, "diffuseColor"), ["unresolvedConnection"]);
      assert.deepEqual(noted(broken, "metallic"), ["cycle"]);
      assert.deepEqual(noted(broken, "roughness"), ["unsupportedShader"]);
      assert.deepEqual(noted(broken, "clearcoat"), ["multipleSources"]);
      assert.deepEqual(noted(broken, "emissiveColor"), ["missingTexture"]);
      assert.deepEqual(noted(list.material("/Looks/Animated")!, "roughness"), ["noValue"]);
      const bare = list.material("/Looks/Bare")!;
      assert.equal(bare.context, null);
      assert.deepEqual(bare.alpha, { kind: "opaque" });
      const plain = list.material("/Looks/Plain")!;
      assert.deepEqual(plain.alpha, { kind: "blend", presence: true });
      assert.deepEqual(plain.notes, []);
      assert.deepEqual(list.material("/Looks/Textured")!.alpha, { kind: "mask", threshold: 0.5 });
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("are read again only when what they read changes", () => {
    const { stage, list, first, edit } = open();
    try {
      assert.equal(first.materialsAdded.length, 9);
      assert.equal(first.work.materialsRead, 9);
      // A surface input: that material alone, no item.
      let diff = edit([{ type: "setValue", path: "/Looks/Plain/Surface.inputs:roughness", value: { type: "float", value: 0.9 } }]);
      assert.deepEqual(diff.materialsChanged, ["/Looks/Plain"]);
      assert.equal(diff.work.materialsRead, 1);
      assert.equal(diff.work.itemsRead, 0);
      assert.deepEqual(list.material("/Looks/Plain")!.roughness, { kind: "constant", value: f32(0.9) });
      // A material interface input a node graph forwards.
      diff = edit([{ type: "setValue", path: "/Looks/Graph.inputs:tint", value: { type: "color3f", value: [1, 0, 0] } }]);
      assert.deepEqual(diff.materialsChanged, ["/Looks/Graph"]);
      assert.equal(diff.work.materialsRead, 1);
      // A texture's file.
      diff = edit([{ type: "setValue", path: "/Looks/Textured/Albedo.inputs:file", value: { type: "asset", value: "./textures/other.png" } }]);
      assert.deepEqual(diff.materialsChanged, ["/Looks/Textured"]);
      assert.equal(diff.work.materialsRead, 1);
      assert.equal(list.material("/Looks/Textured")!.textures[0]!.file.identifier, "./textures/other.png");
      // A property no material reads.
      diff = edit([{ type: "createAttribute", path: "/Looks/Plain/Surface.note", typeName: "string", custom: true }]);
      assert.equal(diff.work.materialsRead, 0);
      // A light's connected input: the light alone.
      diff = edit([{ type: "setValue", path: "/Lights/Rig.inputs:power", value: { type: "float", value: 9 } }]);
      assert.deepEqual(diff.lightsChanged, ["/Lights/Lamp"]);
      assert.equal(diff.work.lightsRead, 1);
      assert.equal(diff.work.materialsRead, 0);
      assert.equal(list.light("/Lights/Lamp")!.emission.intensity, 9);
      // Rebinding to a held material reads none; the unbound one is dropped.
      diff = edit([{ type: "setTargets", path: "/World/Plain.material:binding", targets: ["/Looks/Graph"] }]);
      assert.deepEqual(diff.materialsRemoved, ["/Looks/Plain"]);
      assert.equal(diff.work.materialsRead, 0);
      // A new time reads only the material whose inputs might vary.
      diff = list.setTime(10);
      assert.equal(diff.work.materialsRead, 1);
      assert.deepEqual(diff.materialsChanged, ["/Looks/Animated"]);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("anchor texture paths to their layer", () => {
    assert.equal(anchorAssetPath("./grid.png", "scene.usda"), "./grid.png");
    assert.equal(anchorAssetPath("./grid.png", "assets/rock.usda"), "./assets/grid.png");
    assert.equal(anchorAssetPath("../grid.png", "assets/rock.usda"), "./grid.png");
    assert.equal(anchorAssetPath("./tex/../grid.png", "/abs/rock.usda"), "/abs/grid.png");
    assert.equal(anchorAssetPath("textures/grid.png", "assets/rock.usda"), "textures/grid.png");
    assert.equal(anchorAssetPath("./grid.png", "rock.usdz[geo/rock.usdc]"), "rock.usdz[geo/grid.png]");
    assert.equal(anchorAssetPath("", "scene.usda"), null);
  });
});
