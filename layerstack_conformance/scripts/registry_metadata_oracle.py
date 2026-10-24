# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD types plugin-registered metadata fields; run with
usd-core 26.8.

Usage: registry_metadata_oracle.py [--check]

Registers two codeless plugins whose `plugInfo.json` declares `SdfMetadata`:
`fixtures/registry_metadata` (a field of every declared type, with and
without defaults, on every kind of spec) and `fixtures/registry_metadata/
rejected` (declarations OpenUSD refuses, and a few it takes that look
wrong). It records, call by call:

- `registration`: the errors registration raises, and for every field the
  spec kinds it is metadata on and its fallback;
- `builtins`: every metadata field Sdf and usd-core's own plugins register,
  with the spec kinds it is metadata on, so the built-in table layerstack
  types fields by can be checked whole;
- `reading`: USDA metadata statements on a layer, prim, variant, attribute
  or relationship spec, each in a layer of its own: the value the text
  parser stores (or that it keeps an unregistered field's text), or the
  error that fails the layer, and the line the text writer writes back,
  and whether reading that again gives the same value;
- `authoring`: `SetMetadata` calls on a prim, attribute, relationship and
  the stage with values of a given type, what each stores or why it is
  refused, the line saving writes and whether reopening gives it back;
- `unauthored`: what `GetMetadata` and `HasMetadata` answer for a field no
  layer authors.

It writes `fixtures/registry_metadata/oracle.json`, which
`tests/registry_metadata.rs` replays against layerstack reading the same
declarations; with `--check` it writes nothing and fails on a difference.
Values are recorded by content (a float field's `0.25` as the number), not
by bytes, and the values are ones every platform writes alike.
"""
import argparse
import json
import re
from pathlib import Path

from pxr import Gf, Plug, Sdf, Tf, Usd, Vt

FIXTURE = Path(__file__).resolve().parent.parent / "fixtures" / "registry_metadata"
PLUGINS = [FIXTURE, FIXTURE / "rejected"]
DECLARED = {}
for plugin in PLUGINS:
    info = json.loads((plugin / "plugInfo.json").read_text())
    DECLARED.update(info["Plugins"][0]["Info"]["SdfMetadata"])
SPECS = ["layer", "prim", "variant", "attribute", "relationship"]

# USDA literals for a field of each declared type: valid ones, the
# spellings the text parser converts, and ones it rejects.
LITERALS = {
    "bool": ["true", "0", "2", "1.5", '"yes"', '"off"', '"maybe"', "[true]"],
    "uchar": ["200", "300", "-1", "1.75"],
    "int": ["-7", "2147483648", '"7"', "2.5"],
    "uint": ["4294967295", "-1"],
    "int64": ["-9223372036854775808", "9223372036854775808"],
    "uint64": ["18446744073709551615", "-1"],
    "half": ["0.5", "1"],
    "float": ["1", "0.25", '"x"', "(1, 2)", "None"],
    "double": ["2", "0.125"],
    "timecode": ["12", "1.5"],
    "string": ['"text"', "3"],
    "token": ['"medium"', "3", "@a.usda@"],
    "asset": ["@a.usda@", '"a.usda"'],
    "pathExpression": ['"/A/**"'],
    "float3": ["(1, 2, 3)", "(1, 2)", "1", "[(1, 2, 3)]"],
    "color3f": ["(0.5, 0.25, 1)"],
    "quatf": ["(1, 0, 0, 0)"],
    "matrix4d": [
        "((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (2, 3, 4, 1))",
        "((1, 0), (0, 1))",
    ],
    "bool[]": ["[true, 0]", "[]", "true"],
    "int[]": ["[1, 2]", "[]", "1"],
    "float[]": ["[0.5, 1]"],
    "token[]": ['["a", "b"]', "[]"],
    "string[]": ['["a"]'],
    "asset[]": ["[@a.usda@, @b.usda@]", "[]"],
    "float3[]": ["[(1, 2, 3)]", "[(1, 2)]"],
    "matrix2d[]": ["[((1, 0), (0, 1))]"],
    "dictionary": ["{ int a = 1 }", "{\n}", "1"],
    "tokenlistop": ['["a", "b"]', "[]", "None", "[1]"],
    "stringlistop": ['["a", "b"]'],
    "intlistop": ["[-2147483648, 2147483647]", "[2147483648]"],
    "uintlistop": ["[4294967295]", "[-1]"],
    "int64listop": ["[-9223372036854775808, 9223372036854775807]"],
    "uint64listop": ["[18446744073709551615]"],
}

# Statements beyond `field = literal` at the field's own spec kind.
EXTRA_STATEMENTS = [
    # List-op statements, and list-op statements of fields that are not
    # list ops.
    ("prim", 'prepend metaTokenList = ["a"]'),
    ("prim", 'append metaTokenList = ["b"]'),
    ("prim", 'delete metaTokenList = ["c"]'),
    ("prim", 'add metaTokenList = ["d"]'),
    ("prim", 'reorder metaTokenList = ["e"]'),
    ("prim", 'prepend metaStringList = ["a"]\n    append metaStringList = ["b"]'),
    ("prim", "prepend metaIntList = [1, 2]"),
    ("prim", "delete metaUInt64List = [3]"),
    ("prim", "prepend metaFloat = [1]"),
    ("prim", "prepend metaInts = [1, 2]"),
    ("prim", "delete metaBool = [1]"),
    # Fields on spec kinds they are not registered for are kept as
    # unregistered text.
    ("prim", 'metaToken = "medium"'),
    ("attribute", "metaFloat = 1"),
    ("relationship", "metaBool = 1"),
    ("layer", "metaUChar = 300"),
    ("prim", "metaFloats = [0.5]"),
    ("prim", "metaVariantOnly = 1"),
    ("variant", "metaVariantOnly = 1"),
    ("variant", "metaFloats = [0.5]"),
    ("variant", "metaFloat = 1"),
    ("prim", "metaNowhere = 1"),
    ("prim", "metaDouble = 2"),
    ("layer", "metaInt = 7"),
    ("layer", "metaLoose = 7"),
    ("relationship", "metaLoose = 7"),
    ("prim", "undeclaredField = 1.50"),
    ("prim", 'prepend undeclaredList = ["a"]'),
    # Plugin fields and built-in fields alike.
    ("attribute", "interpolation = 3"),
    ("prim", "interpolation = 3"),
    ("prim", "instanceable = 0"),
    ("layer", "metersPerUnit = 1"),
    ("attribute", "weight = 1"),
    ("prim", "weight = 1"),
    ("relationship", "noLoadHint = 1"),
    ("attribute", "noLoadHint = 1"),
    ("prim", "acceptOpaque = None"),
    # Fields of the spec that are not metadata.
    ("prim", 'specifier = "def"'),
    ("relationship", 'variability = "uniform"'),
    ("layer", 'primOrder = ["P"]'),
    ("prim", 'typeName = "Scope"'),
]

# Typed sample values for authoring, by declared type: Python constructs
# them with their exact type; the Rust replay builds the same.
SAMPLES = {
    "bool": True,
    "uchar": 200,
    "int": -7,
    "uint": 4000000000,
    "int64": -9000000000,
    "uint64": 18446744073709551615,
    "half": 0.5,
    "float": 0.25,
    "double": 2.5,
    "timecode": 12.5,
    "string": "text",
    "token": "medium",
    "asset": "a.usda",
    "pathExpression": "/A/**",
    "float3": [1, 2, 3],
    "color3f": [0.5, 0.25, 1],
    "quatf": [1, 0, 0, 0],
    "matrix4d": [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [2, 3, 4, 1]],
    "bool[]": [True, False],
    "int[]": [1, 2],
    "float[]": [0.5, 1.5],
    "token[]": ["a", "b"],
    "string[]": ["a"],
    "asset[]": ["a.usda", "b.usda"],
    "float3[]": [[1, 2, 3]],
    "matrix2d[]": [[[1, 0], [0, 1]]],
    "dictionary": {"a": {"int": 1}},
    "tokenlistop": {"prepended": ["a"], "deleted": ["b"]},
    "stringlistop": {"explicit": ["a"]},
    "intlistop": {"appended": [-2147483648]},
    "uintlistop": {"prepended": [4294967295]},
    "int64listop": {"explicit": [-9223372036854775808]},
    "uint64listop": {"deleted": [18446744073709551615]},
}

ARRAYS = {
    "bool[]": Vt.BoolArray,
    "int[]": Vt.IntArray,
    "float[]": Vt.FloatArray,
    "token[]": Vt.TokenArray,
    "string[]": Vt.StringArray,
}
LIST_OPS = {
    "tokenlistop": Sdf.TokenListOp,
    "stringlistop": Sdf.StringListOp,
    "intlistop": Sdf.IntListOp,
    "uintlistop": Sdf.UIntListOp,
    "int64listop": Sdf.Int64ListOp,
    "uint64listop": Sdf.UInt64ListOp,
}
LIST_PARTS = {
    "explicit": "explicitItems",
    "prepended": "prependedItems",
    "appended": "appendedItems",
    "deleted": "deletedItems",
    "added": "addedItems",
    "ordered": "orderedItems",
}


def python_value(type_name, sample):
    """`sample` as a Python value of the declared type."""
    scalar = {
        "bool": Vt.Bool,
        "uchar": Vt.UChar,
        "int": Vt.Int,
        "uint": Vt.UInt,
        "int64": Vt.Int64,
        "uint64": Vt.UInt64,
        "half": Vt.Half,
        "float": Vt.Float,
        "double": Vt.Double,
        "string": Vt.String if hasattr(Vt, "String") else str,
        "token": Vt.Token,
    }
    if type_name in scalar:
        return scalar[type_name](sample)
    if type_name == "timecode":
        return Sdf.TimeCode(sample)
    if type_name == "asset":
        return Sdf.AssetPath(sample)
    if type_name == "pathExpression":
        return Sdf.PathExpression(sample)
    if type_name in ("float3", "color3f"):
        return Gf.Vec3f(*sample)
    if type_name == "quatf":
        return Gf.Quatf(sample[0], Gf.Vec3f(*sample[1:]))
    if type_name == "matrix4d":
        return Gf.Matrix4d(*[c for row in sample for c in row])
    if type_name in ARRAYS:
        return ARRAYS[type_name](sample)
    if type_name == "asset[]":
        return Sdf.AssetPathArray([Sdf.AssetPath(s) for s in sample])
    if type_name == "float3[]":
        return Vt.Vec3fArray([Gf.Vec3f(*v) for v in sample])
    if type_name == "matrix2d[]":
        return Vt.Matrix2dArray(
            [Gf.Matrix2d(*[c for row in m for c in row]) for m in sample])
    if type_name == "dictionary":
        return {k: python_value(*next(iter(v.items()))) for k, v in sample.items()}
    if type_name in LIST_OPS:
        op = LIST_OPS[type_name]()
        for part, items in sample.items():
            setattr(op, LIST_PARTS[part], items)
        return op
    raise SystemExit(f"no sample constructor for {type_name}")


def encode(value):
    """A JSON form of a value read from OpenUSD, by content."""
    if value is None:
        return None
    if isinstance(value, Sdf.UnregisteredValue):
        inner = value.value
        if isinstance(inner, str):
            return {"unregistered": inner}
        if isinstance(inner, Sdf.UnregisteredValueListOp):
            return {"unregistered_list": encode_list_op(inner, str)}
        return {"unregistered": encode(inner)}
    if isinstance(value, (bool, int, str)):
        return value
    if isinstance(value, float):
        return value
    if isinstance(value, Sdf.AssetPath):
        return {"asset": value.authoredPath}
    if isinstance(value, Sdf.PathExpression):
        return {"expression": value.GetText()}
    if isinstance(value, Sdf.TimeCode):
        return float(value)
    if isinstance(value, Sdf.OpaqueValue):
        return {"opaque": True}
    if isinstance(value, (Gf.Quatf, Gf.Quatd, Gf.Quath)):
        return [value.GetReal(), *value.GetImaginary()]
    if isinstance(value, (Gf.Matrix2d, Gf.Matrix3d, Gf.Matrix4d)):
        n = {Gf.Matrix2d: 2, Gf.Matrix3d: 3, Gf.Matrix4d: 4}[type(value)]
        return [[value[r][c] for c in range(n)] for r in range(n)]
    if isinstance(value, dict):
        return {"dictionary": {k: encode(v) for k, v in sorted(value.items())}}
    for op_type in LIST_OPS.values():
        if isinstance(value, op_type):
            return encode_list_op(value, encode)
    if hasattr(value, "__len__"):
        return [encode(item) for item in value]
    raise SystemExit(f"cannot encode {value!r}")


def encode_list_op(op, item):
    out = {}
    if op.isExplicit:
        out["explicit"] = [item(v) for v in op.explicitItems]
    for part, attr in LIST_PARTS.items():
        if part != "explicit" and getattr(op, attr):
            out[part] = [item(v) for v in getattr(op, attr)]
    return {"list_op": out}


def value_types():
    """Sdf's value type names by the C++ type they hold, without roles."""
    names = {}
    for attr in dir(Sdf.ValueTypeNames):
        name = getattr(Sdf.ValueTypeNames, attr)
        if isinstance(name, Sdf.ValueTypeName) and not name.role:
            names.setdefault(name.type.typeName, str(name))
    return names


INTEGERS = {"long long": "int64", "long": "int64", "unsigned long long": "uint64",
            "unsigned long": "uint64", "int": "int", "unsigned int": "uint"}


def sdf_type_name(spec, field):
    """The Sdf name of a field's type, the same on every platform."""
    cpp = spec.GetTypeForInfo(field).typeName
    match = re.fullmatch(r"SdfListOp<(.*)>", cpp)
    if match:
        item = {"TfToken": "token", "string": "string",
                "SdfPayload": "payload", "SdfReference": "reference",
                "SdfPath": "path"}.get(match.group(1)) or INTEGERS[match.group(1)]
        return f"{item}listop"
    if cpp == "VtDictionary":
        return "dictionary"
    if cpp == "SdfPermission":
        return "permission"
    return VALUE_TYPES[cpp]


def first_error(error):
    """The first message of an error OpenUSD raises, without its source
    location, layer position, plugin path or Python repr."""
    message = str(error).strip()
    for line in message.splitlines():
        line = line.strip()
        if line.startswith("Error in"):
            message = line.split(" : ", 1)[-1].strip()
            break
    if message.startswith("'"):
        message = message.removeprefix("'").removesuffix("'")
    message = re.sub(r" at '.*' in <.*>$", "", message)
    message = re.sub(r" ?\(?in plugin \"[^\"]*\"\)?", "", message)
    message = re.sub(r", and cannot be set on UsdStage .*$", "", message)
    # A Python repr, which can name an address.
    message = re.sub(r"^Invalid value '.*' \(type '", "Invalid value (type '", message)
    return message.strip()


def layer_text(spec, statement):
    if spec == "layer":
        return f"#usda 1.0\n(\n    {statement}\n)\n"
    if spec == "prim":
        return f'#usda 1.0\ndef "P" (\n    {statement}\n)\n{{\n}}\n'
    if spec == "variant":
        return ('#usda 1.0\ndef "P" (\n    variants = {\n        string v = "x"\n    }\n'
                '    prepend variantSets = "v"\n)\n{\n    variantSet "v" = {\n'
                f'        "x" (\n            {statement}\n        ) {{\n        }}\n    }}\n}}\n')
    if spec == "attribute":
        return f'#usda 1.0\ndef "P"\n{{\n    float a (\n        {statement}\n    )\n}}\n'
    if spec == "relationship":
        return f'#usda 1.0\ndef "P"\n{{\n    rel r (\n        {statement}\n    )\n}}\n'
    raise SystemExit(spec)


def spec_of(layer, spec):
    return {
        "layer": lambda: layer.pseudoRoot,
        "prim": lambda: layer.GetPrimAtPath("/P"),
        "variant": lambda: layer.GetObjectAtPath("/P{v=x}"),
        "attribute": lambda: layer.GetAttributeAtPath("/P.a"),
        "relationship": lambda: layer.GetRelationshipAtPath("/P.r"),
    }[spec]()


def field_of(statement):
    first = statement.split("\n")[0]
    return first.split("=")[0].split()[-1]


def written(text, field):
    """The statement of `field` the text writer wrote, its lines stripped."""
    lines = text.split("\n")
    pattern = re.compile(
        r"^\s*((prepend|append|delete|add|reorder) )?" + re.escape(field) + r" = ")
    out = []
    for i, line in enumerate(lines):
        if not pattern.match(line):
            continue
        out.append(line.strip())
        if line.rstrip().endswith("{"):
            indent = len(line) - len(line.lstrip())
            for rest in lines[i + 1:]:
                out.append(rest.strip())
                if rest.strip() == "}" and len(rest) - len(rest.lstrip()) == indent:
                    break
    return "\n".join(out)


def read_case(spec, statement):
    field = field_of(statement)
    layer = Sdf.Layer.CreateAnonymous(".usda")
    case = {"spec": spec, "statement": statement}
    try:
        layer.ImportFromString(layer_text(spec, statement))
    except Tf.ErrorException as error:
        case["error"] = first_error(error)
        return case
    target = spec_of(layer, spec)
    text = layer.ExportToString()
    case["written"] = written(text, field)
    if field not in target.GetMetaDataInfoKeys():
        # Kept as the text the parser read, and written back from it.
        case["unregistered"] = True
        return case
    if not target.HasInfo(field):
        case["absent"] = True
        return case
    value = target.GetInfo(field)
    again = Sdf.Layer.CreateAnonymous(".usda")
    again.ImportFromString(text)
    case["value"] = encode(value)
    case["reopened"] = spec_of(again, spec).GetInfo(field) == value
    return case


def author_case(spec, field, type_name, sample):
    stage = Usd.Stage.CreateInMemory()
    prim = stage.DefinePrim("/P")
    target = {
        "prim": prim,
        "attribute": prim.CreateAttribute("a", Sdf.ValueTypeNames.Float),
        "relationship": prim.CreateRelationship("r"),
        "stage": stage,
    }[spec]
    case = {"spec": spec, "field": field, "type": type_name, "value": sample}
    try:
        value = python_value(type_name, sample)
        ok = target.SetMetadata(field, value)
    except (Tf.ErrorException, ValueError) as error:
        case["error"] = first_error(error)
        return case
    if not ok:
        case["error"] = "refused"
        return case
    stored = target.GetMetadata(field)
    case["stored"] = encode(stored)
    text = stage.GetRootLayer().ExportToString()
    case["written"] = written(text, field)
    reopened = Usd.Stage.Open(Sdf.Layer.CreateAnonymous(".usda"))
    reopened.GetRootLayer().ImportFromString(text)
    again = {
        "prim": lambda: reopened.GetPrimAtPath("/P"),
        "attribute": lambda: reopened.GetPrimAtPath("/P").GetAttribute("a"),
        "relationship": lambda: reopened.GetPrimAtPath("/P").GetRelationship("r"),
        "stage": lambda: reopened,
    }[spec]()
    case["reopened"] = again.GetMetadata(field) == stored
    return case


AUTHORING = [
    # Each field, on a kind of spec it is metadata on.
    *[(spec, field) for field, spec in [
        ("metaBool", "prim"), ("metaBool", "attribute"), ("metaUChar", "prim"),
        ("metaInt", "prim"), ("metaInt", "stage"), ("metaUInt", "prim"),
        ("metaInt64", "prim"), ("metaUInt64", "prim"), ("metaHalf", "attribute"),
        ("metaFloat", "prim"), ("metaDouble", "stage"), ("metaTimeCode", "prim"),
        ("metaString", "attribute"), ("metaString", "relationship"),
        ("metaToken", "relationship"), ("metaAsset", "prim"),
        ("metaExpression", "prim"), ("metaFloat3", "prim"),
        ("metaColor", "attribute"), ("metaQuat", "prim"), ("metaMatrix", "prim"),
        ("metaBools", "prim"), ("metaInts", "prim"), ("metaTokens", "prim"),
        ("metaStrings", "prim"), ("metaAssets", "prim"), ("metaVectors", "prim"),
        ("metaMatrices", "prim"), ("metaDictionary", "prim"),
        ("metaDictionary", "attribute"), ("metaTokenList", "prim"),
        ("metaStringList", "prim"), ("metaIntList", "prim"),
        ("metaUIntList", "prim"), ("metaInt64List", "prim"),
        ("metaUInt64List", "prim"), ("metaAnywhere", "stage"),
        ("metaAnywhere", "relationship"), ("metaLoose", "attribute"),
    ]],
    # Spec kinds a field is not metadata on.
    ("prim", "metaToken"), ("attribute", "metaFloat"), ("prim", "metaString"),
    ("prim", "metaDouble"), ("stage", "metaBool"), ("prim", "metaFloats"),
    ("prim", "metaVariantOnly"), ("prim", "metaNowhere"),
]
WRONG_TYPES = [
    # A value of another type.
    ("prim", "metaFloat", "string", "wide"),
    ("relationship", "metaToken", "float3", [1, 2, 3]),
    ("prim", "metaInts", "token[]", ["a"]),
    ("prim", "metaBool", "string", "yes"),
    ("prim", "metaTokenList", "stringlistop", {"explicit": ["a"]}),
    ("prim", "metaDictionary", "token", "medium"),
    # A field nothing registers, and built-in fields.
    ("prim", "undeclaredField", "int", 1),
    ("prim", "kind", "token", "component"),
    ("attribute", "interpolation", "token", "vertex"),
    ("prim", "interpolation", "token", "vertex"),
    ("stage", "upAxis", "token", "Z"),
    ("attribute", "weight", "float", 0.5),
]


def declared_type(field):
    return DECLARED[field]["type"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    global VALUE_TYPES
    VALUE_TYPES = value_types()
    errors = []
    for plugin in PLUGINS:
        try:
            Plug.Registry().RegisterPlugins(str(plugin))
        except Tf.ErrorException as error:
            errors.append(error)
    # Registration errors surface on the next call that reports them.
    try:
        Sdf.Layer.CreateAnonymous()
    except Tf.ErrorException as error:
        errors.append(error)
    messages = []
    for error in errors:
        for line in str(error).strip().splitlines():
            if line.strip().startswith("Error in"):
                messages.append(first_error(line))
    layer = Sdf.Layer.CreateAnonymous()
    prim = Sdf.CreatePrimInLayer(layer, "/P")
    variant = Sdf.VariantSpec(Sdf.VariantSetSpec(prim, "v"), "x")
    specs = {
        "layer": layer.pseudoRoot,
        "prim": prim,
        "variant": variant,
        "attribute": Sdf.AttributeSpec(prim, "a", Sdf.ValueTypeNames.Float),
        "relationship": Sdf.RelationshipSpec(prim, "r"),
    }

    def kinds(field):
        return [s for s in SPECS if field in specs[s].GetMetaDataInfoKeys()]

    registration = {}
    for field in sorted(DECLARED):
        where = kinds(field)
        if field in ("documentation", "interpolation"):
            continue
        entry = {"specs": where}
        if where:
            entry["type"] = sdf_type_name(specs[where[0]], field)
            entry["fallback"] = encode(specs[where[0]].GetFallbackForInfo(field))
        registration[field] = entry
    builtins = {}
    for spec in SPECS:
        for field in specs[spec].GetMetaDataInfoKeys():
            if field in DECLARED and field not in ("documentation", "interpolation"):
                continue
            entry = builtins.setdefault(field, {
                "type": sdf_type_name(specs[spec], field), "specs": []})
            entry["specs"].append(spec)
            if entry["type"] not in ("payloadlistop", "permission"):
                entry["fallback"] = encode(specs[spec].GetFallbackForInfo(field))

    reading = []
    for field, declaration in sorted(DECLARED.items()):
        if not isinstance(declaration, dict) or field not in registration:
            continue
        where = registration[field]["specs"]
        if not where:
            continue
        for literal in LITERALS.get(declaration["type"], []):
            reading.append(read_case(where[0], f"{field} = {literal}"))
    for spec, statement in EXTRA_STATEMENTS:
        reading.append(read_case(spec, statement))

    authoring = []
    for spec, field in AUTHORING:
        type_name = declared_type(field)
        authoring.append(author_case(spec, field, type_name, SAMPLES[type_name]))
    for spec, field, type_name, sample in WRONG_TYPES:
        authoring.append(author_case(spec, field, type_name, sample))

    stage = Usd.Stage.CreateInMemory()
    prim = stage.DefinePrim("/P")
    objects = {
        "prim": prim,
        "attribute": prim.CreateAttribute("a", Sdf.ValueTypeNames.Float),
        "relationship": prim.CreateRelationship("r"),
        "stage": stage,
    }
    spec_kind = {"prim": "prim", "attribute": "attribute",
                 "relationship": "relationship", "stage": "layer"}
    unauthored = []
    for name, target in objects.items():
        for field in sorted(registration):
            if spec_kind[name] not in registration[field]["specs"]:
                continue
            unauthored.append({
                "object": name,
                "field": field,
                "value": encode(target.GetMetadata(field)),
                "has": bool(target.HasMetadata(field)),
                "has_authored": bool(target.HasAuthoredMetadata(field)),
            })

    text = json.dumps({
        "openusd_version": ".".join(str(v) for v in Usd.GetVersion()),
        "registration_errors": sorted(messages),
        "registration": registration,
        "builtins": {k: v for k, v in sorted(builtins.items())},
        "reading": reading,
        "authoring": authoring,
        "unauthored": unauthored,
    }, indent=1, sort_keys=True) + "\n"
    snapshot = FIXTURE / "oracle.json"
    if args.check:
        if snapshot.read_text() != text:
            raise SystemExit(f"OpenUSD's registered metadata differs from {snapshot}")
    else:
        snapshot.write_text(text)
        print(f"wrote {snapshot}: {len(reading)} reads, {len(authoring)} edits")


if __name__ == "__main__":
    main()
