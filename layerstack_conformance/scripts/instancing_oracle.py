# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Reports what OpenUSD draws for an exported scene, as JSON.

Usage: instancing_oracle.py LAYER [LAYER...]

Opens each LAYER (USDA, USDC or USDZ) as a stage through Python `pxr` and
prints one JSON object: `version`, OpenUSD's release, and `layers`, a list
with one entry per LAYER:

- `pointInstancers`: how many `PointInstancer` prims the stage has,
  counting abstract prims and instance prototypes;
- `instances`: every prim outside abstract prims that authors a
  reference, in traversal order: its `path`, `name`, `instanceable`,
  `target` (the internal reference's prim path), `world` transform
  (`UsdGeomXformCache::GetLocalToWorldTransform`, 16 numbers, row by row),
  `id` (the custom `instancer:id`, or null), `primvars` (each authored
  primvar's flattened value, by name without `primvars:`, every element
  a list) and `indices` (the indices of those that are indexed);
- `placed`: every mesh drawn, once per placement: meshes reached by
  default traversal (instance proxies included) that no `PointInstancer`
  holds, at their world transforms, and each mesh below a
  `PointInstancer` prototype once per instance, at the transform
  `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime` gives (the
  prototype root's transform included) composed with the mesh's
  transform relative to the prototype root and the instancer's
  world transform. Each entry has `material` (the bound material's path
  from `UsdShadeMaterialBindingAPI::ComputeBoundMaterial`, or ""),
  `triangles` (a fan per face) and `points` (world-space points);
- `extents`: for each `PointInstancer` outside prototypes, its `path`,
  authored `extent`, and the extent OpenUSD `computed` from its
  prototypes and instances (`ComputeExtentAtTime`).

`tests/instanced_references.rs` compares the reports of the
`PointInstancer` and instanced-reference forms of one scene.
"""
import json
import sys

from pxr import Gf, Usd, UsdGeom, UsdShade


def matrix(m):
    return [m[r][c] for r in range(4) for c in range(4)]


def mesh_entry(mesh_prim, world):
    mesh = UsdGeom.Mesh(mesh_prim)
    material, _ = UsdShade.MaterialBindingAPI(mesh_prim).ComputeBoundMaterial()
    counts = mesh.GetFaceVertexCountsAttr().Get() or []
    points = mesh.GetPointsAttr().Get() or []
    return {
        "material": str(material.GetPath()) if material else "",
        "triangles": sum(max(0, n - 2) for n in counts),
        "points": [list(world.Transform(Gf.Vec3d(p))) for p in points],
    }


def under_instancer(prim):
    parent = prim.GetParent()
    while parent:
        if parent.IsA(UsdGeom.PointInstancer):
            return True
        parent = parent.GetParent()
    return False


def reference_target(prim):
    refs = prim.GetMetadata("references")
    if not refs:
        return ""
    items = list(refs.prependedItems) + list(refs.explicitItems) + list(refs.appendedItems)
    return str(items[0].primPath) if items else ""


def report(layer):
    stage = Usd.Stage.Open(layer)
    default = Usd.TimeCode.Default()
    cache = UsdGeom.XformCache(default)
    everything = Usd.PrimRange.Stage(stage, Usd.TraverseInstanceProxies(Usd.PrimAllPrimsPredicate))
    instancers = sum(1 for p in everything if p.IsA(UsdGeom.PointInstancer))
    instancers += sum(
        1
        for prototype in stage.GetPrototypes()
        for p in Usd.PrimRange(prototype)
        if p.IsA(UsdGeom.PointInstancer)
    )
    instances = []
    placed = []
    extents = []
    for prim in stage.Traverse(Usd.TraverseInstanceProxies()):
        if prim.HasAuthoredReferences() and not prim.IsInstanceProxy():
            authored = UsdGeom.PrimvarsAPI(prim).GetAuthoredPrimvars()
            primvars = {
                pv.GetPrimvarName(): [list(v) if hasattr(v, "__len__") else [v] for v in pv.ComputeFlattened()]
                for pv in authored
            }
            indices = {
                pv.GetPrimvarName(): list(pv.GetIndices()) for pv in authored if pv.IsIndexed()
            }
            id_attr = prim.GetAttribute("instancer:id")
            instances.append({
                "path": str(prim.GetPath()),
                "name": prim.GetName(),
                "instanceable": prim.IsInstanceable(),
                "target": reference_target(prim),
                "world": matrix(cache.GetLocalToWorldTransform(prim)),
                "id": id_attr.Get() if id_attr and id_attr.HasAuthoredValue() else None,
                "primvars": primvars,
                "indices": indices,
            })
        if prim.IsA(UsdGeom.Mesh) and not under_instancer(prim):
            placed.append(mesh_entry(prim, cache.GetLocalToWorldTransform(prim)))
        if prim.IsA(UsdGeom.PointInstancer) and not under_instancer(prim):
            instancer = UsdGeom.PointInstancer(prim)
            to_world = cache.GetLocalToWorldTransform(prim)
            extents.append({
                "path": str(prim.GetPath()),
                "extent": [list(v) for v in instancer.GetExtentAttr().Get(default)],
                "computed": [list(v) for v in instancer.ComputeExtentAtTime(default, default)],
            })
            targets = instancer.GetPrototypesRel().GetTargets()
            transforms = instancer.ComputeInstanceTransformsAtTime(default, default)
            indices = instancer.GetProtoIndicesAttr().Get(default)
            for index, transform in zip(indices, transforms):
                root = stage.GetPrimAtPath(targets[index])
                for mesh in Usd.PrimRange(root, Usd.TraverseInstanceProxies()):
                    if not mesh.IsA(UsdGeom.Mesh):
                        continue
                    if mesh == root:
                        relative = Gf.Matrix4d(1)
                    else:
                        relative, _ = cache.ComputeRelativeTransform(mesh, root)
                    placed.append(mesh_entry(mesh, relative * transform * to_world))
    return {
        "pointInstancers": instancers,
        "instances": instances,
        "placed": placed,
        "extents": extents,
    }


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    version = ".".join(str(v) for v in Usd.GetVersion())
    print(json.dumps({
        "version": version,
        "layers": [report(layer) for layer in sys.argv[1:]],
    }))


if __name__ == "__main__":
    main()
