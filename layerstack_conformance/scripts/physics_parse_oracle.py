# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's physics scene parse of the `physics_parse` fixtures.

Usage: physics_parse_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Runs `UsdPhysics.LoadUsdPhysicsFromRange` over each scene in
`fixtures/physics_parse` and records rigid bodies, shapes (kind, custom
geometry token, body, collision groups and materials), joints and
collision groups (merged groups and filters). `custom.usda` is parsed
twice: plain, and with custom tokens naming the shape API `MyConvexAPI`,
the shape type `MyShape` and the instancer type `MyInstancer`.

`tests/physics_parse.rs` compares everything.
"""
import argparse
import json
import pathlib

from pxr import Usd, UsdPhysics as P

ROOT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "physics_parse"
Kind = P.ObjectType
SHAPES = {
    Kind.SphereShape: "sphere", Kind.CubeShape: "cube", Kind.CapsuleShape: "capsule",
    Kind.Capsule1Shape: "capsule_1", Kind.CylinderShape: "cylinder",
    Kind.Cylinder1Shape: "cylinder_1", Kind.ConeShape: "cone", Kind.MeshShape: "mesh",
    Kind.PlaneShape: "plane", Kind.CustomShape: "custom", Kind.SpherePointsShape: "sphere_points",
}
JOINTS = {
    Kind.FixedJoint: "fixed", Kind.RevoluteJoint: "revolute", Kind.PrismaticJoint: "prismatic",
    Kind.SphericalJoint: "spherical", Kind.DistanceJoint: "distance", Kind.D6Joint: "d6",
    Kind.CustomJoint: "custom",
}


def paths(values):
    return sorted(str(p) for p in values)


def parse(path, custom):
    stage = Usd.Stage.Open(str(path))
    if custom:
        tokens = P.CustomUsdPhysicsTokens()
        tokens.shapeTokens.append("MyConvexAPI")
        tokens.shapeTokens.append("MyShape")
        tokens.instancerTokens.append("MyInstancer")
        result = P.LoadUsdPhysicsFromRange(stage, ["/"], [], tokens)
    else:
        result = P.LoadUsdPhysicsFromRange(stage, ["/"])
    out = {"rigid_bodies": {}, "shapes": {}, "joints": {}, "collision_groups": {}}
    for key, (prim_paths, descs) in result.items():
        for prim, desc in zip(prim_paths, descs):
            prim = str(prim)
            if key == Kind.RigidBody:
                out["rigid_bodies"][prim] = {"collisions": paths(desc.collisions)}
            elif key in SHAPES:
                kind = SHAPES[key]
                out["shapes"][prim] = {
                    "kind": kind,
                    "token": str(desc.customGeometryToken) if kind == "custom" else None,
                    "rigid_body": str(desc.rigidBody) or None,
                    "collision_groups": paths(desc.collisionGroups),
                    "materials": [str(m) for m in desc.materials],
                }
            elif key in JOINTS:
                out["joints"][prim] = {"kind": JOINTS[key]}
            elif key == Kind.CollisionGroup:
                out["collision_groups"][prim] = {
                    "merged_groups": paths(desc.mergedGroups),
                    "filtered_groups": paths(desc.filteredGroups),
                    "invert": desc.invertFilteredGroups,
                }
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    scenes = {}
    for scene in sorted(ROOT.glob("*.usda")):
        scenes[scene.name] = parse(scene, False)
        if scene.name == "custom.usda":
            scenes["custom.usda+tokens"] = parse(scene, True)
    result = {"openusd_version": ".".join(map(str, Usd.GetVersion()[1:])), "scenes": scenes}
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    output = ROOT / "oracle.json"
    if args.check:
        assert output.read_text() == text, "oracle.json is out of date"
    else:
        output.write_text(text)


main()
