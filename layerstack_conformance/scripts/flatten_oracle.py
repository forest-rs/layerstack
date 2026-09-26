# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""OpenUSD's flatten of a composed stage, for `tests/flatten.rs`.

Usage (with a Python that imports OpenUSD's `pxr`):

    flatten_oracle.py version
    flatten_oracle.py batch JOBS OUT

JOBS is a JSON list of jobs, each
`{"root": ..., "flattened": [...], "export": ..., "times": [...],
"fallbacks": {...}}`. For each job, OpenUSD opens `root` as a stage with
the global variant fallbacks `fallbacks`, flattens it with
`UsdStage::Flatten` (no source comment) and exports the result to
`export`. OUT receives, per job:

- `openusd`: the layer OpenUSD's flatten writes, as `layer_dump` reads it;
- `ours`: each layer in `flattened` (Layerstack's flatten of the same
  stage, saved as USDA and USDC), read the same way;
- `stage`: what the stage opened from `root` composes, as `stage_dump`
  reads it;
- `openusd_reopened`: what OpenUSD's own flatten composes when OpenUSD
  opens it as a stage;
- `reopened`: what each layer in `flattened` composes when OpenUSD opens it
  as a stage.

`layer_dump` records every spec of a layer and each of its fields as
OpenUSD reads them: list ops as the lists they apply to an empty list,
dictionaries by key, values by `repr`. Two layers that author the same
thing have equal dumps, with two normalizations:

- the generated prototypes `Flattened_Prototype_N` are named after the
  path of their first instance, since OpenUSD numbers them in the order of
  its instance cache;
- for the same reason the prototypes lead the root prims in name order.

`stage_dump` records every active prim OpenUSD composes, instance proxies
included, except the flattened prototypes, with its specifier, type name,
metadata and children, and every property with its `IsCustom`, metadata
(type name and variability included), connections or targets, default
value, sample times and value at each sample time and each job time.
"""

import json
import re
import sys

from pxr import Sdf, Ts, Usd

PROTOTYPE = re.compile(r"Flattened_Prototype_(\d+)(?!\d)")


def fmt_spline(spline):
    """A spline's every field: `TsSpline`'s `repr` names only the object."""
    def extrapolation(e):
        # `loopBoundaryTime` has no Python binding.
        return [repr(e.mode), repr(e.slope)]
    loop = spline.GetInnerLoopParams()
    return {
        "valueType": spline.GetValueTypeName(),
        "curveType": repr(spline.GetCurveType()),
        "pre": extrapolation(spline.GetPreExtrapolation()),
        "post": extrapolation(spline.GetPostExtrapolation()),
        "loop": [repr(loop.protoStart), repr(loop.protoEnd), loop.numPreLoops,
                 loop.numPostLoops, repr(loop.valueOffset)],
        "knots": [
            [repr(k.GetTime()), repr(k.GetValue()),
             repr(k.GetPreValue()) if k.IsDualValued() else None,
             repr(k.GetNextInterpolation()),
             repr(k.GetPreTanWidth()), repr(k.GetPreTanSlope()),
             repr(k.GetPostTanWidth()), repr(k.GetPostTanSlope()),
             repr(k.GetPreTanAlgorithm()), repr(k.GetPostTanAlgorithm())]
            for k in spline.GetKnots().values()
        ],
    }


def fmt(value):
    if isinstance(value, Ts.Spline):
        return fmt_spline(value)
    if hasattr(value, "ApplyOperations"):
        return [fmt(item) for item in value.ApplyOperations([])]
    if isinstance(value, Sdf.Reference):
        return {
            "asset": value.assetPath,
            "prim": str(value.primPath),
            "offset": [value.layerOffset.offset, value.layerOffset.scale],
        }
    if isinstance(value, dict):
        return {str(k): fmt(v) for k, v in sorted(value.items())}
    if isinstance(value, (list, tuple)) and not hasattr(value, "__array__"):
        return [fmt(v) for v in value]
    if isinstance(value, Sdf.Path):
        return str(value)
    return repr(value)


def layer_dump(layer):
    specs = {}

    def visit(path):
        spec = layer.GetObjectAtPath(path)
        if spec is None:
            return
        entry = {"kind": type(spec).__name__}
        for key in spec.ListInfoKeys():
            value = spec.GetInfo(key)
            if key == "timeSamples":
                value = {repr(t): fmt(v) for t, v in sorted(value.items())}
            else:
                value = fmt(value)
            entry[key] = value
        if isinstance(spec, Sdf.PrimSpec):
            entry["children"] = list(spec.nameChildren.keys())
        specs[str(path)] = entry

    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    specs["/"]["children"] = list(layer.rootPrims.keys())
    specs = json.loads(PROTOTYPE.sub(prototype_labels(specs), json.dumps(specs, sort_keys=True)))
    # Prototypes first, by name: OpenUSD writes them in the order of its
    # instance cache.
    roots = specs["/"]["children"]
    prototypes = sorted(r for r in roots if r.startswith("Flattened_Prototype["))
    specs["/"]["children"] = prototypes + [r for r in roots if r not in prototypes]
    return specs


def prototype_labels(specs):
    """Names each prototype after the first of its instances, by path: an
    instance nested in another prototype is named through that prototype's
    own name, so prototypes are named innermost last."""
    instances = {}
    for path, entry in specs.items():
        for ref in entry.get("references", []):
            match = PROTOTYPE.fullmatch(ref["prim"].lstrip("/"))
            if match and ref["asset"] == "":
                instances.setdefault(match.group(1), []).append(path)
    labels = {}

    def label(path):
        match = PROTOTYPE.match(path.lstrip("/"))
        if match is None:
            return path
        if match.group(1) not in labels:
            return None
        return labels[match.group(1)] + path[match.end() + 1:]

    while True:
        ready = {
            number: [label(p) for p in paths]
            for number, paths in instances.items()
            if number not in labels and all(label(p) is not None for p in paths)
        }
        if not ready:
            break
        for number, paths in ready.items():
            labels[number] = "Flattened_Prototype[" + min(paths) + "]"
    return lambda m: labels.get(m.group(1), m.group(0))


def metadata(obj, skip=()):
    return {
        str(k): fmt(v) for k, v in sorted(obj.GetAllMetadata().items()) if k not in skip
    }


def stage_dump(stage, times):
    prims = {}
    predicate = Usd.TraverseInstanceProxies(Usd.PrimAllPrimsPredicate)
    for prim in Usd.PrimRange.Stage(stage, predicate):
        # The prototypes a flattened layer adds for instances are not prims
        # of the stage it was flattened from.
        if not prim.IsActive() or PROTOTYPE.match(str(prim.GetPath()).lstrip("/")):
            continue
        properties = {}
        for prop in prim.GetProperties():
            # OpenUSD resolves the `custom` metadata from the weakest
            # opinion; `IsCustom` (and AOUSD Core §12.2.4) from any.
            entry = {"custom": prop.IsCustom(), "metadata": metadata(prop, skip=("custom",))}
            if isinstance(prop, Usd.Attribute):
                samples = list(prop.GetTimeSamples())
                entry["samples"] = [repr(t) for t in samples]
                entry["default"] = fmt(prop.Get())
                entry["values"] = {
                    repr(t): fmt(prop.Get(Usd.TimeCode(t)))
                    for t in sorted(set(samples) | set(times))
                }
                entry["connections"] = [str(p) for p in prop.GetConnections()]
            else:
                entry["targets"] = [str(p) for p in prop.GetTargets()]
            properties[prop.GetName()] = entry
        prims[str(prim.GetPath())] = {
            # An unauthored specifier is `over`.
            "specifier": repr(prim.GetSpecifier()),
            "metadata": metadata(prim, skip=("specifier",)),
            "children": [c.GetName() for c in prim.GetFilteredChildren(predicate)
                         if c.IsActive() and not PROTOTYPE.match(c.GetName())],
            "properties": properties,
        }
    return prims


def run(job):
    Usd.Stage.SetGlobalVariantFallbacks(
        {k: list(v) for k, v in job.get("fallbacks", {}).items()})
    stage = Usd.Stage.Open(job["root"])
    if stage is None:
        raise SystemExit(f"OpenUSD cannot open {job['root']}")
    times = job.get("times", [])
    flat = stage.Flatten(False)
    flat.Export(job["export"])
    result = {
        "openusd": layer_dump(Sdf.Layer.FindOrOpen(job["export"])),
        "stage": stage_dump(stage, times),
        "openusd_reopened": stage_dump(
            Usd.Stage.Open(Sdf.Layer.FindOrOpen(job["export"])), times),
        "ours": [],
        "reopened": [],
    }
    for path in job["flattened"]:
        layer = Sdf.Layer.FindOrOpen(path)
        if layer is None:
            result["ours"].append(f"OpenUSD cannot open {path}")
            result["reopened"].append(None)
            continue
        result["ours"].append(layer_dump(layer))
        result["reopened"].append(stage_dump(Usd.Stage.Open(layer), times))
    return result


def main(argv):
    if argv[1:2] == ["version"]:
        print(".".join(str(v) for v in Usd.GetVersion()))
    elif argv[1:2] == ["batch"] and len(argv) == 4:
        with open(argv[2]) as f:
            jobs = json.load(f)
        results = [run(job) for job in jobs]
        with open(argv[3], "w") as f:
            json.dump(results, f, indent=1, sort_keys=True)
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
