# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record actual UsdGeomPointBased batch point motion; run with OpenUSD 26.8."""
import argparse
import json
import pathlib
from pxr import Usd, UsdGeom
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--check', action='store_true')
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / 'fixtures' / 'point_motion'
stage = Usd.Stage.Open(str(here / 'scene.usda'))
rows = []
for prim in stage.Traverse():
    for base, times in [(None,[None]), (0.,[-1.,1.,2.,4.,1.]),(2.,[3.,1.,4.,2.]),(10.,[9.,12.,14.])]:
        query = UsdGeom.PointBased(prim)
        outputs = query.ComputePointsAtTimes([Usd.TimeCode.Default() if t is None else Usd.TimeCode(t) for t in times], Usd.TimeCode.Default() if base is None else Usd.TimeCode(base))
        rows.append({'path':str(prim.GetPath()),'base':base,'times':times,'points':None if outputs is None or len(outputs) != len(times) else [[[float(v) for v in p] for p in arr] for arr in outputs]})
result = {'version':'.'.join(map(str,Usd.GetVersion()[1:])),'rows':rows}
path = here / 'oracle.json'
if args.check:
    assert json.loads(path.read_text()) == result
else:
    path.write_text(json.dumps(result,indent=2,sort_keys=True)+'\n')
