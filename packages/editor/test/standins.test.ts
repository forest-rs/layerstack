// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A model's cards are one draw: one geometry without groups, each card's
// vertices naming the face whose texture it shows, and one material whose
// shader every card model shares and three.js does not refresh in full
// every frame.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Material } from "three/webgpu";
import NodeMaterialObserver from "three/src/materials/nodes/manager/NodeMaterialObserver.js";
import type { Card, CardFace, StandinBuffers, TextureFile } from "@forest-rs/layerstack_stagehand";
import { CardMaterial, standinGeometry, standinMaterials } from "../src/viewport/standins.ts";
import { TextureStore } from "../src/viewport/surfaces.ts";

/** Whether three.js's observer would refresh every object drawn with `material` in full on every frame. */
function refreshedInFull(material: Material): boolean {
  const containsNode = (NodeMaterialObserver.prototype as unknown as { containsNode(builder: unknown): boolean }).containsNode;
  return containsNode.call(null, { material, context: {} });
}

function card(face: CardFace, texture: CardFace | null): Card {
  return { face, texture, corners: new Float32Array([0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0]), uvs: new Float32Array([0, 0, 1, 0, 1, 1, 0, 1]) };
}

function file(identifier: string): TextureFile {
  return { assetPath: identifier, layer: null, identifier, udim: false };
}

/** A cards stand-in with textures on `faces` (in `CARD_FACES` order, `null` for none). */
function standin(cards: Card[], textures: (string | null)[]): StandinBuffers {
  return {
    mode: "cards",
    lines: new Float32Array(),
    cards,
    color: [0.2, 0.4, 0.6],
    textures: textures.map((identifier) => (identifier === null ? null : file(identifier))),
    input: { mode: "cards", extent: null, cardGeometry: "box", textures: textures.map((identifier) => identifier !== null), hiddenAxis: null },
  };
}

/** A store whose host has no assets: every image stays loading, then unreadable. */
const store = () => new TextureStore(async () => null, async () => false, () => undefined);

test("a model's cards are one draw, each card naming the face whose texture it shows", () => {
  const buffers = standin([card("XPos", "XPos"), card("XNeg", "XPos"), card("ZPos", "ZPos")], ["x.png", null, "z.png", null, null, null]);
  const geometry = standinGeometry(buffers);
  assert.equal(geometry.groups.length, 0, "no group per card");
  assert.deepEqual(Array.from(geometry.getAttribute("cardFace").array), [0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 2]);
  assert.equal(geometry.index!.count, 18);
  const colored = standinGeometry(standin([card("XPos", null), card("YPos", null)], [null, null, null, null, null, null]));
  assert.deepEqual(Array.from(colored.getAttribute("cardFace").array), [0, 0, 0, 0, 0, 0, 0, 0]);
});

test("a model's cards have one material, its faces in the color while their images load", () => {
  const material = standinMaterials(standin([card("XPos", "XPos")], ["x.png", null, null, null, null, null]), false, store());
  assert.ok(material instanceof CardMaterial);
  const slots = material as unknown as Record<string, { toArray?: () => number[] }>;
  for (let slot = 0; slot < 6; slot++) assert.deepEqual(slots[`cardLook${slot}`]!.toArray!().map((v) => +v.toFixed(3)), [0.2, 0.4, 0.6, 1]);
  assert.equal(material.alphaTest, 0.1, "every card material keeps Hydra's cutout");
});

test("every card material builds one shader, without a full refresh each frame, a copy's too", () => {
  const a = standinMaterials(standin([card("XPos", "XPos")], ["a.png", null, null, null, null, null]), false, store()) as CardMaterial;
  const b = standinMaterials(standin([card("YPos", null)], [null, null, null, null, null, null]), false, store()) as CardMaterial;
  assert.equal(refreshedInFull(a), false);
  assert.ok(a.colorNode !== null && a.roughnessNode !== null);
  assert.equal(a.customProgramCacheKey(), b.customProgramCacheKey());
  const copy = a.clone();
  assert.ok(copy instanceof CardMaterial);
  assert.equal(refreshedInFull(copy), false);
  assert.equal(copy.colorNode, a.colorNode);
  assert.equal((copy as unknown as Record<string, unknown>).cardMap0, (a as unknown as Record<string, unknown>).cardMap0);
});
