# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""OpenUSD's view of a layer, for the authored-layer save round trip.

Usage (with a Python that imports OpenUSD's `pxr`):

    layer_snapshot.py version
    layer_snapshot.py snapshot LAYER [WEAKER]   # JSON on stdout
    layer_snapshot.py compose LAYER TIME...     # JSON on stdout
    layer_snapshot.py convert IN OUT     # OpenUSD writes IN as OUT

A snapshot holds what OpenUSD reads from the layer: its text as
`SdfLayer::ExportToString` prints it (every spec and field with its type,
whitespace normalized), the authored order of each prim's children and
properties (which the text printer sorts), and each prim's `ClaimsAPI`
profile record as `UsdProfilesClaimsAPI` reads it, when that schema is
available. Two layers with equal snapshots author the same thing.

With WEAKER, the snapshot also holds what a stage composing LAYER over
WEAKER (as its two sublayers) resolves: every prim's applied schemas,
relationship targets and attribute connections, so explicit-empty lists
and list edits are checked against the opinions they block or edit.

`compose` opens LAYER as a stage, with the assets its arcs name, and
prints what it composes: every prim's specifier and type, every
attribute's value by default and at each TIME, every relationship's
targets, and the number of composition errors.
"""

import json
import sys

from pxr import Sdf, Usd


def composed(path, weaker):
    root = Sdf.Layer.CreateAnonymous(".usda")
    root.subLayerPaths.append(path)
    root.subLayerPaths.append(weaker)
    stage = Usd.Stage.Open(root)
    out = {}
    for prim in stage.TraverseAll():
        entry = {"apiSchemas": [str(s) for s in prim.GetAppliedSchemas()]}
        for rel in prim.GetRelationships():
            entry[rel.GetName()] = [str(t) for t in rel.GetTargets()]
        for attr in prim.GetAttributes():
            if attr.HasAuthoredConnections():
                entry[attr.GetName()] = [str(c) for c in attr.GetConnections()]
        out[str(prim.GetPath())] = entry
    return out


def compose(path, times):
    stage = Usd.Stage.Open(path)
    if stage is None:
        raise SystemExit(f"OpenUSD cannot open {path}")
    prims = []
    for prim in stage.TraverseAll():
        properties = {}
        for prop in prim.GetProperties():
            if isinstance(prop, Usd.Attribute):
                values = [repr(prop.Get())]
                values += [repr(prop.Get(Usd.TimeCode(t))) for t in times]
                properties[prop.GetName()] = values
            else:
                properties[prop.GetName()] = [str(t) for t in prop.GetTargets()]
        prims.append(
            {
                "path": str(prim.GetPath()),
                "specifier": str(prim.GetSpecifier()),
                "type": str(prim.GetTypeName()),
                "properties": properties,
            }
        )
    errors = stage.GetCompositionErrors() if hasattr(stage, "GetCompositionErrors") else []
    return {"prims": prims, "errors": len(errors)}


def snapshot(path, weaker=None):
    layer = Sdf.Layer.FindOrOpen(path)
    if layer is None:
        raise SystemExit(f"OpenUSD cannot open {path}")
    order = {}

    def visit(spec_path):
        spec = layer.GetObjectAtPath(spec_path)
        if isinstance(spec, Sdf.PrimSpec):
            order[str(spec_path)] = {
                "children": list(spec.nameChildren.keys()),
                "properties": list(spec.properties.keys()),
            }

    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    order["/"] = {"children": list(layer.rootPrims.keys()), "properties": []}

    claims = {}
    try:
        from pxr import UsdProfiles
    except ImportError:
        UsdProfiles = None
    if UsdProfiles is not None:
        stage = Usd.Stage.Open(layer)
        for prim in stage.TraverseAll():
            info = UsdProfiles.ClaimsAPI(prim).GetProfilesInfo()
            claims[str(prim.GetPath())] = repr(sorted(dict(info).items()))

    result = {
        "text": layer.ExportToString(),
        "order": order,
        "claims": claims,
    }
    if weaker is not None:
        result["composed"] = composed(path, weaker)
    return result


def main(argv):
    if argv[1:2] == ["version"]:
        from pxr import Usd as _Usd

        print(".".join(str(v) for v in _Usd.GetVersion()))
    elif argv[1:2] == ["snapshot"] and len(argv) in (3, 4):
        weaker = argv[3] if len(argv) == 4 else None
        print(json.dumps(snapshot(argv[2], weaker), indent=1, sort_keys=True))
    elif argv[1:2] == ["compose"] and len(argv) >= 3:
        times = [float(t) for t in argv[3:]]
        print(json.dumps(compose(argv[2], times), indent=1, sort_keys=True))
    elif argv[1:2] == ["convert"] and len(argv) == 4:
        layer = Sdf.Layer.FindOrOpen(argv[2])
        if layer is None or not layer.Export(argv[3]):
            raise SystemExit(f"OpenUSD cannot convert {argv[2]}")
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
