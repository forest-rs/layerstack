# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""OpenUSD's view of a layer, for the authored-layer save round trip.

Usage (with a Python that imports OpenUSD's `pxr`):

    layer_snapshot.py version
    layer_snapshot.py snapshot LAYER [WEAKER]   # JSON on stdout
    layer_snapshot.py compose LAYER TIME...     # JSON on stdout
    layer_snapshot.py convert IN OUT     # OpenUSD writes IN as OUT
    layer_snapshot.py serve              # the above, one per line of stdin

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
prints what it composes: every prim's specifier, type and variant
selections, every attribute's value by default and at each TIME, every
relationship's targets, and the number of composition errors.

`serve` answers many commands from one process, so a caller pays for
starting Python and loading `pxr` once: each line of standard input is a
JSON array of a command's arguments, and each is answered by a line of JSON
on standard output, `{"out": ...}` with what the command prints or
`{"error": ...}` with why it failed. A command holds no layer once it has
answered, and OpenUSD keeps a layer open only while something holds it, so
every command reads its files as they are on disk when it runs.
"""

import gc
import json
import sys
import traceback

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


def value_repr(value):
    # OpenUSD 26.8 can return an unapplied PathExpressionArrayEdit from Get.
    # Its Python repr embeds an address. Keep that distinction explicit and
    # compare its application over an empty array; the authored snapshot above
    # separately compares the full edit program, including every instruction.
    if isinstance(value, getattr(Sdf, "PathExpressionArrayEdit", ())):
        return {
            "unapplied": "Sdf.PathExpressionArrayEdit",
            "over_empty": repr(value.ComposeOver(Sdf.PathExpressionArray())),
        }
    return repr(value)


def compose(path, times):
    stage = Usd.Stage.Open(path)
    if stage is None:
        raise SystemExit(f"OpenUSD cannot open {path}")
    prims = []
    for prim in stage.TraverseAll():
        properties = {}
        for prop in prim.GetProperties():
            if isinstance(prop, Usd.Attribute):
                values = [value_repr(prop.Get())]
                values += [value_repr(prop.Get(Usd.TimeCode(t))) for t in times]
                properties[prop.GetName()] = values
            else:
                properties[prop.GetName()] = [str(t) for t in prop.GetTargets()]
        prims.append(
            {
                "path": str(prim.GetPath()),
                "specifier": str(prim.GetSpecifier()),
                "type": str(prim.GetTypeName()),
                "variants": dict(prim.GetVariantSets().GetAllVariantSelections()),
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


def run(args):
    """Runs the command `args`, returning what it prints, if anything."""
    if args == ["version"]:
        return ".".join(str(v) for v in Usd.GetVersion())
    if args[:1] == ["snapshot"] and len(args) in (2, 3):
        weaker = args[2] if len(args) == 3 else None
        return json.dumps(snapshot(args[1], weaker), indent=1, sort_keys=True)
    if args[:1] == ["compose"] and len(args) >= 2:
        times = [float(t) for t in args[2:]]
        return json.dumps(compose(args[1], times), indent=1, sort_keys=True)
    if args[:1] == ["convert"] and len(args) == 3:
        layer = Sdf.Layer.FindOrOpen(args[1])
        if layer is None or not layer.Export(args[2]):
            raise SystemExit(f"OpenUSD cannot convert {args[1]}")
        return None
    raise SystemExit(__doc__)


def serve():
    for line in sys.stdin:
        try:
            reply = {"out": run(json.loads(line)) or ""}
        except SystemExit as error:
            reply = {"error": str(error.code)}
        except Exception:
            reply = {"error": traceback.format_exc()}
        # Frees the layers of any reference cycle the command left.
        gc.collect()
        print(json.dumps(reply), flush=True)


def main(argv):
    if argv[1:] == ["serve"]:
        serve()
        return
    out = run(argv[1:])
    if out is not None:
        print(out)


if __name__ == "__main__":
    main(sys.argv)
