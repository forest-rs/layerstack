# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's material-bind subsets as a renderer reads them.

Usage: material_subsets_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Opens `fixtures/material_subsets/scene.usda` and, for every mesh, records
`UsdShadeMaterialBindingAPI::GetMaterialBindSubsetsFamilyType`, the
subsets `GetMaterialBindSubsets` returns that `UsdImagingMeshAdapter`
turns into face sets (element type `face`), each one's indices and
`ComputeBoundMaterial`, the mesh's own bound material, and
`UsdGeomSubset::ValidateFamily(face, materialBind)`. The renderer reads
the subsets whether or not the family validates.

It also replays `CreateMaterialBindSubset` and
`SetMaterialBindSubsetsFamilyType` call by call on a new stage, recording
whether each call is accepted, the subset path it returns and the family
type after it.

`tests/conformance/material_subsets.rs` compares everything.
"""
import argparse
import json
import pathlib

from pxr import Sdf, Tf, Usd, UsdGeom, UsdShade, Vt

ROOT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "material_subsets"


def bound(prim):
    material, _ = UsdShade.MaterialBindingAPI(prim).ComputeBoundMaterial()
    return str(material.GetPath()) if material else None


def read(stage):
    meshes = {}
    for prim in stage.Traverse():
        if not prim.IsA(UsdGeom.Mesh):
            continue
        api = UsdShade.MaterialBindingAPI(prim)
        faces = []
        for subset in api.GetMaterialBindSubsets():
            element = subset.GetElementTypeAttr().Get()
            indices = subset.GetIndicesAttr().Get()
            if element != UsdGeom.Tokens.face or indices is None:
                continue
            faces.append({
                "path": str(subset.GetPath()),
                "indices": list(indices),
                "material": bound(subset.GetPrim()),
            })
        valid, _ = UsdGeom.Subset.ValidateFamily(
            UsdGeom.Imageable(prim), UsdGeom.Tokens.face, "materialBind")
        meshes[str(prim.GetPath())] = {
            "family_type": str(api.GetMaterialBindSubsetsFamilyType()),
            "members": [str(s.GetPath()) for s in api.GetMaterialBindSubsets()],
            "subsets": faces,
            "fallback": bound(prim),
            "valid": valid,
        }
    return meshes


def call(log, label, action):
    try:
        result = action()
        log.append({"call": label, "accepted": bool(result), "result": result})
    except Tf.ErrorException:
        log.append({"call": label, "accepted": False, "result": None})


def authoring():
    stage = Usd.Stage.CreateInMemory()
    mesh = UsdGeom.Mesh.Define(stage, "/M")
    mesh.CreateFaceVertexCountsAttr([3, 3, 3, 3])
    mesh.CreateFaceVertexIndicesAttr([0, 1, 2] * 4)
    mesh.CreatePointsAttr([(0, 0, 0), (1, 0, 0), (0, 1, 0)])
    api = UsdShade.MaterialBindingAPI.Apply(mesh.GetPrim())
    family = lambda: str(api.GetMaterialBindSubsetsFamilyType())
    log = []

    def create(name, indices):
        subset = api.CreateMaterialBindSubset(name, Vt.IntArray(indices))
        return {
            "path": str(subset.GetPath()),
            "indices": list(subset.GetIndicesAttr().Get()),
            "family_type": family(),
        }

    def set_type(token):
        ok = api.SetMaterialBindSubsetsFamilyType(token)
        return {"family_type": family()} if ok else None

    call(log, "create red [0, 1]", lambda: create("red", [0, 1]))
    call(log, "create red [2]", lambda: create("red", [2]))
    call(log, "set unrestricted", lambda: set_type(UsdGeom.Tokens.unrestricted))
    call(log, "set partition", lambda: set_type(UsdGeom.Tokens.partition))
    call(log, "create blue [3]", lambda: create("blue", [3]))
    # The generic setter accepts `unrestricted`; the next create re-types it.
    UsdGeom.Subset.SetFamilyType(mesh, "materialBind", UsdGeom.Tokens.unrestricted)
    call(log, "create green [0]", lambda: create("green", [0]))
    call(log, "set nonOverlapping", lambda: set_type(UsdGeom.Tokens.nonOverlapping))
    layer = stage.GetRootLayer()
    family_spec = layer.GetAttributeAtPath("/M.subsetFamily:materialBind:familyType")
    return {
        "calls": log,
        "subsets": [str(s.GetPath()) for s in api.GetMaterialBindSubsets()],
        "family_variability": str(family_spec.variability),
        "family_custom": family_spec.custom,
        "subset_specifier": str(layer.GetPrimAtPath("/M/red").specifier),
        "subset_type": layer.GetPrimAtPath("/M/red").typeName,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stage = Usd.Stage.Open(str(ROOT / "scene.usda"))
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "meshes": read(stage),
        "authoring": authoring(),
    }
    output = ROOT / "oracle.json"
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.check:
        assert output.read_text() == text, "oracle.json is out of date"
    else:
        output.write_text(text)


main()
