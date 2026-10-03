# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Compare built-in light/filter/container CanConnect and provider behavior."""
import argparse, json, pathlib
from pxr import Usd, UsdLux, UsdShade, Sdf
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check",action="store_true")
args=parser.parse_args()
s=Usd.Stage.CreateInMemory()
for cls,path in [(UsdLux.SphereLight,"/Light"),(UsdLux.LightFilter,"/Filter"),
                 (UsdShade.Material,"/Material"),(UsdShade.NodeGraph,"/Graph"),
                 (UsdShade.Shader,"/Shader"),(UsdShade.Shader,"/Filter/Child"),
                 (UsdShade.Shader,"/Graph/Child"),(UsdShade.Shader,"/Material/Child")]:
    prim=cls.Define(s,path).GetPrim()
    c=UsdShade.ConnectableAPI(prim)
    c.CreateInput("a",Sdf.ValueTypeNames.Float).Set(2.0)
    c.CreateInput("interface",Sdf.ValueTypeNames.Float).SetConnectability(UsdShade.Tokens.interfaceOnly)
    c.CreateOutput("out",Sdf.ValueTypeNames.Float)
for cls,path,api in [(UsdShade.Shader,"/ShaderLight",UsdLux.LightAPI),
                     (UsdLux.LightFilter,"/FilterLight",UsdLux.LightAPI),
                     (UsdShade.NodeGraph,"/GraphLight",UsdLux.MeshLightAPI)]:
    prim=cls.Define(s,path).GetPrim()
    api.Apply(prim)
    c=UsdShade.ConnectableAPI(prim)
    c.CreateInput("a",Sdf.ValueTypeNames.Float).Set(2.0)
    c.CreateOutput("out",Sdf.ValueTypeNames.Float)
ports=[]
for prim in s.Traverse():
    c=UsdShade.ConnectableAPI(prim)
    ports.extend([p.GetAttr() for p in c.GetInputs(True)+c.GetOutputs(True)])
rows=[]
for dest in ports:
    for source in ports:
        view=UsdShade.Input(dest) if dest.GetName().startswith("inputs:") else UsdShade.Output(dest)
        rows.append({"destination":str(dest.GetPath()),"source":str(source.GetPath()),"allowed":view.CanConnect(source)})
UsdShade.Input(s.GetAttributeAtPath("/Light.inputs:a")).GetAttr().SetConnections([Sdf.Path("/Light.inputs:intensity")])
result={"version":".".join(map(str,Usd.GetVersion()[1:])),"rows":rows,
        "providers":[str(a.GetPath()) for a in UsdShade.Utils.GetValueProducingAttributes(UsdShade.Input(s.GetAttributeAtPath("/Light.inputs:a")),False)]}
root=pathlib.Path(__file__).resolve().parents[1]/"fixtures"
scene=root/"lux_connections.usda"
output=root/"lux_connections.json"
if args.check:
    assert json.loads(output.read_text())==result
    assert scene.read_text()==s.GetRootLayer().ExportToString()
else:
    scene.write_text(s.GetRootLayer().ExportToString())
    output.write_text(json.dumps(result,indent=2,sort_keys=True)+"\n")
