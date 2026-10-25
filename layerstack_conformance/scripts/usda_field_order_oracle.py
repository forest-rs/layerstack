# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the USDA text OpenUSD writes for layers authored in scrambled
orders, and for a scrambled file read back.

Usage: usda_field_order_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/usda_field_order` by
default, the files of `FILES` (`scrambled.usda`, `scrambled.usdc`, ...)
and `oracle.json`, which `tests/conformance/usda_field_order.rs` replays against
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
spec, and the name appended to the prepended `variantSetNames`, or put
first with `"position": "front"`), `remove_variant_set`
(`SdfPrimSpec::RemoveVariantSet`, which leaves the list),
`select`, `property_order` and `prim_order`. Values are
`{"type": ..., "value": ...}`, a dictionary's entries keyed by name, and
a number may be text (`"1e-7"`, `"-0"`, `"nan"`) read with `float`.
`text` is the layer's `ExportToString` after every step.

OpenUSD spells a `double` or `float` with `TfStringify`
(`pxr/base/tf/stringUtils.cpp`): the fewest digits that read back,
positional for a decimal exponent in [-6, 15) and `3.42e-8` otherwise,
ties between two such candidates broken to the even one; and a `half` or
`timecode` with a stream's `%g`, six significant digits (`0.0999756`,
`1e-07`). The `numbers` case puts such numbers everywhere a value is
written. `generated` holds, for arrays too long to keep, an FNV-1a hash of
OpenUSD's text of each: every half but the NaNs, and `float`, `double`
and `timecode` arrays drawn from `splitmix64` bits and from decimals of
up to six digits.

`round_trip` holds `ExportToString` of `scrambled.usda`, a file written
out of order by hand, and of `scrambled.usdc`, OpenUSD's crate file of it,
which keeps the file's order: both are sorted alike. `unregistered`
holds the export of `unregistered.usda`, the same unregistered metadata
keys on the layer and on a prim, which shows the two orders apart:
`Beta`, `_u`, `a10`, `a9` in the layer, `_u`, `a9`, `a10`, `Beta` on the
prim.

OpenUSD keeps metadata no schema registers as the text of its value, as
its text parser read it (`textParserHelpers.cpp`), a crate file stores
that text, and the text writer writes it back as it is. `unregistered.usdc`
and `unregistered_values.usdc`, crate files of the unregistered keys and
of unregistered values of many kinds (strings, numbers that read back
respelled, lists, a dictionary, list-op statements on the layer and a
prim), export as their text files do.

`variant_order` holds the export of `variant_order.usda`, whose
`variantSet` blocks are out of `variantSets` order, nested ones included,
and `variant_order.usdc` that of its crate file. OpenUSD writes variant
sets in the order their specs were created (`variantSetChildren`): as the
file introduces them, an explicit or added `variantSets` list first.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures. A crate file's bytes differ between
OpenUSD builds, so `--check` exports each crate file from the checked-in
one and compares the fresh crate file with it by contents: every spec's
fields, in the order OpenUSD reads them, and every prim's children,
properties and variant sets, in order.
"""
import argparse
import filecmp
import json
import os
import struct
import tempfile

from pxr import Gf, Sdf, Usd, Vt

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
    # Variant sets in the order their specs are created, which is not the
    # order `variantSets` lists them in: one added at the front of the
    # list, removed and created again there, and sets nested in a variant,
    # the second listed first.
    "variant_set_order": [
        {"op": "prim", "path": "/V", "specifier": "def"},
        {"op": "variant_set", "path": "/V", "name": "z"},
        {"op": "variant_set", "path": "/V", "name": "m", "position": "front"},
        {"op": "variant_set", "path": "/V", "name": "a"},
        {"op": "variant_set", "path": "/V", "name": "k"},
        {"op": "prim", "path": "/V{z=x}"},
        {"op": "prim", "path": "/V{m=x}"},
        {"op": "prim", "path": "/V{a=x}"},
        {"op": "prim", "path": "/V{k=x}"},
        {"op": "remove_variant_set", "path": "/V", "name": "m"},
        {"op": "variant_set", "path": "/V", "name": "m", "position": "front"},
        {"op": "prim", "path": "/V{m=y}"},
        {"op": "variant_set", "path": "/V{a=x}", "name": "q"},
        {"op": "variant_set", "path": "/V{a=x}", "name": "b", "position": "front"},
        {"op": "prim", "path": "/V{a=x}{q=x}"},
        {"op": "prim", "path": "/V{a=x}{b=x}"},
    ],
    # Numbers in every place a value is written: `double` and `float` as
    # `TfStringify` writes them (the fewest digits that read back, with an
    # exponent outside [-6, 15)), `half` and `timecode` as a stream writes
    # them (`%g`, six digits). Numbers are text, read as `float(text)`.
    "numbers": [
        {"op": "prim", "path": "/G", "specifier": "def"},
        {"op": "prim", "path": "/F", "specifier": "def"},
        {"op": "attribute", "path": "/F.d", "type": "double[]", "default": v("double[]", [
            "0.1", "3.42e-8", "1e-6", "1.5e-6", "1e-7", "1e15", "999999999999999", "1e16",
            "1e21", "1e300", "5e-324", "-0", "0", "1", "-2.5", "123456789012345678",
            "1.7976931348623157e308", "2.2250738585072014e-308", "inf", "-inf", "nan"])},
        {"op": "attribute", "path": "/F.f", "type": "float[]", "default": v("float[]", [
            "0.1", "1e-7", "1e-6", "1e15", "1e14", "123456789", "16777217", "-0", "1",
            "3.4e38", "1e-45", "1.1754944e-38", "0.3", "inf", "-inf", "nan"])},
        {"op": "attribute", "path": "/F.h", "type": "half[]", "default": v("half[]", [
            "0.1", "1e-5", "6e-8", "65504", "-0", "1", "0.0001", "0.00009", "2049", "0.333",
            "1000.5", "inf", "-inf", "nan"])},
        {"op": "attribute", "path": "/F.t", "type": "timecode[]", "default": v("timecode[]", [
            "1e-7", "1234560", "0.0001", "0.00001", "2.5", "-0", "1e-10", "100000", "1e20",
            "inf", "-inf", "nan"])},
        {"op": "attribute", "path": "/F.one", "type": "half", "default": v("half", "0.1")},
        {"op": "attribute", "path": "/F.time", "type": "timecode",
         "default": v("timecode", "1e-7")},
        {"op": "sample", "path": "/F.time", "time": "2", "value": v("timecode", "0.00001")},
        {"op": "attribute", "path": "/F.vh", "type": "half3",
         "default": v("half3", ["0.1", "1e-5", "-0"])},
        {"op": "attribute", "path": "/F.vf", "type": "float3",
         "default": v("float3", ["0.1", "1e-7", "1e15"])},
        {"op": "attribute", "path": "/F.vd", "type": "double4",
         "default": v("double4", ["1e-7", "1e15", "-0", "0.000001"])},
        {"op": "attribute", "path": "/F.qh", "type": "quath",
         "default": v("quath", ["0.1", "0.2", "0.3", "0.4"])},
        {"op": "attribute", "path": "/F.qf", "type": "quatf",
         "default": v("quatf", ["0.1", "1e-7", "0.3", "0.4"])},
        {"op": "attribute", "path": "/F.qd", "type": "quatd",
         "default": v("quatd", ["1", "3.42e-8", "-0", "1e16"])},
        {"op": "attribute", "path": "/F.m", "type": "matrix2d",
         "default": v("matrix2d", [["1e-7", "0.1"], ["1e20", "-0"]])},
        {"op": "attribute", "path": "/F.s", "type": "double", "default": v("double", "0.1")},
        {"op": "sample", "path": "/F.s", "time": "1e20", "value": v("double", "3.42e-8")},
        {"op": "sample", "path": "/F.s", "time": "1.2345678", "value": v("double", "1e15")},
        {"op": "sample", "path": "/F.s", "time": "1e-7", "value": v("double", "-0")},
        {"op": "sample", "path": "/F.s", "time": "-0", "value": v("double", "1e-7")},
        {"op": "reference", "path": "/F", "target": "/G", "offset": "1e-7",
         "scale": "1.2345678e20"},
        {"op": "metadata", "path": "/", "key": "startTimeCode", "value": v("double", "1e-7")},
        {"op": "metadata", "path": "/", "key": "endTimeCode", "value": v("double", "1e15")},
        {"op": "metadata", "path": "/F", "key": "customData",
         "value": dictionary(d=v("double", "3.42e-8"), f=v("float", "1e-7"),
                             h=v("half", "0.1"), t=v("timecode", "1e-7"),
                             hs=v("half[]", ["1e-5", "0.1"]), ts=v("timecode[]", ["1e6"]))},
    ],
}

# Generated values `generated` records OpenUSD's text of, as an FNV-1a
# hash of each array's text (`[0, 5.96046e-08, ...]`): every half but the
# NaNs, which a stream writes as the C library does (glibc's `-nan` for a
# negative one, Apple's `nan`), and
# 100000 numbers each from `splitmix64` bits (any `float` or `double`) and
# from decimals of up to six digits at scales from 10^-22 to 10^22.
GENERATED = [
    ("every half", "half[]"),
    ("float bits", "float[]"),
    ("double bits", "double[]"),
    ("decimal floats", "float[]"),
    ("decimal doubles", "double[]"),
    ("decimal time codes", "timecode[]"),
]
GENERATED_COUNT = 100000
MASK = (1 << 64) - 1


def splitmix64(state):
    while True:
        state = (state + 0x9E3779B97F4A7C15) & MASK
        z = state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
        yield z ^ (z >> 31)


def power_of_ten(k):
    p = 1.0
    for _ in range(k):
        p *= 10.0
    return p


def decimal(bits):
    """A decimal of up to six digits, `m * 10^k` or `m / 10^k`."""
    m = bits % 1999999 - 999999
    k = (bits >> 32) % 23
    return m / power_of_ten(k) if bits >> 63 else m * power_of_ten(k)


def as_float(x):
    """`x` rounded to a `float`, as a C++ cast rounds it."""
    return struct.unpack("<f", struct.pack("<f", x))[0]


def generated(name):
    """The values of the generated array `name`, the same in
    `tests/conformance/usda_field_order.rs`."""
    if name == "every half":
        # But the NaNs: the C library spells their sign, or not.
        return [struct.unpack("<e", struct.pack("<H", bits))[0] for bits in range(1 << 16)
                if bits & 0x7C00 != 0x7C00 or not bits & 0x3FF]
    bits = splitmix64({"float bits": 1, "double bits": 2, "decimal floats": 3,
                       "decimal doubles": 4, "decimal time codes": 5}[name])
    draws = [next(bits) for _ in range(GENERATED_COUNT)]
    if name == "float bits":
        return [struct.unpack("<f", struct.pack("<I", b >> 32))[0] for b in draws]
    if name == "double bits":
        return [struct.unpack("<d", struct.pack("<Q", b))[0] for b in draws]
    if name == "decimal floats":
        return [as_float(decimal(b)) for b in draws]
    return [decimal(b) for b in draws]


def fnv1a64(text):
    h = 0xCBF29CE484222325
    for byte in text.encode():
        h = ((h ^ byte) * 0x100000001B3) & MASK
    return f"{h:016x}"


def generated_texts():
    """OpenUSD's text of each generated array, hashed."""
    layer = Sdf.Layer.CreateAnonymous("generated.usda")
    prim = Sdf.CreatePrimInLayer(layer, "/P")
    prim.specifier = Sdf.SpecifierDef
    for i, (name, type_name) in enumerate(GENERATED):
        attr = Sdf.AttributeSpec(prim, f"a{i}", sdf_type(type_name))
        attr.default = array(type_name, generated(name))
    lines = {line.split(" = ", 1)[0].split()[-1]: line.split(" = ", 1)[1]
             for line in layer.ExportToString().splitlines() if " a" in line and " = [" in line}
    return [{"name": name, "type": type_name, "count": len(generated(name)),
             "fnv1a64": fnv1a64(lines[f"a{i}"])}
            for i, (name, type_name) in enumerate(GENERATED)]

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


UNREGISTERED_VALUES = '''#usda 1.0
(
    s = "1"
    n = 1.50
    i = -3
    big = 18446744073709551615
    tiny = 0.0000000342
    ints = [1, 2]
    words = ["a", "b"]
    one = ["a"]
    flag = true
    dict = {
        int a = 1
        string b = "x"
    }
    prepend tags = ["a", "b"]
    append ids = [1, 2]
    delete ids = [3]
)

def "P" (
    q = 'it"s'
    note = "a \\"quoted\\" note"
    explicit = ["x"]
    prepend tags = ["c"]
)
{
}
'''

VARIANT_ORDER = '''#usda 1.0

def "P" (
    prepend variantSets = ["b", "a", "c"]
)
{
    variantSet "c" = {
        "x" {
            float q = 1
        }
    }
    variantSet "a" = {
        "y" (
            prepend variantSets = ["n2", "n1"]
        ) {
            variantSet "n1" = {
                "k" {
                }
            }
            variantSet "n2" = {
                "k" {
                }
            }
        }
    }
    variantSet "b" = {
        "x" {
        }
    }
}

def "Q" (
    variantSets = ["s", "r"]
)
{
    variantSet "t" = {
        "x" {
        }
    }
    variantSet "r" = {
        "x" {
        }
    }
    variantSet "s" = {
        "x" {
        }
    }
}
'''

# Files written by hand, read back, and for those marked, exported as a
# crate file and that read back: `round_trip` keys each export by the
# file's stem, and a crate's by the stem and `.usdc`, but the first is
# kept as `usda` and `usdc`.
FILES = [
    ("scrambled", SCRAMBLED, True),
    ("unregistered", UNREGISTERED, True),
    ("unregistered_values", UNREGISTERED_VALUES, True),
    ("variant_order", VARIANT_ORDER, True),
]


def sdf_type(name):
    found = Sdf.ValueTypeNames.Find(name)
    assert found, name
    return found


def array(type_name, items):
    """A `Vt` array of `type_name` (`half[]` and its siblings)."""
    return {"half[]": Vt.HalfArray, "float[]": Vt.FloatArray, "double[]": Vt.DoubleArray,
            "timecode[]": Sdf.TimeCodeArray}[type_name](items)


def python_value(value):
    """A step value as `pxr` takes it for its declared type. Numbers given
    as text are read with `float`."""
    type_name, v = value["type"], value["value"]
    if type_name == "dictionary":
        return {key: python_value(entry) for key, entry in v.items()}
    if type_name in ("half[]", "float[]", "double[]", "timecode[]"):
        return array(type_name, [float(x) for x in v])
    if type_name in ("half", "float"):
        # Typed, for a dictionary entry: a Python `float` is a `double`.
        return {"half": Vt.Half, "float": Vt.Float}[type_name](float(v))
    if type_name == "double":
        return float(v)
    if type_name == "timecode":
        return Sdf.TimeCode(float(v))
    if type_name[:-1] in ("half", "float", "double") and type_name[-1].isdigit():
        vec = {"half": "Vec%sh", "float": "Vec%sf", "double": "Vec%sd"}[type_name[:-1]]
        return getattr(Gf, vec % type_name[-1])(*[float(x) for x in v])
    if type_name in ("quath", "quatf", "quatd"):
        quat, vec = {"quath": (Gf.Quath, Gf.Vec3h), "quatf": (Gf.Quatf, Gf.Vec3f),
                     "quatd": (Gf.Quatd, Gf.Vec3d)}[type_name]
        r, i, j, k = [float(x) for x in v]
        return quat(r, vec(i, j, k))
    if type_name == "matrix2d":
        return Gf.Matrix2d(*[float(x) for row in v for x in row])
    if type_name.endswith("[]") and v and isinstance(v[0], list):
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
            layer.SetTimeSample(attr.path, float(step["time"]), python_value(step["value"]))
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
            offset = Sdf.LayerOffset(float(step.get("offset", 0.0)), float(step.get("scale", 1.0)))
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
            names = prim.variantSetNameList.prependedItems
            if step["name"] in names:
                names.remove(step["name"])
            if step.get("position") == "front":
                names.insert(0, step["name"])
            else:
                names.append(step["name"])
        elif op == "remove_variant_set":
            # `SdfPrimSpec::RemoveVariantSet`: the spec goes, the list stays.
            del layer.GetPrimAtPath(path).variantSets[step["name"]]
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
            order = [list(prim.nameChildren.keys()), list(prim.properties.keys()),
                     list(prim.variantSets.keys())]
        elif isinstance(spec, Sdf.VariantSetSpec):
            order = [[variant.name for variant in spec.variantList]]
        contents.append((str(path), fields, order))
    return contents


def write(out, checked_in=None):
    """Writes the fixtures into `out`. With `checked_in`, the fixtures
    directory, each crate file is exported from the one there rather than
    the one written here."""
    os.makedirs(out, exist_ok=True)
    round_trip = {}
    for stem, source, crate in FILES:
        usda = os.path.join(out, f"{stem}.usda")
        with open(usda, "w") as f:
            f.write(source)
        text = Sdf.Layer.FindOrOpen(usda)
        round_trip["usda" if stem == "scrambled" else stem] = text.ExportToString()
        if crate:
            usdc = os.path.join(out, f"{stem}.usdc")
            text.Export(usdc)
            binary = Sdf.Layer.FindOrOpen(os.path.join(checked_in, f"{stem}.usdc")
                                          if checked_in else usdc)
            key = "usdc" if stem == "scrambled" else f"{stem}.usdc"
            round_trip[key] = binary.ExportToString()
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "cases": [{"name": name, "steps": steps, "text": run(steps)}
                  for name, steps in CASES.items()],
        "generated": generated_texts(),
        "round_trip": round_trip,
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
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp, checked_in=FIXTURES)
        names = [f"{stem}.usda" for stem, _, _ in FILES] + ["oracle.json"]
        for name in names:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")
        for stem, _, crate in FILES:
            if not crate:
                continue
            fresh = Sdf.Layer.FindOrOpen(os.path.join(tmp, f"{stem}.usdc"))
            kept = Sdf.Layer.FindOrOpen(os.path.join(FIXTURES, f"{stem}.usdc"))
            if crate_contents(fresh) != crate_contents(kept):
                raise SystemExit(f"{stem}.usdc holds other contents than {FIXTURES}'s")


if __name__ == "__main__":
    main()
