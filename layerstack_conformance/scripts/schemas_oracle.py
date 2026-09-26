# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's prim definitions for small codeless schema sets.

Usage: schemas_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, for each schema set below, under
`layerstack_conformance/fixtures/schemas/<set>` by default:

- `plugInfo.json` and `generatedSchema.usda`: a codeless schema plugin, as
  usdGenSchema would write it (base class properties and built-ins baked
  into each typed schema, multiple-apply names templated with
  `__INSTANCE_NAME__`);
- `scene.usda`: prims that use those schemas;
- `oracle.json`: what OpenUSD reports for every prim of `scene.usda`, which
  `tests/schemas.rs` replays against layerstack.

OpenUSD reads one set of schema plugins per process, so each set is
recorded by a child process with `PXR_PLUGINPATH_NAME` naming its
directory.

For every prim the vectors record its type name and the schema type it
resolves to (empty when the type is abstract or unknown, AOUSD Core
§13.3.1), `IsA` for each typed schema, `GetAppliedSchemas`, `HasAPI` for
each applied schema and for each instance name the prim applies, and
`GetPropertyNames` (authored and defined properties). For every property
they record whether the prim definition defines it, its kind, type name,
variability, the definition's fallback and the value `Get()` resolves at
the default time.

The sets:

- `typed_and_applied`: the AOUSD Core §13.3.1 example (`Foo`, `Bar`,
  `Baz`), the §13.3.2 example (`FooBar`, `BarBaz`) and the first
  §13.3.2.3 example (`myFoo`);
- `inclusions`: the §13.3.2.1 example, where `BazFoo:bazfooinstance`
  auto-applies to `FooBar` and `BazFoo` includes `BarBaz` by type;
- `fallback_order`: the second §13.3.2.3 example and the §13.3.2.4 one,
  where `FooBar` includes `BarBaz:myInstance` and `BazFoo:otherInstance`
  auto-applies to it;
- `coverage`: an abstract base with concrete children, unknown and
  abstract type names, nested built-ins, weaker definitions filling in a
  missing fallback, multiple-apply templates and instance names containing
  `:`, multiple-apply built-ins by type and by named instance, override
  properties (including a variability change, which is ignored, a type
  change, which drops the override, and an override of nothing),
  auto-applies to an abstract base and to an API schema, an inclusion
  cycle, invalid `apiSchemas` entries, and fallbacks shadowed by an
  authored value and by a default block;
- `openusd`: no plugin of its own but OpenUSD's own schemas, which
  `layerstack_schemas` ships: a mesh, a sphere, a material with a shader,
  a light and a prim applying `CollectionAPI:foo` and `MaterialBindingAPI`.
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(os.path.join(HERE, "..", "fixtures", "schemas"))

GENERATED_HEADER = '''#usda 1.0
(
    "Test schemas for layerstack's prim definition oracle; written as usdGenSchema writes generatedSchema.usda."
)
'''

# Each set: the schemas (identifier, kind, base identifier or None, the
# typed schemas a single-apply schema auto-applies to), plugin-level
# auto-applies (for named multiple-apply instances), the generated schema
# body and the scene.
SETS = {
    "typed_and_applied": {
        "schemas": [
            ("Foo", "abstractTyped", None, []),
            ("Bar", "concreteTyped", "Foo", []),
            ("Baz", "concreteTyped", "Foo", []),
            ("FooBar", "singleApplyAPI", None, []),
            ("BarBaz", "multipleApplyAPI", None, []),
        ],
        "auto_apply": {},
        "generated": '''
class "Foo"
{
    int fooprop = 0
}

class Bar "Bar"
{
    float barprop = 1
    int fooprop = 0
}

class Baz "Baz"
{
    int fooprop = 0
}

class "FooBar"
{
    half foobarprop = 0.5
}

class "BarBaz"
{
    string __INSTANCE_NAME__:barbazprop = ""
}
''',
        "scene": '''#usda 1.0

# AOUSD Core 13.3.1.
def Bar "bar1"
{
    string bar1prop = ""
}

def Baz "baz1"
{
    float2 baz1prop = (0, 0)
}

# AOUSD Core 13.3.2.
def "foo" (
    apiSchemas = ["FooBar"]
)
{
    double3 pt
}

def "bar" (
    apiSchemas = ["FooBar", "BarBaz:my_instance"]
)
{
    string anotherString = "default"
}

# AOUSD Core 13.3.2.3, before `FooBar` includes anything.
def Bar "myFoo" (
    apiSchemas = ["FooBar"]
)
{
    double myFoo:prop = 5
}
''',
    },
    "inclusions": {
        "schemas": [
            ("FooBar", "singleApplyAPI", None, []),
            ("BarBaz", "multipleApplyAPI", None, []),
            ("BazFoo", "multipleApplyAPI", None, []),
        ],
        "auto_apply": {"BazFoo:bazfooinstance": ["FooBar"]},
        "generated": '''
class "FooBar"
{
    half foobarprop = 0.5
}

class "BarBaz"
{
    string __INSTANCE_NAME__:barbazprop = ""
}

class "BazFoo" (
    apiSchemas = ["BarBaz:__INSTANCE_NAME__"]
)
{
    int bazfooprop:__INSTANCE_NAME__ = 1
}
''',
        "scene": '''#usda 1.0

# AOUSD Core 13.3.2.1.
def "foo" (
    apiSchemas = ["FooBar"]
)
{
}

def "twice" (
    apiSchemas = ["BazFoo:one", "BazFoo:two:deep"]
)
{
}
''',
    },
    "fallback_order": {
        "schemas": [
            ("Foo", "abstractTyped", None, []),
            ("Bar", "concreteTyped", "Foo", []),
            ("FooBar", "singleApplyAPI", None, []),
            ("BarBaz", "multipleApplyAPI", None, []),
            ("BazFoo", "multipleApplyAPI", None, []),
        ],
        "auto_apply": {"BazFoo:otherInstance": ["FooBar"]},
        "generated": '''
class "Foo"
{
    int fooprop = 0
}

class Bar "Bar"
{
    float barprop = 1
    int fooprop = 0
}

class "FooBar" (
    apiSchemas = ["BarBaz:myInstance"]
)
{
    half foobarprop = 0.5
}

class "BarBaz"
{
    string __INSTANCE_NAME__:barbazprop = ""
}

class "BazFoo" (
    apiSchemas = ["BarBaz:__INSTANCE_NAME__"]
)
{
    int bazfooprop:__INSTANCE_NAME__ = 1
}
''',
        "scene": '''#usda 1.0

# AOUSD Core 13.3.2.3 and 13.3.2.4.
def Bar "myFoo" (
    apiSchemas = ["FooBar"]
)
{
    double myFoo:prop = 5
}
''',
    },
    "coverage": {
        "schemas": [
            ("Shape", "abstractTyped", None, []),
            ("Tile", "concreteTyped", "Shape", []),
            ("Panel", "concreteTyped", "Tile", []),
            ("LabelAPI", "singleApplyAPI", None, []),
            ("StyleAPI", "singleApplyAPI", None, []),
            ("ColorAPI", "singleApplyAPI", None, []),
            ("OverrideAPI", "singleApplyAPI", None, []),
            ("AutoFirstAPI", "singleApplyAPI", None, ["Shape"]),
            ("AutoSecondAPI", "singleApplyAPI", None, ["Shape"]),
            ("SlotAPI", "multipleApplyAPI", None, []),
            ("PinAPI", "multipleApplyAPI", None, []),
            ("CycleOneAPI", "singleApplyAPI", None, []),
            ("CycleTwoAPI", "singleApplyAPI", None, []),
        ],
        "auto_apply": {"PinAPI:auto": ["LabelAPI"]},
        "generated": '''
class "Shape"
{
    uniform token shading = "flat"
    int sides = 3
}

class Tile "Tile" (
    apiSchemas = ["LabelAPI", "SlotAPI:main"]
    customData = {
        token[] apiSchemaOverridePropertyNames = ["label:text", "style:weight"]
    }
)
{
    uniform string label:text = "tile"
    uniform token shading = "flat"
    int sides = 3
    double style:weight = 2
    float width = 1
}

class Panel "Panel" (
    apiSchemas = ["StyleAPI", "LabelAPI", "SlotAPI:main"]
    customData = {
        token[] apiSchemaOverridePropertyNames = ["label:text", "style:weight"]
    }
)
{
    double depth = 0.5
    uniform string label:text = "tile"
    uniform token shading = "flat"
    int sides = 3
    double style:weight = 2
    float width = 1
}

class "LabelAPI" (
    apiSchemas = ["StyleAPI"]
)
{
    string label:note
    int label:size = 12
    string label:text = "untitled"
}

class "StyleAPI" (
    apiSchemas = ["ColorAPI"]
)
{
    int style:rank = 1
    token style:weight = "regular"
}

class "ColorAPI"
{
    rel color:source
    color3f color:value = (1, 1, 1)
    string label:note = "from color"
    string label:text = "colored"
    float style:rank = 2
}

class "OverrideAPI" (
    apiSchemas = ["LabelAPI"]
    customData = {
        token[] apiSchemaOverridePropertyNames = ["ghost", "label:size"]
    }
)
{
    int ghost = 5
    uniform int label:size = 20
    token override:mode = "strict"
}

class "AutoFirstAPI"
{
    bool auto:enabled = 1
    int auto:first = 1
}

class "AutoSecondAPI"
{
    bool auto:enabled = 0
}

class "SlotAPI" (
    apiSchemas = ["PinAPI:__INSTANCE_NAME__", "PinAPI:__INSTANCE_NAME__:extra"]
    customData = {
        token[] apiSchemaOverridePropertyNames = ["pin:__INSTANCE_NAME__:offset"]
    }
)
{
    float pin:__INSTANCE_NAME__:offset = 0.5
    uniform token slot:__INSTANCE_NAME__ = "empty"
    int slot:__INSTANCE_NAME__:index = 0
    rel slot:__INSTANCE_NAME__:targets
}

class "PinAPI"
{
    string __INSTANCE_NAME__:pinned = "no"
    float pin:__INSTANCE_NAME__:offset = 0.25
}

class "CycleOneAPI" (
    apiSchemas = ["CycleTwoAPI"]
)
{
    int cycle:one = 1
}

class "CycleTwoAPI" (
    apiSchemas = ["CycleOneAPI"]
)
{
    int cycle:one = 22
    int cycle:two = 2
}
''',
        "scene": '''#usda 1.0

# No type: only the applied schema defines properties.
def "Typeless" (
    prepend apiSchemas = ["LabelAPI"]
)
{
}

# An unknown type name is typeless (AOUSD Core 13.3.1); applied schemas
# still apply.
def Mystery "Unknown" (
    prepend apiSchemas = ["StyleAPI"]
)
{
    custom double extra = 1
}

# An abstract type name is typeless too.
def Shape "AbstractTyped"
{
}

# A typed schema with built-ins, overrides and auto-applies.
def Tile "Tile"
{
}

# Applied schemas after the type's: an override schema, multiple-apply
# instances (one with an instance name containing `:`), a repeat of a
# built-in, an unknown schema, a multiple-apply schema without an instance
# and a single-apply schema with one. Authored values shadow fallbacks; a
# default block hides one.
def Panel "Panel" (
    prepend apiSchemas = ["OverrideAPI", "SlotAPI:left:upper", "SlotAPI:right", "LabelAPI", "UnknownAPI", "SlotAPI", "LabelAPI:bad"]
)
{
    double depth = None
    int slot:right:index = 7
    float width = 5
}

# `apiSchemas` composes as a list op: the reference's list, less what this
# prim deletes, plus what it appends.
def "Composed" (
    delete apiSchemas = ["OverrideAPI"]
    append apiSchemas = ["PinAPI:tail"]
    references = </Panel>
)
{
}

# An inclusion cycle: each schema includes the other once.
def "CycleFromOne" (
    prepend apiSchemas = ["CycleOneAPI"]
)
{
}

def "CycleFromTwo" (
    prepend apiSchemas = ["CycleTwoAPI"]
)
{
}
''',
    },
    "openusd": {
        "builtin": True,
        "schemas": [
            ("Typed", "abstractTyped", None, []),
            ("Imageable", "abstractTyped", None, []),
            ("Xformable", "abstractTyped", None, []),
            ("Boundable", "abstractTyped", None, []),
            ("Gprim", "abstractTyped", None, []),
            ("PointBased", "abstractTyped", None, []),
            ("Mesh", "concreteTyped", None, []),
            ("Sphere", "concreteTyped", None, []),
            ("Xform", "concreteTyped", None, []),
            ("Scope", "concreteTyped", None, []),
            ("NodeGraph", "concreteTyped", None, []),
            ("Material", "concreteTyped", None, []),
            ("Shader", "concreteTyped", None, []),
            ("BoundableLightBase", "abstractTyped", None, []),
            ("SphereLight", "concreteTyped", None, []),
            ("MaterialBindingAPI", "singleApplyAPI", None, []),
            ("LightAPI", "singleApplyAPI", None, []),
            ("ShadowAPI", "singleApplyAPI", None, []),
            ("ShapingAPI", "singleApplyAPI", None, []),
            ("VisibilityAPI", "singleApplyAPI", None, []),
            ("GeomModelAPI", "singleApplyAPI", None, []),
            ("CollectionAPI", "multipleApplyAPI", None, []),
        ],
        "auto_apply": {},
        "scene": '''#usda 1.0

def Xform "World"
{
    def Mesh "Mesh"
    {
        int[] faceVertexCounts = [4]
    }

    def Sphere "Ball"
    {
        double radius = 2
    }

    def Material "Material"
    {
        token outputs:surface.connect = </World/Material/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            token outputs:surface
        }
    }

    def SphereLight "Light"
    {
        float inputs:intensity = 5
    }

    def Scope "Group" (
        prepend apiSchemas = ["CollectionAPI:foo", "MaterialBindingAPI"]
    )
    {
        rel material:binding = </World/Material>
    }
}
''',
    },
}


def type_name_for(identifier):
    return "LayerstackTest" + identifier


def plug_info(spec):
    identifiers = {identifier for identifier, _, _, _ in spec["schemas"]}
    types = {}
    for identifier, kind, base, auto_apply_to in spec["schemas"]:
        if base is not None:
            assert base in identifiers, base
            bases = [type_name_for(base)]
        elif kind.endswith("Typed"):
            bases = ["UsdTyped"]
        else:
            bases = ["UsdAPISchemaBase"]
        entry = {
            "alias": {"UsdSchemaBase": identifier},
            "autoGenerated": True,
            "bases": bases,
            "schemaIdentifier": identifier,
            "schemaKind": kind,
        }
        if auto_apply_to:
            entry["apiSchemaAutoApplyTo"] = auto_apply_to
        types[type_name_for(identifier)] = entry
    info = {"Types": types}
    if spec["auto_apply"]:
        info["AutoApplyAPISchemas"] = {
            name: {"apiSchemaAutoApplyTo": targets}
            for name, targets in spec["auto_apply"].items()
        }
    return {
        "Plugins": [
            {
                "Info": info,
                "Name": "layerstackTestSchemas",
                "ResourcePath": ".",
                "Root": ".",
                "Type": "resource",
            }
        ]
    }


def write_set(out_dir, name, spec):
    directory = os.path.join(out_dir, name)
    os.makedirs(directory, exist_ok=True)
    with open(os.path.join(directory, "scene.usda"), "w") as f:
        f.write(spec["scene"])
    if spec.get("builtin"):
        return directory
    with open(os.path.join(directory, "plugInfo.json"), "w") as f:
        json.dump(plug_info(spec), f, indent=4, sort_keys=True)
        f.write("\n")
    with open(os.path.join(directory, "generatedSchema.usda"), "w") as f:
        f.write(GENERATED_HEADER + spec["generated"])
    return directory


def encode(value):
    """A JSON form of a resolved value; `None` when there is none."""
    from pxr import Gf, Sdf

    if value is None:
        return None
    if isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, Sdf.AssetPath):
        return value.path
    if isinstance(value, Sdf.PathExpression):
        return value.GetText()
    if isinstance(value, (Gf.Quatd, Gf.Quatf, Gf.Quath)):
        return [float(value.GetReal())] + [float(x) for x in value.GetImaginary()]
    if isinstance(value, (Gf.Matrix2d, Gf.Matrix3d, Gf.Matrix4d)):
        return [float(x) for row in value for x in row]
    try:
        return [encode(v) for v in value]
    except TypeError:
        raise TypeError(f"cannot encode {value!r}")


def variability_name(variability):
    from pxr import Sdf

    return "uniform" if variability == Sdf.VariabilityUniform else "varying"


def record(name, spec, directory):
    """Runs in the child process, with the set's plugin registered."""
    from pxr import Usd

    registry = Usd.SchemaRegistry()
    typed = [i for i, kind, _, _ in spec["schemas"] if kind.endswith("Typed")]
    single = [i for i, kind, _, _ in spec["schemas"] if kind == "singleApplyAPI"]
    multiple = [i for i, kind, _, _ in spec["schemas"] if kind == "multipleApplyAPI"]
    for identifier in typed + single + multiple:
        assert Usd.SchemaRegistry.FindSchemaInfo(identifier) is not None, (
            f"{name}: schema {identifier} is not registered")

    stage = Usd.Stage.Open(os.path.join(directory, "scene.usda"))
    prims = []
    for prim in stage.Traverse():
        definition = prim.GetPrimDefinition()
        defined = set(definition.GetPropertyNames())
        applied = [str(s) for s in prim.GetAppliedSchemas()]
        instances = sorted({
            s.split(":", 1)[1] for s in applied if ":" in s
        } | {"absent"})
        properties = {}
        for prop in prim.GetProperties():
            prop_name = prop.GetName()
            entry = {"defined": prop_name in defined}
            if isinstance(prop, Usd.Attribute):
                attr = prim.GetAttribute(prop_name)
                entry["kind"] = "attribute"
                entry["type_name"] = str(attr.GetTypeName())
                entry["variability"] = variability_name(attr.GetVariability())
                entry["value"] = encode(attr.Get())
                entry["fallback"] = encode(
                    definition.GetAttributeFallbackValue(prop_name))
            else:
                entry["kind"] = "relationship"
            properties[prop_name] = entry
        prims.append({
            "path": str(prim.GetPath()),
            "type_name": str(prim.GetTypeName()),
            "schema_type": str(prim.GetPrimTypeInfo().GetSchemaTypeName()),
            "is_a": {t: prim.IsA(t) for t in typed},
            "applied_schemas": applied,
            "has_api": {s: prim.HasAPI(s) for s in single + multiple},
            "has_api_instance": [
                [s, instance, prim.HasAPI(s, instance)]
                for s in multiple for instance in instances
            ],
            "property_names": [str(n) for n in prim.GetPropertyNames()],
            "properties": properties,
        })
    return {
        "openusd_version": ".".join(str(v) for v in Usd.GetVersion()),
        "set": name,
        "prims": prims,
    }


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "--record":
        name, directory = sys.argv[2], sys.argv[3]
        json.dump(record(name, SETS[name], directory), sys.stdout)
        return
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    for name, spec in SETS.items():
        directory = os.path.abspath(write_set(out_dir, name, spec))
        env = dict(os.environ)
        env.pop("PXR_PLUGINPATH_NAME", None)
        if not spec.get("builtin"):
            env["PXR_PLUGINPATH_NAME"] = directory
        result = subprocess.run(
            [sys.executable, os.path.abspath(__file__), "--record", name, directory],
            env=env, check=True, capture_output=True, text=True)
        if result.stderr.strip():
            # OpenUSD warns about the inclusion cycle, the invalid
            # `apiSchemas` entries and the dropped overrides; show them.
            sys.stderr.write(f"== {name}\n{result.stderr}")
        oracle = json.loads(result.stdout)
        with open(os.path.join(directory, "oracle.json"), "w") as f:
            json.dump(oracle, f, indent=1, sort_keys=True)
            f.write("\n")
        print(f"wrote {directory}")


if __name__ == "__main__":
    main()
