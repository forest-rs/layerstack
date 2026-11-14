// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Connections through a node graph nested in a material: a container's
// boundary is followed port by port when looking for cycles, and what
// USD's encapsulation does not take is said.
import assert from "node:assert/strict";
import { test } from "node:test";
import { loadLayerstack, type Stage } from "@forest-rs/layerstack";
import { cycleEdges, findPort, judge, readIssues } from "../src/model/connections.ts";
import { judgeDrop } from "../src/model/graphGesture.ts";
import { readNetwork, type ShadingNetwork } from "../src/model/network.ts";
import { portMenu } from "../src/model/networkEdits.ts";

const layerstack = await loadLayerstack();

/**
 * A material with a node graph in it: `Graph.inputs:a` feeds `Inner`,
 * which `Graph.outputs:x` reads and `Reads` reads from there; `inputs:b`
 * and `outputs:y` read and feed nothing, and `inputs:c` reads `inputs:b`.
 */
const NESTED = `#usda 1.0
def Scope "Looks"
{
    def Material "M"
    {
        float inputs:gain = 1
        token outputs:surface.connect = </Looks/M/Surface.outputs:surface>
        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            float inputs:metallic = 0.5
            token outputs:surface
        }
        def Shader "Tex"
        {
            uniform token info:id = "UsdUVTexture"
            float outputs:r
        }
        def NodeGraph "Graph"
        {
            float inputs:a
            float inputs:b
            float inputs:c.connect = </Looks/M/Graph.inputs:b>
            float outputs:x.connect = </Looks/M/Graph/Inner.outputs:r>
            float outputs:y
            def Shader "Inner"
            {
                uniform token info:id = "UsdUVTexture"
                float inputs:rotation.connect = </Looks/M/Graph.inputs:a>
                float inputs:other
                float outputs:r
            }
        }
        def Shader "Reads"
        {
            uniform token info:id = "UsdTransform2d"
            float inputs:rotation.connect = </Looks/M/Graph.outputs:x>
            float outputs:result
        }
    }
    def Material "Other"
    {
        token outputs:surface.connect = </Looks/Other/Far.outputs:r>
        def Shader "Far"
        {
            uniform token info:id = "UsdUVTexture"
            float inputs:fallback.connect = </Looks/M/Tex.outputs:r>
            float outputs:r
        }
    }
}
`;

const M = "/Looks/M";

function nested(): { stage: Stage; network: ShadingNetwork } {
  const stage = layerstack.open({ identifier: "nested.usda", bytes: new TextEncoder().encode(NESTED) });
  return { stage, network: readNetwork(stage, M) };
}

test("a node graph's ports that read nothing of each other are not one node of a cycle", () => {
  const { stage, network } = nested();
  const at = (path: string) => findPort(network, `${M}${path}`)!;
  // Nothing here is a cycle: one input of the graph reading another is a chain.
  assert.deepEqual([...cycleEdges(network)], []);
  assert.deepEqual([...readIssues(stage, network)], []);
  // What reads the graph's output may feed an input of the graph that output does not read.
  assert.deepEqual(judge(network, at("/Reads.outputs:result"), at("/Graph.inputs:b")), { kind: "match" });
  assert.equal(judgeDrop(network, `${M}/Reads.outputs:result`, { kind: "port", port: `${M}/Graph.inputs:b` })?.kind, "match");
  assert.ok(portMenu(network, `${M}/Graph.inputs:b`)[0]!.entries!.some((entry) => entry.id === `${M}/Reads.outputs:result`), "and Connect to… offers it");
  // Into the input that output does read: a cycle, named port by port through the graph.
  assert.deepEqual(judge(network, at("/Reads.outputs:result"), at("/Graph.inputs:a")), {
    kind: "refused",
    reason: "it would make a cycle: Graph.a → Inner → Graph.x → Reads → Graph.a",
  });
  // Two ports of one boundary: the reverse of a connection there is a cycle, though both are one drawn node.
  assert.deepEqual(judge(network, at("/Graph.inputs:c"), at("/Graph.inputs:b")), { kind: "refused", reason: "it would make a cycle: Graph.b → Graph.c → Graph.b" });
  assert.ok(!portMenu(network, `${M}/Graph.inputs:b`)[0]!.entries!.some((entry) => entry.id === `${M}/Graph.inputs:c`));
});
