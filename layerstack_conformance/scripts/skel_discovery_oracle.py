# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which prims OpenUSD's `UsdSkelCache` binds, and their deformation.

Usage: skel_discovery_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/skel_discovery/scene.usda` and, for each SkelRoot,
records every skinning target `ComputeSkelBindings` returns: its skeleton,
`HasJointInfluences`, `HasBlendShapes`, the blend-shaped points (weights
from the bound skeleton's animation, at default time) and the skinned
points. C++ keeps a prim whose influences are malformed (no joint
influences) and skips an inbetween whose weight is 0 or 1, with warnings.

`tests/skel_discovery.rs` compares everything.
"""
import argparse
import json
import pathlib

from pxr import Usd, UsdSkel, Vt

ROOT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "skel_discovery"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stage = Usd.Stage.Open(str(ROOT / "scene.usda"))
    cache = UsdSkel.Cache()
    roots = {}
    time = Usd.TimeCode.Default()
    for root_prim in stage.GetPseudoRoot().GetChildren():
        root = UsdSkel.Root(root_prim)
        cache.Populate(root, Usd.PrimDefaultPredicate)
        targets = []
        for binding in cache.ComputeSkelBindings(root, Usd.PrimDefaultPredicate):
            skel_query = cache.GetSkelQuery(binding.GetSkeleton())
            anim = skel_query.GetAnimQuery()
            for query in binding.GetSkinningTargets():
                prim = query.GetPrim()
                points = prim.GetAttribute("points").Get(time)
                record = {
                    "path": str(prim.GetPath()),
                    "skeleton": str(binding.GetSkeleton().GetPath()),
                    "joint_influences": query.HasJointInfluences(),
                    "blend_shapes": query.HasBlendShapes(),
                    "blend_points": None,
                    "skinned_points": None,
                }
                if query.HasBlendShapes():
                    shapes = UsdSkel.BlendShapeQuery(UsdSkel.BindingAPI(prim))
                    weights = query.GetBlendShapeMapper().Remap(anim.ComputeBlendShapeWeights(time))
                    sub_weights, shape_indices, sub_indices = shapes.ComputeSubShapeWeights(weights)
                    blended = Vt.Vec3fArray(points)
                    shapes.ComputeDeformedPoints(
                        sub_weights, shape_indices, sub_indices,
                        shapes.ComputeBlendShapePointIndices(),
                        shapes.ComputeSubShapePointOffsets(), blended)
                    record["blend_points"] = [list(p) for p in blended]
                if query.HasJointInfluences():
                    skinned = Vt.Vec3fArray(points)
                    query.ComputeSkinnedPoints(skel_query.ComputeSkinningTransforms(time), skinned, time)
                    record["skinned_points"] = [list(p) for p in skinned]
                targets.append(record)
        roots[str(root_prim.GetPath())] = sorted(targets, key=lambda r: r["path"])
    result = {"openusd_version": ".".join(map(str, Usd.GetVersion()[1:])), "roots": roots}
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    output = ROOT / "oracle.json"
    if args.check:
        assert output.read_text() == text, "oracle.json is out of date"
    else:
        output.write_text(text)


main()
