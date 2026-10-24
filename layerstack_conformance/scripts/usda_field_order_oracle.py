# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the USDA text OpenUSD writes for layers authored in scrambled
orders, and for a scrambled file read back.

Usage: usda_field_order_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/usda_field_order` by
default, `scrambled.usda`, `scrambled.usdc`, `unregistered.usda` and
`oracle.json`, which `tests/usda_field_order.rs` replays against
layerstack's USDA save.

OpenUSD's text writer does not keep authoring order
(`pxr/usd/sdf/fileIO_Common.h`, `pxr/usd/sdf/usdaFileFormat.cpp`): a
comment comes first in each metadata section, then the other fields sorted
by name, `TfDictionaryLessThan` for prims, variants and properties and
byte order for the layer; properties sorted by name; variants and
dictionary keys in byte order. Child prims, variant sets, list items and
targets keep their order.

Each case of `CASES` is a list of steps, each one call on an empty layer
`scene.usda` (and, for an arc into another layer, an empty `asset.usda`),
made in a deliberately scrambled order through the `Sdf` API: `prim`
(`Sdf.CreatePrimInLayer`, then the specifier and type; a variant path
creates the variant), `attribute` (`Sdf.AttributeSpec`, with its default),
`relationship` (`Sdf.RelationshipSpec`, with explicit targets), `sample`
(`SdfLayer::SetTimeSample`), `connect` (explicit `connectionPaths`),
`metadata` (`SdfSpec::SetInfo` on a prim, property or the layer, `/`),
`active` and `instanceable`, `api_schema` (`UsdPrim::AddAppliedSchema`'s
list edit), `reference`, `payload`, `inherit` and `specialize` (appended
to the prepended items, as `UsdReferences::AddReference` and its siblings
do by default), `variant_set` (`UsdVariantSets::AddVariantSet`: the set
spec, and the name appended to the prepended `variantSetNames`),
`select`, `property_order` and `prim_order`. Values are
`{"type": ..., "value": ...}`, a dictionary's entries keyed by name.
`text` is the layer's `ExportToString` after every step.

`round_trip` holds `ExportToString` of `scrambled.usda`, a file written
out of order by hand, and of `scrambled.usdc`, OpenUSD's crate file of it,
which keeps the file's order: both are sorted alike. `unregistered`
holds the export of `unregistered.usda`, the same unregistered metadata
keys on the layer and on a prim, which shows the two orders apart:
`Beta`, `_u`, `a10`, `a9` in the layer, `_u`, `a9`, `a10`, `Beta` on the
prim.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures. A crate file's bytes differ between
OpenUSD builds, so `--check` takes `round_trip.usdc` from the checked-in
`scrambled.usdc` and compares the fresh crate file with it by contents:
every spec's fields, in the order OpenUSD reads them, and every prim's
children and properties, in order.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "usda_field_order"))


def v(type_name, value):
    return {"type": type_name, "value": value}


def dictionary(**entries):
    return v("dictionary", entries)


CASES = {
    # The authoring that showed the bug: properties and metadata in
    # authored order instead of OpenUSD's.
    "mesh": [
        {"op": "prim", "path": "/M", "specifier": "def", "type": "Mesh"},
        {"op": "attribute", "path": "/M.zeta", "type": "float", "default": v("float", 1.0)},
        {"op": "attribute", "path": "/M.points", "type": "point3f[]",
         "default": v("point3f[]", [[0, 0, 0]])},
        {"op": "relationship", "path": "/M.links", "targets": ["/M"]},
        {"op": "metadata", "path": "/M", "key": "kind", "value": v("token", "component")},
        {"op": "active", "path": "/M", "value": True},
    ],
    # Names that tell `TfDictionaryLessThan` from byte order: case, digits
    # and leading zeros, `_` and namespaces, attributes and relationships
    # mixed.
    "names": [
        {"op": "prim", "path": "/N", "specifier": "def", "type": "Xform"},
        *[
            {"op": "attribute", "path": f"/N.{name}", "type": "int", "custom": True,
             "default": v("int", i)}
            for i, name in enumerate([
                "primvars_x", "primvars:st", "Points2", "a10", "a9", "a09", "B", "b",
                "a_b", "aB", "ab", "primvars:normals", "x01", "x1", "x001", "_hidden",
                "points", "primvars:st:indices", "xformOp:translate", "xformOpOrder",
            ])
        ],
        {"op": "relationship", "path": "/N.material:binding", "targets": ["/N"]},
        {"op": "relationship", "path": "/N.proxyPrim", "custom": True, "targets": []},
        {"op": "relationship", "path": "/N.a5", "targets": ["/N", "/N.a9"]},
    ],
    # Every kind of prim field at once: metadata, arcs, variant fields and
    # the reorder statements, set in reverse of OpenUSD's order.
    "prim_fields": [
        {"op": "prim", "path": "/C", "specifier": "class"},
        {"op": "prim", "path": "/S", "specifier": "def"},
        {"op": "prim", "path": "/P", "specifier": "def", "type": "Xform"},
        {"op": "prim_order", "path": "/P", "names": ["y", "x"]},
        {"op": "property_order", "path": "/P", "names": ["b", "a"]},
        {"op": "variant_set", "path": "/P", "name": "shade"},
        {"op": "prim", "path": "/P{shade=dark}"},
        {"op": "select", "path": "/P", "set": "shade", "variant": "dark"},
        {"op": "specialize", "path": "/P", "target": "/S"},
        {"op": "reference", "path": "/P", "target": "/S"},
        {"op": "reference", "path": "/P", "asset": "asset.usda", "target": "/A",
         "offset": 2.0},
        {"op": "payload", "path": "/P", "target": "/S"},
        {"op": "metadata", "path": "/P", "key": "kind", "value": v("token", "group")},
        {"op": "instanceable", "path": "/P", "value": False},
        {"op": "inherit", "path": "/P", "target": "/C"},
        {"op": "metadata", "path": "/P", "key": "hidden", "value": v("bool", True)},
        {"op": "metadata", "path": "/P", "key": "documentation", "value": v("string", "d")},
        {"op": "metadata", "path": "/P", "key": "displayName", "value": v("string", "P")},
        {"op": "metadata", "path": "/P", "key": "customData",
         "value": dictionary(zz=v("int", 1), Ab=v("string", "x"), aa=v("double", 0.5),
                             _x=v("int", 2), b10=v("int", 3), b9=v("int", 4),
                             nested=dictionary(y=v("int", 1), x=v("int", 2)))},
        {"op": "api_schema", "path": "/P", "name": "MaterialBindingAPI"},
        {"op": "api_schema", "path": "/P", "name": "CollectionAPI:a"},
        {"op": "active", "path": "/P", "value": True},
        {"op": "metadata", "path": "/P", "key": "comment", "value": v("string", "a note")},
        {"op": "attribute", "path": "/P.b", "type": "double", "default": v("double", 1.0)},
        {"op": "attribute", "path": "/P.a", "type": "double", "default": v("double", 2.0)},
        {"op": "prim", "path": "/P/y", "specifier": "def"},
        {"op": "prim", "path": "/P/x", "specifier": "over"},
    ],
    # Property metadata, samples set out of time order, and several
    # targets and connections, which keep their order.
    "property_fields": [
        {"op": "prim", "path": "/Q", "specifier": "def", "type": "Mesh"},
        {"op": "attribute", "path": "/Q.primvars:st", "type": "texCoord2f[]",
         "default": v("texCoord2f[]", [[0, 0], [1, 0]])},
        {"op": "metadata", "path": "/Q.primvars:st", "key": "interpolation",
         "value": v("token", "vertex")},
        {"op": "metadata", "path": "/Q.primvars:st", "key": "elementSize",
         "value": v("int", 1)},
        {"op": "metadata", "path": "/Q.primvars:st", "key": "hidden", "value": v("bool", True)},
        {"op": "metadata", "path": "/Q.primvars:st", "key": "displayName",
         "value": v("string", "UV")},
        {"op": "metadata", "path": "/Q.primvars:st", "key": "documentation",
         "value": v("string", "texture coordinates")},
        {"op": "metadata", "path": "/Q.primvars:st", "key": "customData",
         "value": dictionary(z=v("int", 1), a=v("int", 2))},
        {"op": "metadata", "path": "/Q.primvars:st", "key": "comment",
         "value": v("string", "first")},
        {"op": "attribute", "path": "/Q.size", "type": "float", "uniform": True,
         "default": v("float", 2.0)},
        {"op": "sample", "path": "/Q.size", "time": 10.0, "value": v("float", 3.0)},
        {"op": "sample", "path": "/Q.size", "time": -1.0, "value": v("float", 1.0)},
        {"op": "sample", "path": "/Q.size", "time": 2.5, "value": v("float", 4.0)},
        {"op": "attribute", "path": "/Q.inputs:b", "type": "float", "custom": True},
        {"op": "attribute", "path": "/Q.inputs:a", "type": "float", "custom": True},
        {"op": "connect", "path": "/Q.inputs:b", "targets": ["/Q.size", "/Q.inputs:a"]},
        {"op": "relationship", "path": "/Q.rel", "custom": True,
         "targets": ["/Q", "/Q.size", "/Q.inputs:a"]},
        {"op": "metadata", "path": "/Q.rel", "key": "documentation", "value": v("string", "r")},
        {"op": "metadata", "path": "/Q.rel", "key": "customData",
         "value": dictionary(k=v("int", 1))},
    ],
    # Variant sets and variants created in reverse name order, selections
    # set backwards, and each variant's contents scrambled.
    "variants": [
        {"op": "prim", "path": "/V", "specifier": "def", "type": "Xform"},
        {"op": "variant_set", "path": "/V", "name": "zset"},
        {"op": "variant_set", "path": "/V", "name": "aset"},
        {"op": "prim", "path": "/V{zset=b}"},
        {"op": "prim", "path": "/V{zset=a}"},
        {"op": "prim", "path": "/V{aset=x}"},
        {"op": "select", "path": "/V", "set": "zset", "variant": "b"},
        {"op": "select", "path": "/V", "set": "aset", "variant": "x"},
        {"op": "attribute", "path": "/V{zset=a}.zeta", "type": "float",
         "default": v("float", 1.0)},
        {"op": "attribute", "path": "/V{zset=a}.alpha", "type": "float",
         "default": v("float", 2.0)},
        {"op": "metadata", "path": "/V{zset=a}", "key": "kind", "value": v("token", "x")},
        {"op": "active", "path": "/V{zset=a}", "value": False},
        {"op": "prim", "path": "/V{zset=a}Z", "specifier": "def"},
        {"op": "prim", "path": "/V{zset=a}A", "specifier": "def"},
        {"op": "attribute", "path": "/V{zset=b}.q", "type": "float", "default": v("float", 1.0)},
        {"op": "attribute", "path": "/V.height", "type": "double", "default": v("double", 1.0)},
        {"op": "prim", "path": "/V/Child", "specifier": "def"},
    ],
    # Layer metadata, `defaultPrim` and the comment among it, and root and
    # child prims, which keep their order.
    "layer": [
        {"op": "metadata", "path": "/", "key": "upAxis", "value": v("token", "Z")},
        {"op": "metadata", "path": "/", "key": "metersPerUnit", "value": v("double", 0.01)},
        {"op": "prim", "path": "/Z", "specifier": "def"},
        {"op": "prim", "path": "/A", "specifier": "def"},
        {"op": "prim", "path": "/Z/z", "specifier": "def"},
        {"op": "prim", "path": "/Z/a", "specifier": "def"},
        {"op": "prim", "path": "/Z/m", "specifier": "def"},
        {"op": "metadata", "path": "/", "key": "endTimeCode", "value": v("double", 10.0)},
        {"op": "metadata", "path": "/", "key": "startTimeCode", "value": v("double", 1.0)},
        {"op": "metadata", "path": "/", "key": "defaultPrim", "value": v("token", "Z")},
        {"op": "metadata", "path": "/", "key": "documentation", "value": v("string", "docs")},
        {"op": "metadata", "path": "/", "key": "comment", "value": v("string", "a layer")},
        {"op": "metadata", "path": "/", "key": "timeCodesPerSecond",
         "value": v("double", 24.0)},
        {"op": "metadata", "path": "/", "key": "framesPerSecond", "value": v("double", 24.0)},
        {"op": "metadata", "path": "/", "key": "customLayerData",
         "value": dictionary(zz=v("int", 1), Ab=v("int", 2), aa=v("int", 3))},
    ],
}

SCRAMBLED = '''#usda 1.0
(
    upAxis = "Z"
    subLayers = [
        @./b.usda@,
        @./a.usda@ (offset = 2)
    ]
    defaultPrim = "P"
    "layer comment"
    metersPerUnit = 1
    customLayerData = {
        int zz = 1
        dictionary b = {
            int y = 1
            int x = 2
        }
        int a = 2
    }
)

def "P" (
    variants = {
        string zz = "b"
        string aa = "a"
    }
    kind = "group"
    references = [
        </Q>,
        @./r.usda@</R>
    ]
    prepend variantSets = ["zz", "aa"]
    inherits = </Q>
    "prim comment"
    active = true
    customData = {
        int z = 1
        int a = 2
    }
)
{
    reorder properties = ["zeta", "alpha"]
    reorder nameChildren = ["Y", "X"]
    rel zrel = [
        </P/Y>,
        </P/X>,
    ]
    float zeta = 1
    float alpha.timeSamples = {
        1: 2,
        5: 1,
    }
    float alpha.connect = </P.zeta>
    float alpha = 3 (
        interpolation = "vertex"
        customData = {
            int b = 1
            int a = 2
        }
        doc = "x"
        "property comment"
    )
    custom rel arel
    double A2 = 1
    double a10 = 1
    double a9 = 1

    def "Y"
    {
    }

    def "X"
    {
    }
    variantSet "zz" = {
        "b" {
            float q = 1

        }
        "a" (
            kind = "x"
            active = false
        ) {
            float q = 2
            float b = 1

        }
    }
    variantSet "aa" = {
        "a" {

        }
    }
}

def "Q"
{
}
'''

UNREGISTERED = '''#usda 1.0
(
    zeta = "1"
    a10 = "x"
    a9 = "y"
    Beta = "2"
    _u = "4"
)

def "P" (
    zeta = "1"
    Beta = "2"
    a10 = "x"
    a9 = "y"
    _u = "4"
)
{
}
'''


def sdf_type(name):
    found = Sdf.ValueTypeNames.Find(name)
    assert found, name
    return found


def python_value(value):
    """A step value as `pxr` takes it for its declared type."""
    if value["type"] == "dictionary":
        return {key: python_value(entry) for key, entry in value["value"].items()}
    v = value["value"]
    if value["type"] == "double":
        return float(v)
    if value["type"].endswith("[]") and v and isinstance(v[0], list):
        return [tuple(item) for item in v]
    return v


def explicit(list_op, items):
    """Makes `list_op` explicit with `items`; an empty list is authored too
    (`= None`), which assigning no items would not do."""
    list_op.ClearEditsAndMakeExplicit()
    list_op.explicitItems = items


def run(steps):
    layer = Sdf.Layer.CreateAnonymous("scene.usda")
    for step in steps:
        op, path = step["op"], step["path"]
        if op == "prim":
            spec = Sdf.CreatePrimInLayer(layer, path)
            if "specifier" in step:
                spec.specifier = {"def": Sdf.SpecifierDef, "over": Sdf.SpecifierOver,
                                  "class": Sdf.SpecifierClass}[step["specifier"]]
            if "type" in step:
                spec.typeName = step["type"]
        elif op == "attribute":
            prop = Sdf.Path(path)
            prim = layer.GetPrimAtPath(prop.GetPrimOrPrimVariantSelectionPath())
            variability = Sdf.VariabilityUniform if step.get("uniform") else Sdf.VariabilityVarying
            attr = Sdf.AttributeSpec(prim, prop.name, sdf_type(step["type"]), variability,
                                     step.get("custom", False))
            if "default" in step:
                attr.default = python_value(step["default"])
        elif op == "relationship":
            prop = Sdf.Path(path)
            prim = layer.GetPrimAtPath(prop.GetPrimOrPrimVariantSelectionPath())
            rel = Sdf.RelationshipSpec(prim, prop.name, step.get("custom", False))
            explicit(rel.targetPathList, step["targets"])
        elif op == "sample":
            attr = layer.GetAttributeAtPath(path)
            layer.SetTimeSample(attr.path, step["time"], python_value(step["value"]))
        elif op == "connect":
            explicit(layer.GetAttributeAtPath(path).connectionPathList, step["targets"])
        elif op == "metadata":
            spec = layer.pseudoRoot if path == "/" else layer.GetObjectAtPath(path)
            spec.SetInfo(step["key"], python_value(step["value"]))
        elif op == "active":
            layer.GetPrimAtPath(path).active = step["value"]
        elif op == "instanceable":
            layer.GetPrimAtPath(path).instanceable = step["value"]
        elif op == "api_schema":
            prim = layer.GetPrimAtPath(path)
            schemas = prim.GetInfo("apiSchemas")
            if step["name"] not in schemas.prependedItems + schemas.appendedItems:
                schemas.prependedItems = list(schemas.prependedItems) + [step["name"]]
            prim.SetInfo("apiSchemas", schemas)
        elif op in ("reference", "payload"):
            prim = layer.GetPrimAtPath(path)
            offset = Sdf.LayerOffset(step.get("offset", 0.0))
            if op == "reference":
                arc = Sdf.Reference(step.get("asset", ""), step["target"], offset)
                prim.referenceList.prependedItems.append(arc)
            else:
                arc = Sdf.Payload(step.get("asset", ""), step["target"], offset)
                prim.payloadList.prependedItems.append(arc)
        elif op == "inherit":
            layer.GetPrimAtPath(path).inheritPathList.prependedItems.append(step["target"])
        elif op == "specialize":
            layer.GetPrimAtPath(path).specializesList.prependedItems.append(step["target"])
        elif op == "variant_set":
            prim = layer.GetPrimAtPath(path)
            Sdf.VariantSetSpec(prim, step["name"])
            prim.variantSetNameList.prependedItems.append(step["name"])
        elif op == "select":
            layer.GetPrimAtPath(path).variantSelections[step["set"]] = step["variant"]
        elif op == "property_order":
            layer.GetPrimAtPath(path).propertyOrder = step["names"]
        elif op == "prim_order":
            layer.GetPrimAtPath(path).nameChildrenOrder = step["names"]
        else:
            raise SystemExit(f"unknown step {op}")
    return layer.ExportToString()


def info(layer, spec, key):
    """`spec`'s `key` field as Python can hold it: `pxr` has no converter
    for the layer's `subLayerOffsets` field."""
    if key == "subLayerOffsets":
        return [(o.offset, o.scale) for o in layer.subLayerOffsets]
    return spec.GetInfo(key)


def crate_contents(layer):
    """What a crate layer holds, independent of its bytes: each spec's
    fields and values in the order OpenUSD reads them, and each prim's
    child prims, properties and variants in order. Targets and connections
    are paths without specs of their own; their order is in the fields."""
    paths = []
    layer.Traverse(Sdf.Path.absoluteRootPath, paths.append)
    contents = []
    for path in sorted(paths):
        spec = layer.pseudoRoot if path == Sdf.Path.absoluteRootPath else layer.GetObjectAtPath(path)
        if spec is None:
            continue
        fields = [(key, info(layer, spec, key)) for key in spec.ListInfoKeys()]
        prim = spec.primSpec if isinstance(spec, Sdf.VariantSpec) else spec
        order = []
        if isinstance(prim, Sdf.PrimSpec):
            order = [list(prim.nameChildren.keys()), list(prim.properties.keys())]
        elif isinstance(spec, Sdf.VariantSetSpec):
            order = [[variant.name for variant in spec.variantList]]
        contents.append((str(path), fields, order))
    return contents


def write(out, crate=None):
    """Writes the fixtures into `out`; `round_trip.usdc` is the export of
    `crate`, a crate file of `scrambled.usda`, when given, and of the
    crate file written here otherwise."""
    os.makedirs(out, exist_ok=True)
    usda = os.path.join(out, "scrambled.usda")
    with open(usda, "w") as f:
        f.write(SCRAMBLED)
    text = Sdf.Layer.FindOrOpen(usda)
    usdc = os.path.join(out, "scrambled.usdc")
    text.Export(usdc)
    binary = Sdf.Layer.FindOrOpen(crate or usdc)
    unregistered = os.path.join(out, "unregistered.usda")
    with open(unregistered, "w") as f:
        f.write(UNREGISTERED)
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "cases": [{"name": name, "steps": steps, "text": run(steps)}
                  for name, steps in CASES.items()],
        "round_trip": {
            "usda": text.ExportToString(),
            "usdc": binary.ExportToString(),
            "unregistered": Sdf.Layer.FindOrOpen(unregistered).ExportToString(),
        },
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    checked_in = os.path.join(FIXTURES, "scrambled.usdc")
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp, crate=checked_in)
        for name in ["scrambled.usda", "unregistered.usda", "oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")
        fresh = Sdf.Layer.FindOrOpen(os.path.join(tmp, "scrambled.usdc"))
        if crate_contents(fresh) != crate_contents(Sdf.Layer.FindOrOpen(checked_in)):
            raise SystemExit(f"scrambled.usdc holds other contents than {FIXTURES}'s")


if __name__ == "__main__":
    main()
