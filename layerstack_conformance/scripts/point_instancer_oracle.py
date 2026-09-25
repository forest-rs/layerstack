# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Reports what OpenUSD computes for an exported PointInstancer, as JSON.

Usage: point_instancer_oracle.py INSTANCER_PATH LAYER [LAYER...]

Opens each LAYER (USDA, USDC or USDZ) as a stage through Python `pxr` and
prints one JSON object: `version`, OpenUSD's release, and `layers`, a list
with one entry per LAYER:

- `prototypes`: the `prototypes` relationship's targets, in order;
- `protoIndices`, `ids`: the authored arrays;
- `orientationsf`, `orientations`: the authored `quatf[]` and `quath[]`
  arrays as `[x, y, z, w]` lists (empty when not authored);
- `extent`: the authored extent, and `computedExtent`,
  `UsdGeomPointInstancer::ComputeExtentAtTime` at the default time;
- `transforms`: `ComputeInstanceTransformsAtTime` at the default time
  (prototype root transforms included, nothing masked), each matrix as 16
  numbers, row by row;
- `bindings`: for every prim below the prototypes whose bound material
  (`UsdShadeMaterialBindingAPI::ComputeBoundMaterial`) is set, its path and
  the material's path; `GeomSubset` prims are included.

`tests/point_instancer.rs` compares the report with the exporter's inputs.
"""
import json
import sys

from pxr import Usd, UsdGeom, UsdShade


def quats(values):
    """`[x, y, z, w]` lists of a quaternion array, or `[]` when unauthored."""
    return [list(q.GetImaginary()) + [q.GetReal()] for q in (values or [])]


def report(layer, instancer_path):
    stage = Usd.Stage.Open(layer)
    instancer = UsdGeom.PointInstancer(stage.GetPrimAtPath(instancer_path))
    if not instancer:
        raise SystemExit(f"{layer}: no PointInstancer at {instancer_path}")
    default = Usd.TimeCode.Default()
    prototypes = instancer.GetPrototypesRel().GetTargets()
    transforms = instancer.ComputeInstanceTransformsAtTime(default, default)
    bindings = []
    for target in prototypes:
        for prim in Usd.PrimRange(stage.GetPrimAtPath(target)):
            material, _ = UsdShade.MaterialBindingAPI(prim).ComputeBoundMaterial()
            if material:
                bindings.append([str(prim.GetPath()), str(material.GetPath())])
    return {
        "prototypes": [str(p) for p in prototypes],
        "protoIndices": list(instancer.GetProtoIndicesAttr().Get(default)),
        "ids": list(instancer.GetIdsAttr().Get(default) or []),
        "orientationsf": quats(instancer.GetOrientationsfAttr().Get(default)),
        "orientations": quats(instancer.GetOrientationsAttr().Get(default)),
        "extent": [list(v) for v in instancer.GetExtentAttr().Get(default)],
        "computedExtent": [list(v) for v in instancer.ComputeExtentAtTime(default, default)],
        "transforms": [[m[r][c] for r in range(4) for c in range(4)] for m in transforms],
        "bindings": bindings,
    }


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    instancer_path, layers = sys.argv[1], sys.argv[2:]
    version = ".".join(str(v) for v in Usd.GetVersion())
    print(json.dumps({
        "version": version,
        "layers": [report(layer, instancer_path) for layer in layers],
    }))


if __name__ == "__main__":
    main()
