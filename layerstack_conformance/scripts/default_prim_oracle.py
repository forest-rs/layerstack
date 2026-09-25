# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD resolves references and payloads through `defaultPrim`.

Usage: default_prim_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/tests` by default:

- `assets/default_prim/usda/*.usda`: the layers below, as authored;
- `assets/default_prim/usdc/*.usdc`: the same layers written by OpenUSD as
  crate files, with asset paths pointing at the `.usdc` siblings;
- `data/default_prim.json`: what OpenUSD composes from `root`, which
  `tests/default_prim.rs` replays against layerstack for both formats.

For every composed prim the vectors record its children and prim stack, and
for a few attributes their resolved default. Each composition error is
recorded with the composed prim (`rootSite`), the arc, the target layer and
the unresolved path: `<defaultPrim>` when the layer has no usable
`defaultPrim`, else the path it names. The script fails unless OpenUSD
composes the USDA and USDC layers identically.

`default_prim_paths` records `SdfLayer::GetDefaultPrimAsPath` for a set of
authored `defaultPrim` tokens (an empty path is `null`).
"""
import json
import os
import re
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(os.path.join(HERE, "..", "tests"))

# Layers keyed by name; `{ext}` is the format's file extension.
LAYERS = {
    # The asset placed twice: its `defaultPrim` is `/Model`, and `/Other`
    # must not be composed into the placements.
    "asset": '''#usda 1.0
(
    defaultPrim = "Model"
)

def Xform "Model"
{
    double size = 2

    def Scope "Geo"
    {
        def Scope "Mesh"
        {
        }
    }
}

def Scope "Other"
{
    double size = 7

    def Scope "OtherChild"
    {
    }
}
''',
    "nodefault": '''#usda 1.0

def Scope "Model"
{
    def Scope "Child"
    {
    }
}
''',
    # Names a root prim that does not exist.
    "missing": '''#usda 1.0
(
    defaultPrim = "Missing"
)

def Scope "Model"
{
    def Scope "Child"
    {
    }
}
''',
    # Not a prim path.
    "invalid": '''#usda 1.0
(
    defaultPrim = "Model.attr"
)

def Scope "Model"
{
    def Scope "Child"
    {
    }
}
''',
    # A path relative to the pseudo-root names a subroot prim.
    "subroot": '''#usda 1.0
(
    defaultPrim = "Model/Geo"
)

def Scope "Model"
{
    def Scope "Geo"
    {
        def Scope "Mesh"
        {
        }
    }
}
''',
    # `defaultPrim` is read from the root layer of the target layer stack;
    # the prim it names may be defined in a sublayer.
    "stacked": '''#usda 1.0
(
    defaultPrim = "Model"
    subLayers = [
        @./stacked_sub.{ext}@
    ]
)
''',
    "stacked_sub": '''#usda 1.0
(
    defaultPrim = "Ignored"
)

def Scope "Model"
{
    def Scope "SubChild"
    {
    }
}

def Scope "Ignored"
{
}
''',
    # A sublayer's `defaultPrim` is not the layer stack's.
    "subonly": '''#usda 1.0
(
    subLayers = [
        @./stacked_sub.{ext}@
    ]
)
''',
    # An asset whose default prim itself references assets by `defaultPrim`.
    "wrapper": '''#usda 1.0
(
    defaultPrim = "Wrapper"
)

def Xform "Wrapper" (
    references = @./asset.{ext}@
)
{
}

def Xform "Broken" (
    payload = @./nodefault.{ext}@
)
{
}
''',
    "root": '''#usda 1.0
(
    defaultPrim = "Local"
)

def Scope "Local"
{
    double value = 7

    def Scope "LocalChild"
    {
    }
}

def "PlacementA" (
    references = @./asset.{ext}@
)
{
}

def "PlacementB" (
    payload = @./asset.{ext}@
)
{
    double size = 3
}

def "Explicit" (
    references = @./asset.{ext}@</Other>
)
{
}

def "Internal" (
    references = </Local>
)
{
}

def "InternalDefault" (
    references = <>
)
{
}

def "NoDefault" (
    references = @./nodefault.{ext}@
)
{
}

def "MissingDefault" (
    references = @./missing.{ext}@
)
{
}

def "InvalidDefault" (
    payload = @./invalid.{ext}@
)
{
}

def "Subroot" (
    references = @./subroot.{ext}@
)
{
}

def "Stacked" (
    references = @./stacked.{ext}@
)
{
}

def "SubOnly" (
    references = @./subonly.{ext}@
)
{
}

def "Nested" (
    references = @./wrapper.{ext}@
)
{
}

def "NestedUnresolved" (
    references = @./wrapper.{ext}@</Broken>
)
{
}

def "MissingAssetRef" (
    references = @./absent.{ext}@
)
{
}

def "MissingAssetPayload" (
    payload = @./absent.{ext}@
)
{
}

def "MissingAssetExplicitRef" (
    references = @./absent.{ext}@</Local>
)
{
}

def "MissingAssetExplicitPayload" (
    payload = @./absent.{ext}@</Local>
)
{
}
''',
}

# Attributes whose resolved default is recorded.
ATTRIBUTES = [
    "/PlacementA.size",
    "/PlacementB.size",
    "/Explicit.size",
    "/Nested.size",
    # The root layer's `defaultPrim` is `/Local`, which authors `value`: an
    # arc to an asset that cannot be opened must not fall back to it.
    "/Local.value",
    "/InternalDefault.value",
    "/MissingAssetRef.value",
    "/MissingAssetPayload.value",
    "/MissingAssetExplicitRef.value",
    "/MissingAssetExplicitPayload.value",
]

DEFAULT_PRIM_TOKENS = [
    "Model", "Model/Geo", "/Model/Geo", "_x", "Mödel", "1Bad", "Model.attr",
    "/", "", "./Model", "../X", "Model{v=a}", "Bad Name", "Model/", "a:b",
]

ERROR = re.compile(
    r"Unresolved (reference|payload) prim path @(.+?)@<(.*?)> "
    r"introduced by @(.+?)@<(.*?)>")
ASSET_ERROR = re.compile(
    r"Could not open asset @(.+?)@ for (reference|payload) "
    r"introduced by @(.+?)@<(.*?)>\.?", re.DOTALL)


def layer_name(identifier):
    """The layer's name without directory or extension."""
    return os.path.splitext(os.path.basename(identifier))[0]


def write_layers(directory, ext):
    os.makedirs(directory, exist_ok=True)
    for name, text in LAYERS.items():
        path = os.path.join(directory, f"{name}.{ext}")
        text = text.replace("{ext}", ext)
        if ext == "usda":
            with open(path, "w") as f:
                f.write(text)
        else:
            layer = Sdf.Layer.CreateAnonymous(".usda")
            if not layer.ImportFromString(text):
                sys.exit(f"OpenUSD rejected {name}")
            if not layer.Export(path):
                sys.exit(f"could not write {path}")


def compose(directory, ext):
    stage = Usd.Stage.Open(os.path.join(directory, f"root.{ext}"))
    prims = []
    for prim in stage.TraverseAll():
        prims.append({
            "path": str(prim.GetPath()),
            "children": [child.GetName() for child in prim.GetAllChildren()],
            "prim_stack": [[layer_name(spec.layer.identifier), str(spec.path)]
                           for spec in prim.GetPrimStack()],
        })
    values = {}
    for attr_path in ATTRIBUTES:
        attr = stage.GetAttributeAtPath(attr_path)
        values[attr_path] = attr.Get() if attr else None
    errors = []
    for error in stage.GetCompositionErrors():
        message = str(error).strip()
        root_site = re.search(r"<([^<>]*)>$", str(error.rootSite)).group(1)
        if match := ERROR.fullmatch(message):
            arc, layer, unresolved, site_layer, site_path = match.groups()
            errors.append({
                "type": error.errorType.displayName,
                "prim": root_site,
                "arc": arc,
                "layer": layer_name(layer),
                "unresolved": f"<{unresolved}>",
                "introduced_by": [layer_name(site_layer), site_path],
            })
        elif match := ASSET_ERROR.match(message):
            asset, arc, site_layer, site_path = match.groups()
            errors.append({
                "type": error.errorType.displayName,
                "prim": root_site,
                "arc": arc,
                "asset": layer_name(asset),
                "introduced_by": [layer_name(site_layer), site_path],
            })
        else:
            sys.exit(f"unexpected composition error: {message}")
    errors.sort(key=lambda e: (e["prim"], e.get("layer", ""), e.get("unresolved", "")))
    return {"prims": prims, "values": values, "errors": errors}


def default_prim_paths():
    layer = Sdf.Layer.CreateAnonymous()
    out = []
    for token in DEFAULT_PRIM_TOKENS:
        layer.defaultPrim = token
        path = layer.GetDefaultPrimAsPath()
        out.append([token, None if path.isEmpty else str(path)])
    return out


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    assets = os.path.join(out_dir, "assets", "default_prim")
    results = {}
    for ext in ("usda", "usdc"):
        directory = os.path.join(assets, ext)
        write_layers(directory, ext)
        results[ext] = compose(directory, ext)
    if results["usda"] != results["usdc"]:
        sys.exit("OpenUSD composes the USDA and USDC layers differently")
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/default_prim_oracle.py",
        "openusd_version": version,
        "formats": ["usda", "usdc"],
        "root": "root",
        **results["usda"],
        "default_prim_paths": default_prim_paths(),
    }
    out_path = os.path.join(out_dir, "data", "default_prim.json")
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(f"wrote {len(doc['prims'])} prims and {len(doc['errors'])} errors "
          f"from OpenUSD {version} to {out_path}")


if __name__ == "__main__":
    main()
