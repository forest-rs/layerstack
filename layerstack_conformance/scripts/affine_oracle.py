# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin complete affine Gf factors, including shear, reflection and singular cases."""
import argparse
import json
import pathlib
from pxr import Gf, Usd
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
identity = Gf.Matrix4d(1)
matrices = [identity, Gf.Matrix4d(1).SetTranslate(Gf.Vec3d(1,2,3))]
for scale in [(2,3,4),(-2,3,4),(-2,-3,4),(0,2,3),(0,0,0),(1e-8,1e4,1e4)]:
    matrices.append(Gf.Matrix4d(1).SetScale(Gf.Vec3d(*scale)))
rotation = Gf.Matrix4d(1).SetRotate(Gf.Rotation(Gf.Vec3d(1,2,3),37))
matrices += [rotation, matrices[2] * rotation, rotation * matrices[2]]
shear = Gf.Matrix4d(1)
shear[0] = Gf.Vec4d(1,0.5,-0.2,0)
matrices += [shear, shear * matrices[3] * rotation * matrices[1]]
rows = []
for matrix in matrices:
    for epsilon in [1e-10, 1e-6]:
        ok, r, s, u, t, p = matrix.Factor(epsilon)
        rows.append({"matrix": [list(v) for v in matrix], "epsilon": epsilon,
                     "singular": not ok, "scale": list(s), "translation": list(t),
                     "orientation": [list(r[i])[:3] for i in range(3)],
                     "rotation": [list(u[i])[:3] for i in range(3)]})
result = {"version": ".".join(map(str,Usd.GetVersion()[1:])), "rows": rows}
path = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "affine.json"
if args.check:
    assert json.loads(path.read_text()) == result
else:
    path.write_text(json.dumps(result,indent=2,sort_keys=True)+"\n")
