# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Validate the runnable Rust tutorial outputs with native OpenUSD.

Run each `tutorial_*` example into its own directory under the argument path.
This checks authored files, composition and binary export independently of Rust.
"""
import sys
from pathlib import Path
from pxr import Gf, Usd, UsdGeom

root = Path(sys.argv[1])

def open_stage(tutorial, file):
    stage = Usd.Stage.Open(str(root / tutorial / file))
    assert stage, (tutorial, file)
    return stage

def sphere(stage, path, color):
    prim = stage.GetPrimAtPath(path)
    assert prim and prim.GetTypeName() == "Sphere", path
    assert UsdGeom.Sphere(prim).GetRadiusAttr().Get() == 2
    assert list(UsdGeom.Gprim(prim).GetDisplayColorAttr().Get()) == [Gf.Vec3f(*color)]

hello = open_stage("tutorial_hello_world", "HelloWorld.usda")
assert str(hello.GetDefaultPrim().GetPath()) == "/hello"
sphere(hello, "/hello/world", (0, 0, 1))
assert UsdGeom.XformCache().GetLocalToWorldTransform(hello.GetPrimAtPath("/hello/world")).ExtractTranslation() == Gf.Vec3d(4, 5, 6)
references = open_stage("tutorial_references", "RefExample.usda")
sphere(references, "/refSphere/world", (0, 0, 1))
sphere(references, "/refSphere2/world", (1, 0, 0))
variants = open_stage("tutorial_variants", "HelloWorld.usda")
selection = variants.GetPrimAtPath("/hello").GetVariantSet("shadingVariant")
assert selection.GetVariantSelection() == "green"
sphere(variants, "/hello/world", (0, 1, 0))
assert selection.SetVariantSelection("red")
sphere(variants, "/hello/world", (1, 0, 0))
exported = open_stage("tutorial_stage_io", "HelloWorld.usdc")
sphere(exported, "/hello/world", (0, 0, 1))
source = open_stage("tutorial_stage_io", "HelloWorld.usda")
assert exported.Flatten(addSourceFileComment=False).ExportToString() == source.Flatten(addSourceFileComment=False).ExportToString()
print("All four Rust tutorial outputs agree with native OpenUSD")
