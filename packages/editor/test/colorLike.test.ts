// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Colour-like float tuples: an input that is a colour by its definition
// (UsdUVTexture's RGBA inputs, `sdrMetadata`), an interface input that is
// one by what it feeds (directly, or through one or two node graphs), one
// that feeds nothing colour-like, and the registry choosing the colour
// presentation only when the inference says so, read once per revision.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import { colorLike, colorLikeCue, colorLikeNow } from "../src/model/colorLike.ts";
import { type PresentationContext, PresentationRegistry, valueTypes } from "../src/presentations/registry.ts";

const LAYER = `#usda 1.0
def Scope "Looks"
{
    def Material "Direct"
    {
        float4 inputs:tint = (1, 0.5, 0.25, 1)
        float4 inputs:offset = (0, 0, 0, 0)
        float4 inputs:unused = (1, 1, 1, 1)
        float3 inputs:base = (0.2, 0.3, 0.4)
        float2 inputs:repeat = (2, 2)
        float4 inputs:count = (1, 2, 3, 4)
        token outputs:surface.connect = </Looks/Direct/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Direct.inputs:base>
            token outputs:surface
        }

        def Shader "Texture"
        {
            uniform token info:id = "UsdUVTexture"
            float4 inputs:scale.connect = </Looks/Direct.inputs:tint>
            float4 inputs:bias.connect = </Looks/Direct.inputs:offset>
            float4 inputs:fallback = (0.5, 0.5, 0.5, 1)
            float2 inputs:st.connect = </Looks/Direct/Place.outputs:result>
            float3 outputs:rgb
        }

        def Shader "Place"
        {
            uniform token info:id = "UsdTransform2d"
            float2 inputs:scale.connect = </Looks/Direct.inputs:repeat>
            float2 outputs:result
        }

        def Shader "Custom"
        {
            uniform token info:id = "MyNoise"
            float4 inputs:amounts.connect = </Looks/Direct.inputs:count>
            float3 inputs:gel = (1, 0.9, 0.8) (
                sdrMetadata = {
                    string role = "color"
                }
            )
        }
    }

    def Material "Nested"
    {
        float4 inputs:tint = (1, 1, 1, 1)
        float4 inputs:deep = (0.5, 0.5, 0.5, 1)

        def NodeGraph "Graph"
        {
            float4 inputs:tint.connect = </Looks/Nested.inputs:tint>
            float4 inputs:deep.connect = </Looks/Nested.inputs:deep>

            def Shader "Texture"
            {
                uniform token info:id = "UsdUVTexture"
                float4 inputs:scale.connect = </Looks/Nested/Graph.inputs:tint>
                float3 outputs:rgb
            }

            def NodeGraph "Inner"
            {
                float4 inputs:deep.connect = </Looks/Nested/Graph.inputs:deep>

                def Shader "Texture"
                {
                    uniform token info:id = "UsdUVTexture"
                    float4 inputs:fallback.connect = </Looks/Nested/Graph/Inner.inputs:deep>
                    float3 outputs:rgb
                }
            }
        }
    }
}
`;

const layerstack = await loadLayerstack();
const stage = layerstack.open({ identifier: "looks.usda", bytes: new TextEncoder().encode(LAYER) });

describe("colour-like inputs", () => {
  it("are the RGBA inputs of UsdUVTexture and inputs sdrMetadata marks", () => {
    assert.deepEqual(colorLikeNow(stage, "/Looks/Direct/Texture.inputs:fallback"), { input: "/Looks/Direct/Texture.inputs:fallback", why: "previewSurface", through: [] });
    assert.equal(colorLikeNow(stage, "/Looks/Direct/Custom.inputs:gel")?.why, "sdrMetadata");
    assert.equal(colorLikeNow(stage, "/Looks/Direct/Place.inputs:scale"), null, "UsdTransform2d's scale is not a colour");
    assert.equal(colorLikeNow(stage, "/Looks/Direct/Custom.inputs:amounts"), null);
  });

  it("are interface inputs that feed one", () => {
    const tint = colorLikeNow(stage, "/Looks/Direct.inputs:tint");
    assert.deepEqual(tint, { input: "/Looks/Direct/Texture.inputs:scale", why: "previewSurface", through: ["/Looks/Direct.inputs:tint"] });
    assert.equal(colorLikeCue(tint!), "as colour (feeds Texture.scale)");
    assert.equal(colorLikeNow(stage, "/Looks/Direct.inputs:offset")?.input, "/Looks/Direct/Texture.inputs:bias");
    assert.deepEqual(colorLikeNow(stage, "/Looks/Direct.inputs:base"), { input: "/Looks/Direct/Surface.inputs:diffuseColor", why: "colorRole", through: ["/Looks/Direct.inputs:base"] });
  });

  it("follow connections through one and two node graphs", () => {
    assert.deepEqual(colorLikeNow(stage, "/Looks/Nested.inputs:tint"), {
      input: "/Looks/Nested/Graph/Texture.inputs:scale",
      why: "previewSurface",
      through: ["/Looks/Nested.inputs:tint", "/Looks/Nested/Graph.inputs:tint"],
    });
    assert.deepEqual(colorLikeNow(stage, "/Looks/Nested.inputs:deep")?.through, ["/Looks/Nested.inputs:deep", "/Looks/Nested/Graph.inputs:deep", "/Looks/Nested/Graph/Inner.inputs:deep"]);
    assert.equal(colorLikeNow(stage, "/Looks/Nested/Graph.inputs:tint")?.input, "/Looks/Nested/Graph/Texture.inputs:scale", "a node graph's input, by what it feeds");
  });

  it("are not a float4 that feeds nothing colour-like", () => {
    assert.equal(colorLikeNow(stage, "/Looks/Direct.inputs:unused"), null, "feeds nothing");
    assert.equal(colorLikeNow(stage, "/Looks/Direct.inputs:count"), null, "feeds an input that is not a colour");
    assert.equal(colorLikeNow(stage, "/Looks/Direct.inputs:repeat"), null);
  });

  it("are read once per revision of the network", () => {
    const changeLog: { cause: string; changes: Record<string, readonly unknown[]> }[] = [];
    const session = { stage, changeLog } as unknown as Parameters<typeof colorLike>[0];
    assert.equal(colorLike(session, "/Looks/Direct.inputs:unused"), null);
    stage.apply([{ type: "connectToSource", path: "/Looks/Direct/Texture.inputs:fallback", source: "/Looks/Direct.inputs:unused" }]);
    assert.equal(colorLike(session, "/Looks/Direct.inputs:unused"), null, "kept until a recomposition reaches the material");
    const none = { created: [], removed: [], resynced: [], changedInfoOnly: [], primFields: [], changedProperties: [], resyncedProperties: [], mutedLayers: [], unmutedLayers: [], mutedIdentifiers: [], unmutedIdentifiers: [] };
    const connected = { property: "/Looks/Direct/Texture.inputs:fallback", field: "targets", metadata: null };
    changeLog.push({ cause: "connect", changes: { ...none, changedProperties: [connected] } });
    assert.equal(colorLike(session, "/Looks/Direct.inputs:unused")?.input, "/Looks/Direct/Texture.inputs:fallback");
  });
});

describe("the colorLike presentation type", () => {
  it("comes before a role-less float tuple's own type, and only there", () => {
    assert.deepEqual(valueTypes("float4"), ["colorLike", "float4", "tuple", "value"]);
    assert.deepEqual(valueTypes("half3"), ["colorLike", "half3", "tuple", "value"]);
    assert.deepEqual(valueTypes("color4f"), ["color4f", "color", "tuple", "value"]);
    assert.deepEqual(valueTypes("float2"), ["float2", "tuple", "value"]);
    assert.deepEqual(valueTypes("float4[]"), ["float4[]", "array", "value"]);
  });

  it("is chosen when what the value is for makes it a colour", () => {
    const registry = new PresentationRegistry();
    const View = () => null;
    const context = { store: null, stage, time: undefined } as unknown as PresentationContext;
    registry.register({ id: "fields", kind: "value", type: "tuple", accept: View });
    registry.register({ id: "swatch", kind: "value", type: "colorLike", accept: View, applies: (object) => object.attribute !== null && colorLikeNow(stage, object.attribute.path) !== null });
    const value = (path: string) => ({ kind: "value", attribute: stage.attribute(path), typeName: "float4", shown: { value: null, text: "" }, at: null }) as const;
    assert.equal(registry.find(value("/Looks/Nested.inputs:tint"), "accept", context)?.id, "swatch");
    assert.equal(registry.find(value("/Looks/Direct.inputs:count"), "accept", context)?.id, "fields");
  });
});
