// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { ABI, ArrayHandle, LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Layerstack, Stage } from "../dist/index.js";
import type { NativeModule } from "../dist/native.js";
import { bytes, mockSource, scene, usdz } from "./fixtures.ts";

const layerstack: Layerstack = await loadLayerstack();

function openScene(): Stage {
  return layerstack.open({ identifier: "scene.usda", bytes: bytes("scene.usda") });
}

/** Runs `f` and returns the `LayerstackError` it throws. */
async function failure(f: () => unknown): Promise<LayerstackError> {
  try {
    await f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

describe("loading the module", () => {
  it("reports its versions and is one instance", async () => {
    assert.equal(layerstack.version.abi, ABI);
    const { version } = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")) as { version: string };
    assert.equal(layerstack.version.package, version);
    assert.equal(layerstack.version.crate, version, "the package and the crate are released together");
    assert.equal(await loadLayerstack(), layerstack);
  });

  it("imports no Node built-in, so a browser bundle resolves nothing extra", () => {
    for (const file of ["index.js", "stage.js", "arrays.js", "edits.js", "errors.js", "schemas.js", "skinning.js"]) {
      const source = readFileSync(new URL(`../dist/${file}`, import.meta.url), "utf8");
      assert.doesNotMatch(source, /\bimport\s*\(|from\s+["']node:/, file);
    }
  });

  it("refuses a module of another ABI", async () => {
    const foreign: NativeModule = {
      Stage: { open: () => assert.fail("never opened") },
      EditBatch: class {} as unknown as NativeModule["EditBatch"],
      nativeVersion: () => ["9.9.9", "other", ABI + 1],
      skinPoints: () => assert.fail("never skinned"),
      skinNormals: () => assert.fail("never skinned"),
      skinTransform: () => assert.fail("never skinned"),
      blackbodyTemperatureRgb: () => assert.fail("never asked"),
    };
    const error = await failure(() => loadLayerstack(foreign));
    assert.equal(error.code, "module.abi");
    assert.equal(error.operation, "loadLayerstack");
  });
});

describe("opening with only the root", () => {
  it("composes what it has and lists the rest as unavailable", () => {
    using stage = openScene();
    const unavailable = stage.unavailable();
    assert.deepEqual(
      unavailable.map((need) => [need.identifier, need.assetPath, need.usedBy, need.prims]),
      [
        ["parts/ball.usda", "./parts/ball.usda", "reference", ["/Ball"]],
        ["sub.usda", "./sub.usda", "sublayer", ["/"]],
      ],
    );
    assert.ok(unavailable.every((need) => need.anchor === stage.root));
    assert.equal(stage.root.identifier, "scene.usda");
    assert.equal(stage.root.format, "usda");
    const ball = stage.prim("/Ball");
    assert.equal(ball?.typeName, "Sphere");
    assert.equal(ball?.specifier, "def");
    const status = stage.status();
    assert.equal(status.complete, false);
    assert.equal(status.unavailable.length, 2);
    assert.deepEqual(status.compositionErrors, []);
  });

  it("reads prims, children and attributes", () => {
    using stage = openScene();
    assert.deepEqual(stage.children("/"), ["/Base", "/Ball", "/Garden"]);
    assert.deepEqual(stage.children("/Garden"), ["/Garden/Moss", "/Garden/Fern"]);
    assert.equal(stage.prim("/Garden/Fern")?.active, false);
    assert.equal(stage.prim("/Base")?.abstract, true);
    assert.equal(stage.prim("/Nothing"), null);
    const ball = stage.prim("/Ball");
    assert.ok(ball?.attributeNames().includes("size"));
    // A schema fallback answers before anything authors the radius.
    const radius = ball?.attribute("radius");
    assert.equal(radius?.custom, false);
    assert.deepEqual(radius?.get(), { kind: "scalar", type: "double", value: 1 });
    assert.equal(radius?.explain()?.source, "fallback");
  });
});

describe("the resolve loop", () => {
  it("fetches in rounds until nothing is unavailable", async () => {
    using stage = openScene();
    const source = mockSource(scene);
    const result = await stage.resolve(source);
    assert.equal(result.rounds, 2, "the referenced layer wants its own reference");
    assert.deepEqual(source.asked, ["parts/ball.usda", "sub.usda", "parts/trim.usda"]);
    assert.equal(result.fetched, 3);
    assert.deepEqual(
      result.loaded.map((layer) => layer.identifier),
      ["parts/ball.usda", "sub.usda", "parts/trim.usda"],
    );
    assert.deepEqual(result.failures, []);
    assert.equal(stage.status().complete, true);
    assert.deepEqual(stage.attribute("/Ball.tag")?.get(), { kind: "scalar", type: "token", value: "trim" });
    assert.deepEqual(stage.attribute("/Ball.radius")?.get(), { kind: "scalar", type: "double", value: 0.5 });
  });

  it("can be driven by hand: unavailable, supply, loadSupplied", () => {
    using stage = openScene();
    for (const need of stage.unavailable()) stage.supply(need.identifier, bytes(need.identifier));
    const first = stage.loadSupplied();
    assert.deepEqual(
      first.loaded.map((layer) => layer.identifier),
      ["parts/ball.usda", "sub.usda"],
    );
    assert.deepEqual(
      stage.unavailable().map((need) => need.identifier),
      ["parts/trim.usda"],
    );
    assert.ok(layerstack.transfer().bytesIn > 0);
  });

  it("keeps a confirmed-absent asset unresolved, with composition codes", async () => {
    using stage = openScene();
    const result = await stage.resolve(mockSource({ ...scene, "sub.usda": null, "parts/ball.usda": null }));
    assert.deepEqual(
      result.failures.map((failure) => [failure.assetPath, failure.code]).sort(),
      [
        ["./parts/ball.usda", "io.not_found"],
        ["./sub.usda", "io.not_found"],
      ],
    );
    const status = stage.status();
    assert.deepEqual(status.unavailable, []);
    assert.deepEqual(
      status.unresolved.map((asset) => [asset.assetPath, asset.usedBy, asset.identifier, asset.reason]).sort(),
      [
        ["./parts/ball.usda", "reference", "parts/ball.usda", "io.not_found"],
        ["./sub.usda", "sublayer", "sub.usda", "io.not_found"],
      ],
    );
    const codes = status.compositionErrors.map((issue) => issue.code).sort();
    assert.deepEqual(codes, ["composition.unresolved_asset", "composition.unresolved_sublayer"]);
    assert.equal(status.compositionErrors.find((issue) => issue.code === "composition.unresolved_asset")?.prim, "/Ball");
    assert.equal(status.complete, false);
  });

  it("fails with resolve.fetch naming the identifier, keeping what arrived", async () => {
    using stage = openScene();
    const files: Record<string, string> = { ...scene };
    delete files["sub.usda"];
    const error = await failure(() => stage.resolve(mockSource(files)));
    assert.equal(error.code, "resolve.fetch");
    assert.equal(error.identifier, "sub.usda");
    assert.ok(error.cause instanceof Error);
    assert.ok(stage.layers().some((layer) => layer.identifier === "parts/ball.usda"));
    assert.deepEqual(stage.unavailable().map((need) => need.identifier).sort(), ["parts/trim.usda", "sub.usda"]);
  });

  it("stops with resolve.aborted", async () => {
    using stage = openScene();
    const controller = new AbortController();
    controller.abort(new Error("the user left"));
    const error = await failure(() => stage.resolve(mockSource(scene), { signal: controller.signal }));
    assert.equal(error.code, "resolve.aborted");
  });
});

describe("a USDZ package", () => {
  it("opens as one buffer and wants only what is outside it", async () => {
    // A search-relative path (`outside.usda`) looks inside the package
    // first, then beside it; an anchored one (`./`, `../`) stays inside.
    const pack = usdz([
      [
        "root.usda",
        `#usda 1.0
(
    subLayers = [@./inner.usda@]
)
def "Model" (
    prepend references = @outside.usda@</Part>
) {
}
`,
      ],
      ["inner.usda", `#usda 1.0\nover "Model" {\n    float height = 3\n}\n`],
    ]);
    using stage = layerstack.open({ identifier: "set/model.usdz", bytes: pack });
    assert.deepEqual(
      stage.unavailable().map((need) => need.identifier),
      ["set/outside.usda"],
    );
    assert.deepEqual(stage.attribute("/Model.height")?.get(), { kind: "scalar", type: "float", value: 3 });
    const packaged = stage.layers().filter((layer) => layer.identifier?.startsWith("set/model.usdz"));
    assert.deepEqual(packaged.map((layer) => [layer.identifier, layer.format]).sort(), [
      ["set/model.usdz", "usdz"],
      ["set/model.usdz[inner.usda]", "usda"],
    ]);
    assert.ok(packaged.every((layer) => layer.readOnly), "a package's layers are read-only");
    assert.equal(stage.root.identifier, "set/model.usdz");
    await stage.resolve(mockSource({ "set/outside.usda": `#usda 1.0\ndef "Part" {\n    int count = 4\n}\n` }));
    assert.deepEqual(stage.attribute("/Model.count")?.get(), { kind: "scalar", type: "int", value: 4 });
  });
});

describe("values and explanations", () => {
  it("explains an inherited value, then the stronger sublayer opinion", async () => {
    using stage = openScene();
    const size = stage.attribute("/Ball.size");
    const inherited = size?.explain();
    assert.equal(inherited?.source, "default");
    assert.deepEqual(inherited?.value, { kind: "scalar", type: "double", value: 2 });
    const [first] = inherited?.opinions ?? [];
    assert.equal(first?.arc, "inherits");
    assert.equal(first?.specPath, "/Base.size");
    assert.equal(first?.layer, stage.root);
    assert.equal(first?.role, "contributed");
    assert.equal(first?.contribution, "value");

    await stage.resolve(mockSource(scene));
    const local = stage.attribute("/Ball.size")?.explain();
    assert.deepEqual(local?.value, { kind: "scalar", type: "double", value: 5 });
    const [strongest, weaker] = local?.opinions ?? [];
    assert.equal(strongest?.layer.identifier, "sub.usda");
    assert.equal(strongest?.arc, "local");
    assert.equal(weaker?.arc, "inherits");
    assert.equal(weaker?.role, "ignored");
    assert.equal(weaker?.cause, "shadowed");
  });

  it("hands numeric arrays over as a handle, copied only on request", async () => {
    using stage = openScene();
    await stage.resolve(mockSource(scene));
    const before = layerstack.transfer().bytesOut;
    using points = stage.attribute("/Ball.points")?.array();
    assert.ok(points instanceof ArrayHandle);
    assert.equal(points.type, "point3f[]");
    assert.equal(points.scalar, "f32");
    assert.equal(points.components, 3);
    assert.equal(points.length, 2);
    assert.equal(points.byteLength, 24);
    assert.equal(layerstack.transfer().bytesOut - before, 0, "reading copies nothing");
    const copy = points.copy();
    assert.ok(copy instanceof Float32Array);
    assert.deepEqual(Array.from(copy), [0, 0, 0, 1, 2, 3]);
    copy[0] = 42;
    assert.equal((points.copy() as Float32Array)[0], 0, "each copy is the module's value");
    assert.equal(layerstack.transfer().bytesOut - before, 48, "each copy counts");
  });

  it("keeps 64-bit integers exact as bigint", async () => {
    using stage = openScene();
    await stage.resolve(mockSource(scene));
    assert.deepEqual(stage.attribute("/Ball.serial")?.get(), {
      kind: "scalar",
      type: "int64",
      value: 9007199254740993n,
    });
    assert.equal(typeof stage.root.id, "bigint");
  });
});

describe("metadata, relationships and variant sets", () => {
  it("read what the layers author", () => {
    using stage = layerstack.open({
      identifier: "meta.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
def "World" (
    kind = "assembly"
    variants = {
        string shade = "light"
    }
    prepend variantSets = "shade"
) {
    custom rel lookAt = [</World/Rock>, </World.size>]
    double size = 2 (
        hidden = true
    )

    def "Rock" {
    }

    variantSet "shade" = {
        "light" {
        }
        "dark" {
        }
    }
}
`),
    });
    const world = stage.prim("/World");
    assert.deepEqual(world?.metadata("kind"), { kind: "scalar", type: "token", value: "assembly" });
    assert.equal(world?.metadata("documentation"), null);
    assert.deepEqual(stage.attribute("/World.size")?.metadata("hidden"), { kind: "scalar", type: "bool", value: true });
    assert.deepEqual(world?.variantSets(), [{ name: "shade", selection: "light", variants: ["dark", "light"] }]);
    const lookAt = stage.relationship("/World.lookAt");
    assert.equal(lookAt?.name, "lookAt");
    assert.equal(lookAt?.custom, true);
    assert.deepEqual(lookAt?.targets(), ["/World/Rock", "/World.size"]);
    assert.equal(stage.relationship("/World.missing"), null);
    assert.deepEqual(world?.relationshipNames(), ["lookAt"]);
    assert.ok(!world?.relationshipNames().includes("size"), "attributes are not relationships");
    assert.deepEqual(world?.relationship("lookAt")?.targets(), ["/World/Rock", "/World.size"]);
    // A schema's relationships are listed before anything authors them.
    stage.apply([{ type: "definePrim", path: "/World/Rock/Cam", typeName: "Camera" }], stage.root);
    assert.ok(stage.prim("/World/Rock/Cam")?.relationshipNames().includes("proxyPrim"));
  });
});

describe("errors", () => {
  it("carry stable codes and what they are about", async () => {
    const rejected = await failure(() =>
      layerstack.open({ identifier: "bad.usda", bytes: new TextEncoder().encode("#usda 1.0\ndef {") }),
    );
    assert.equal(rejected.code, "io.rejected");
    assert.equal(rejected.operation, "open");
    assert.equal(rejected.identifier, "bad.usda");

    using stage = openScene();
    const invalid = await failure(() => stage.prim("Ball"));
    assert.equal(invalid.code, "path.invalid");
    assert.equal(invalid.path, "Ball");
    assert.equal((await failure(() => stage.attribute("/Ball"))).code, "path.invalid");
    const missing = await failure(() => stage.children("/Nothing"));
    assert.equal(missing.code, "prim.not_found");
    assert.equal(missing.path, "/Nothing");
    const argument = await failure(() => stage.supply("x.usda", "text" as unknown as Uint8Array));
    assert.equal(argument.code, "argument.invalid");
  });

  it("refuse a layer of another stage", async () => {
    using first = openScene();
    using second = openScene();
    assert.equal(first.isDirty(first.root), false);
    const error = await failure(() => second.isDirty(first.root));
    assert.equal(error.code, "layer.affinity");
    assert.equal(error.operation, "isDirty");
  });
});

describe("disposal", () => {
  it("frees the native stage, after which every call fails", async () => {
    const stage = openScene();
    const ball = stage.prim("/Ball");
    stage.free();
    assert.equal(stage.disposed, true);
    stage.free();
    assert.equal((await failure(() => stage.prim("/Ball"))).code, "stage.disposed");
    assert.equal((await failure(() => ball?.children())).code, "stage.disposed");
    assert.equal((await failure(() => stage.resolve(mockSource(scene)))).code, "stage.disposed");
  });

  it("works with using", () => {
    let escaped: Stage | undefined;
    {
      using stage = openScene();
      escaped = stage;
      assert.equal(stage.disposed, false);
    }
    assert.equal(escaped.disposed, true);
  });
});

describe("composition limits", () => {
  // A prim that specializes a class specializing its parent: `/Leaf/Leaf/...`
  // without end, through no arc cycle.
  const chain = new TextEncoder().encode(`#usda 1.0
def "Leaf" (prepend specializes = </_LeafClass>) {}
class "_LeafClass" (specializes = </Branch>) {}
class "Branch" {
    def "Leaf" (prepend specializes = </_LeafClass>) {}
}
`);
  const reached = (stage: Stage) =>
    stage
      .status()
      .compositionErrors.filter((issue) => issue.code === "composition.limit_exceeded")
      .map((issue) => issue.prim);

  it("stops an endless namespace at the browser default", () => {
    using stage = layerstack.open({ identifier: "chain.usda", bytes: chain });
    assert.deepEqual(reached(stage), ["/Leaf".repeat(66)]);
  });

  it("takes the limits from the open options", () => {
    using stage = layerstack.open({ identifier: "chain.usda", bytes: chain }, { limits: { composedDepth: 8 } });
    // `/Leaf/Leaf` reads `/Branch/Leaf`, two levels deep, and each prim below it
    // is a level further below it.
    assert.deepEqual(reached(stage), ["/Leaf".repeat(10)]);
    using unlimited = layerstack.open(
      { identifier: "chain.usda", bytes: chain },
      { limits: { primIndexNodes: 5000, composedDepth: null } },
    );
    assert.equal(reached(unlimited).length, 1);
  });

  it("refuses a limit that is not a non-negative integer", async () => {
    const error = await failure(() =>
      layerstack.open({ identifier: "chain.usda", bytes: chain }, { limits: { primIndexNodes: -1 } }),
    );
    assert.equal(error.code, "argument.invalid");
  });
});

describe("typed schemas registered at open", () => {
  const bytes = new TextEncoder().encode(`#usda 1.0
def RockMesh "Rock" {
}
def Boulder "Boulder" {
}
def Mesh "Plain" {
}
`);

  it("are their base to isA, with its properties", () => {
    using stage = layerstack.open(
      { identifier: "rocks.usda", bytes },
      {
        schemas: [
          { name: "RockMesh", base: "Mesh" },
          { name: "Boulder", base: "RockMesh" },
        ],
      },
    );
    for (const path of ["/Rock", "/Boulder", "/Plain"]) {
      const prim = stage.prim(path)!;
      assert.equal(prim.isA("Mesh"), true, path);
      assert.equal(prim.isA("Gprim"), true, path);
      assert.equal(prim.isA("Sphere"), false, path);
    }
    assert.equal(stage.prim("/Rock")!.typeName, "RockMesh");
    assert.equal(stage.prim("/Boulder")!.isA("RockMesh"), true);
    assert.equal(stage.prim("/Rock")!.isA("Boulder"), false);
    assert.ok(stage.prim("/Rock")!.attributeNames().includes("faceVertexCounts"), "Mesh's properties");
    using plain = layerstack.open({ identifier: "rocks.usda", bytes });
    assert.equal(plain.prim("/Rock")!.isA("Mesh"), false, "unregistered, the type is unknown");
  });

  it("refuse a name that is taken or not an identifier, and a base that is not a typed schema", async () => {
    for (const schemas of [
      [{ name: "Mesh", base: "Gprim" }],
      [{ name: "1Rock", base: "Mesh" }],
      [{ name: "Rock", base: "CollectionAPI" }],
      [{ name: "Rock", base: "Missing" }],
      [
        { name: "Rock", base: "Mesh" },
        { name: "Rock", base: "Sphere" },
      ],
      [{ name: "Rock" }],
    ]) {
      const error = await failure(() =>
        layerstack.open({ identifier: "rocks.usda", bytes }, { schemas: schemas as { name: string; base: string }[] }),
      );
      assert.equal(error.code, "argument.invalid", JSON.stringify(schemas));
    }
  });
});
