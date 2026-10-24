# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records where OpenUSD reads an attribute's value before and after a write.

Usage: authoring_inspection_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/authoring_inspection` by
default, the layers below and `oracle.json`, which
`tests/conformance/authoring_inspection.rs` replays against
`Stage::inspect_property_authoring`.

Each case names an attribute, an edit target (a layer of the root layer
stack, a node of a prim's index and a layer of its stack, or a variant
spec), and a write: a default, a time sample at each recorded time, or a
spline. For each time (`default` and time codes) the oracle records
`UsdAttribute::GetResolveInfo(time)` before the write and after it: the
source (`UsdResolveInfoSource`), whether the value is blocked, the node
(its arc, its site, unless that is a prototype's, and the root layer of its
stack) and the layer of that stack the value is read from, which Python
does not expose and the oracle finds as `_ResolveInfoResolver` does: the
strongest layer of the node's stack with that slot, or for clips the
strongest that authors `clips` at the node's site or an ancestor. `shows`
is whether the attribute's value there is the written one afterwards.

The write is made through Sdf at the edit target's mapped spec path, as
`UsdAttribute::Set` and `SetSpline` make it, so that writes the stage would
refuse through an instance proxy are recorded too. Every write starts from
the layers as written here.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Pcp, Sdf, Ts, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "authoring_inspection"))
WRITTEN = 100.0
TIMES = ["default", -5.0, 0.0, 5.0, 15.0]

LAYERS = {
    "root.usda": """#usda 1.0
(
    relocates = {
        </Model/Child>: </Model/Moved>
    }
    subLayers = [
        @./weak.usda@,
        @./base.usda@
    ]
)

def "Plain"
{
    double animBlocked = AnimationBlock
    double blocked = None
    double blockedSamples.timeSamples = {
        0: None,
    }
    double both = 2
    double both.timeSamples = {
        0: 3,
    }
    double curve.spline = {
        0: 1; post held,
        10: 2; post held,
    }
    double sampled.timeSamples = {
        0: 0,
        10: 10,
    }
    double settled = 1
}

def Sphere "Ball"
{
}

def "Model" (
    references = @./asset.usda@</Asset>
)
{
    double local = 1

    def "Bare"
    {
    }
}

def "Var" (
    variants = {
        string mode = "on"
    }
    prepend variantSets = "mode"
)
{
    variantSet "mode" = {
        "off" {
            double x = 8
        }
        "on" {
            double x = 9
        }
    }
}

class "Base"
{
    double inherited = 4
}

def "Kid" (
    inherits = </Base>
)
{
}

def "Inst" (
    instanceable = true
    references = @./asset.usda@</Asset>
)
{
}

def "Clipped" (
    clips = {
        dictionary default = {
            double2[] active = [(0, 0), (10, 1)]
            asset[] assetPaths = [@./clip0.usda@, @./clip1.usda@]
            asset manifestAssetPath = @./manifest.usda@
            string primPath = "/Clip"
        }
    }
)
{
}

def "AutoClipped" (
    clips = {
        dictionary default = {
            double2[] active = [(0, 0), (10, 1)]
            asset[] assetPaths = [@./clip0.usda@, @./clip1.usda@]
            string primPath = "/Clip"
        }
    }
)
{
}
""",
    "weak.usda": """#usda 1.0

over "Plain"
{
    double animBlocked = 6
    double animBlocked.timeSamples = {
        0: 4,
    }
    double free = 7
    double sampled = 5
}

over "Clipped"
{
    uniform double u = 3
    double x = 1
    double y = 2
}

over "AutoClipped"
{
    double x = 1
    double y = 2
    double z = 3
}
""",
    "base.usda": """#usda 1.0

over "Plain"
{
    double animBlocked = 12
}
""",
    "asset.usda": """#usda 1.0

def "Asset"
{
    double local = 6
    double sampled.timeSamples = {
        0: 1,
    }
    double x = 5

    def "Child"
    {
        double x = 5
    }
}
""",
    "clip0.usda": """#usda 1.0

def "Clip"
{
    double x.timeSamples = {
        0: 10,
        5: 15,
    }
}
""",
    "clip1.usda": """#usda 1.0

def "Clip"
{
    double z.timeSamples = {
        10: 1,
    }
}
""",
    "manifest.usda": """#usda 1.0

over "Clip"
{
    uniform double u
    double x
    double y
}
""",
}

ROOT = {"layer": "root.usda"}
WEAK = {"layer": "weak.usda"}
BASE = {"layer": "base.usda"}


def node(holder, arc, site, layer):
    return {"holder": holder, "arc": arc, "site": site, "layer": layer}


ASSET = node("/Model", "reference", "/Asset", "asset.usda")
# (name, attribute, target, write)
CASES = [
    ("stronger samples mask a default", "/Plain.sampled", WEAK, "default"),
    ("stronger samples mask a sample", "/Plain.sampled", WEAK, "sample"),
    ("own samples mask a default", "/Plain.sampled", ROOT, "default"),
    ("a sample beside samples", "/Plain.sampled", ROOT, "sample"),
    ("a stronger default", "/Plain.settled", WEAK, "default"),
    ("a sample beside a default", "/Plain.settled", ROOT, "sample"),
    ("a stronger block", "/Plain.blocked", WEAK, "default"),
    ("a stronger blocked sample", "/Plain.blockedSamples", WEAK, "default"),
    ("an animation block skips weaker samples", "/Plain.animBlocked", BASE, "default"),
    ("a default beside skipped samples", "/Plain.animBlocked", WEAK, "default"),
    ("an animation block masks a sample", "/Plain.animBlocked", WEAK, "sample"),
    ("a sample beside an animation block", "/Plain.animBlocked", ROOT, "sample"),
    ("stronger samples and default", "/Plain.both", WEAK, "default"),
    ("a stronger spline masks a default", "/Plain.curve", WEAK, "default"),
    ("own spline masks a default", "/Plain.curve", ROOT, "default"),
    ("a sample beside a spline", "/Plain.curve", ROOT, "sample"),
    ("a stronger spline masks a spline", "/Plain.curve", WEAK, "spline"),
    ("a spline over a weaker default", "/Plain.free", ROOT, "spline"),
    ("over a schema fallback", "/Ball.radius", WEAK, "default"),
    ("a reference without a local opinion", "/Model.x", ASSET, "default"),
    ("a reference under a local opinion", "/Model.local", ASSET, "default"),
    ("a local default over referenced samples", "/Model.sampled", ROOT, "default"),
    ("a culled reference site", "/Model/Bare.x", ASSET, "sample"),
    ("the selected variant", "/Var.x", node("/Var", "variant", "/Var{mode=on}", "root.usda"),
     "default"),
    ("an unselected variant", "/Var.x", {"layer": "root.usda", "variant": "/Var{mode=off}"},
     "default"),
    ("a local sample over a variant", "/Var.x", ROOT, "sample"),
    ("the inherited class", "/Kid.inherited", node("/Kid", "inherit", "/Base", "root.usda"),
     "default"),
    ("a local sample over an inherited class", "/Kid.inherited", ROOT, "sample"),
    ("a relocated prim's local opinion", "/Model/Moved.x", ROOT, "default"),
    ("a relocated prim's source", "/Model/Moved.x",
     node("/Model/Moved", "reference", "/Asset/Child", "asset.usda"), "sample"),
    ("an instance proxy's prototype", "/Inst/Child.x",
     node("/Inst", "reference", "/Asset", "asset.usda"), "default"),
    ("an instance proxy's local opinion", "/Inst/Child.x", ROOT, "default"),
    ("clips mask a default", "/Clipped.x", WEAK, "default"),
    ("clips mask a sample with no clip value", "/Clipped.y", WEAK, "sample"),
    ("a uniform manifest attribute", "/Clipped.u", WEAK, "default"),
    ("a sample at the clips' anchor", "/Clipped.x", ROOT, "sample"),
    ("generated manifest clips before activation", "/AutoClipped.z", WEAK, "default"),
    ("an attribute no clip holds", "/AutoClipped.y", WEAK, "default"),
]

ARCS = {
    Pcp.ArcTypeRoot: "root",
    Pcp.ArcTypeInherit: "inherit",
    Pcp.ArcTypeVariant: "variant",
    Pcp.ArcTypeRelocate: "relocate",
    Pcp.ArcTypeReference: "reference",
    Pcp.ArcTypePayload: "payload",
    Pcp.ArcTypeSpecialize: "specialize",
}
SOURCES = {
    Usd.ResolveInfoSourceNone: "none",
    Usd.ResolveInfoSourceFallback: "fallback",
    Usd.ResolveInfoSourceDefault: "default",
    Usd.ResolveInfoSourceTimeSamples: "time_samples",
    Usd.ResolveInfoSourceValueClips: "value_clips",
    Usd.ResolveInfoSourceSpline: "spline",
}


def name_of(layer):
    return os.path.basename(layer.realPath)


def nodes(root):
    yield root
    for child in root.children:
        yield from nodes(child)


def find_node(stage, holder, arc, site):
    index = stage.GetPrimAtPath(holder).GetPrimIndex()
    return next(n for n in nodes(index.rootNode)
                if ARCS[n.arcType] == arc and str(n.path) == site)


def edit_target(stage, target, out):
    layer = Sdf.Layer.Find(os.path.join(out, target["layer"]))
    if "holder" in target:
        found = find_node(stage, target["holder"], target["arc"], target["site"])
        return Usd.EditTarget(layer, found)
    if "variant" in target:
        return Usd.EditTarget.ForLocalDirectVariant(layer, Sdf.Path(target["variant"]))
    return Usd.EditTarget(layer)


def source_layer(info, name, source):
    """The layer `_ResolveInfoResolver` reads `source` from."""
    found = info.GetNode()
    if not found:
        return None
    path = found.path
    spec = path.AppendProperty(name)
    for layer in found.layerStack.layers:
        attr = layer.GetAttributeAtPath(spec)
        if source == "time_samples" and layer.GetNumTimeSamplesForPath(spec) > 0:
            return name_of(layer)
        if source == "spline" and attr and attr.HasSpline():
            return name_of(layer)
        if (source == "default" and attr and attr.HasDefaultValue()
                and not isinstance(attr.default, (Sdf.AnimationBlock, Sdf.ValueBlock))):
            return name_of(layer)
    if source == "value_clips":
        prim = path.GetPrimPath()
        while not prim.isEmpty and prim != Sdf.Path.absoluteRootPath:
            for layer in found.layerStack.layers:
                spec = layer.GetPrimAtPath(prim.StripAllVariantSelections())
                if spec and "clips" in spec.ListInfoKeys():
                    return name_of(layer)
            prim = prim.GetParentPath()
    return None


def resolve(attr, time):
    if not attr:
        # Nothing defines or authors the attribute yet.
        return {"source": "none", "blocked": False, "layer": None, "node": None}, None
    code = Usd.TimeCode.Default() if time == "default" else Usd.TimeCode(time)
    info = attr.GetResolveInfo(code)
    source = SOURCES[info.GetSource()]
    found = info.GetNode()
    record = {
        "source": source,
        "blocked": info.ValueIsBlocked(),
        "layer": source_layer(info, attr.GetName(), source),
        "node": None,
    }
    if found:
        path = str(found.path)
        record["node"] = {
            "arc": ARCS[found.arcType],
            "site": None if path.startswith("/__Prototype") else path,
            "stack": name_of(found.layerStack.layers[0]),
        }
    value = attr.Get(code)
    return record, value


def write_value(target, prop, write, time):
    layer = target.GetLayer()
    spec_path = target.MapToSpecPath(Sdf.Path(prop))
    prim = Sdf.CreatePrimInLayer(layer, spec_path.GetPrimOrPrimVariantSelectionPath())
    attr = prim.attributes.get(spec_path.name)
    if attr is None:
        attr = Sdf.AttributeSpec(prim, spec_path.name, Sdf.ValueTypeNames.Double)
    if write == "default":
        attr.default = WRITTEN
    elif write == "sample":
        layer.SetTimeSample(spec_path, time, WRITTEN)
    else:
        spline = Ts.Spline("double")
        knot = Ts.Knot()
        knot.SetTime(0.0)
        knot.SetValue(WRITTEN)
        spline.SetKnot(knot)
        attr.SetSpline(spline)


def reload_layers(out):
    for name in LAYERS:
        layer = Sdf.Layer.Find(os.path.join(out, name))
        if layer:
            layer.Reload(force=True)


def record_case(out, prop, target_spec, write):
    times = []
    for time in TIMES:
        if write != "default" and time == "default":
            continue
        reload_layers(out)
        stage = Usd.Stage.Open(os.path.join(out, "root.usda"))
        path = Sdf.Path(prop)
        before, _ = resolve(stage.GetPrimAtPath(path.GetPrimPath()).GetAttribute(path.name),
                            time)
        target = edit_target(stage, target_spec, out)
        write_value(target, prop, write, 0.0 if time == "default" else time)
        attr = stage.GetPrimAtPath(path.GetPrimPath()).GetAttribute(path.name)
        after, value = resolve(attr, time)
        times.append({"time": time, "before": before, "after": after,
                      "shows": value == WRITTEN})
    return times


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    reload_layers(out)
    cases = []
    for name, prop, target, kind in CASES:
        cases.append({"name": name, "property": prop, "target": target, "write": kind,
                      "times": record_case(out, prop, target, kind)})
    reload_layers(out)
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd_version": f"{minor}.{patch}", "root": "root.usda", "cases": cases}
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    assert Usd.GetVersion() == (0, 26, 8)
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        for name in list(LAYERS) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
