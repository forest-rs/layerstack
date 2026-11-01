# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record what OpenUSD answers about the materials of a draw list.

Usage: stagehand_materials_oracle.py [--check]

Writes `fixtures/stagehand_materials/`: `scene.usda`, the texture files it
names (each file's content is its own name; only their existence matters)
and `oracle.json`. For each material of the scene, under the universal
render context and under `["mtlx"]`:

- `UsdShadeMaterial::ComputeSurfaceSource`: the shader, its output and
  the source type;
- for a `UsdPreviewSurface`, each of its inputs'
  `UsdShadeInput::GetValueProducingAttributes`, with the value of an
  input that produces one (`Get` at the default time and at time 10) or
  the shader `info:id`, output and declared type of a shader output, and
  each input's declared type. A connection's two ends need not have the
  same type: OpenUSD neither refuses nor converts it (`Channels`: a float
  reads a texture's `rgb` through a referenced node graph's `float3`
  output, a color reads `r`), and `UsdShadeConnectableAPI::CanConnect`
  is recorded for each connected input to show it;
- for each `UsdUVTexture`, `UsdPrimvarReader_float2` and `UsdTransform2d`
  reached, the same for each of their inputs, and for a texture's file its
  authored path, `Sdf.ComputeAssetPathRelativeToLayer` against the layer
  authoring it, the resolved path, and for a UDIM path
  `UsdShade.UdimUtils.ResolveUdimTilePaths`;
- the shader registry's defaults (`Sdr`) for the four shaders.

It also records each light's connected inputs: their value-producing
attributes and values. Paths under the fixture directory are written
relative to it. `--check` rewrites everything into a temporary directory
and fails unless it matches the committed files. `layerstack_stagehand`'s
`materials_match_openusd` test compares its `MaterialItem`s with it, as does
the TypeScript draw list's test.
"""
import argparse
import filecmp
import json
import os
import shutil
import tempfile

from pxr import Sdf, Sdr, Usd, UsdLux, UsdShade

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "stagehand_materials"))

SCENE = '''#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{
    def Mesh "Plain" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        rel material:binding = </Looks/Plain>
    }

    def Mesh "Textured" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        texCoord2f[] primvars:uv = [(0, 0), (1, 0), (0, 1)] (
            interpolation = "vertex"
        )
        rel material:binding = </Looks/Textured>
    }

    def Sphere "Graph" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        rel material:binding = </Looks/Graph>
    }

    def Sphere "Contexts" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        double radius = 2
        rel material:binding = </Looks/Contexts>
    }

    def Cube "Udim" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        rel material:binding = </Looks/Udim>
    }

    def Cube "Broken" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        double size = 3
        rel material:binding = </Looks/Broken>
    }

    def Cube "Animated" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        double size = 4
        rel material:binding = </Looks/Animated>
    }

    def Cube "Painted" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        double size = 5
        rel material:binding = </Looks/Painted>
    }

    def Cube "Bare" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        double size = 6
        rel material:binding = </Looks/Bare>
    }

    def Cube "Channels" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        double size = 7
        rel material:binding = </Looks/Channels>
    }
}

def Scope "Looks"
{
    def Material "Plain"
    {
        token outputs:surface.connect = </Looks/Plain/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.8, 0.2, 0.1)
            color3f inputs:emissiveColor = (0.1, 0.1, 0)
            float inputs:metallic = 1
            float inputs:roughness = 0.25
            float inputs:clearcoat = 0.5
            float inputs:clearcoatRoughness = 0.1
            float inputs:opacity = 0.5
            token inputs:opacityMode = "presence"
            float inputs:ior = 1.33
            float inputs:occlusion = 0.75
            float inputs:displacement = 0.125
            token outputs:surface
        }
    }

    def Material "Textured"
    {
        string inputs:coordinates = "uv"
        token outputs:surface.connect = </Looks/Textured/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Textured/Albedo.outputs:rgb>
            float inputs:opacity.connect = </Looks/Textured/Albedo.outputs:a>
            float inputs:opacityThreshold = 0.5
            float inputs:roughness.connect = </Looks/Textured/Packed.outputs:g>
            float inputs:metallic.connect = </Looks/Textured/Packed.outputs:b>
            float inputs:occlusion.connect = </Looks/Textured/Packed.outputs:r>
            normal3f inputs:normal.connect = </Looks/Textured/Normal.outputs:rgb>
            int inputs:useSpecularWorkflow = 1
            color3f inputs:specularColor = (0.04, 0.04, 0.04)
            token outputs:surface
        }

        def Shader "Reader"
        {
            uniform token info:id = "UsdPrimvarReader_float2"
            string inputs:varname.connect = </Looks/Textured.inputs:coordinates>
            float2 inputs:fallback = (0.5, 0.25)
            float2 outputs:result
        }

        def Shader "Place"
        {
            uniform token info:id = "UsdTransform2d"
            float2 inputs:in.connect = </Looks/Textured/Reader.outputs:result>
            float inputs:rotation = 30
            float2 inputs:scale = (2, 3)
            float2 inputs:translation = (0.5, -0.25)
            float2 outputs:result
        }

        def Shader "Albedo"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./textures/grid.png@
            float2 inputs:st.connect = </Looks/Textured/Place.outputs:result>
            token inputs:wrapS = "repeat"
            token inputs:wrapT = "mirror"
            token inputs:sourceColorSpace = "sRGB"
            float4 inputs:fallback = (1, 0, 1, 1)
            float3 outputs:rgb
            float outputs:a
        }

        def Shader "Packed"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @textures/packed.png@
            float2 inputs:st.connect = </Looks/Textured/Reader.outputs:result>
            token inputs:wrapS = "clamp"
            token inputs:sourceColorSpace = "raw"
            float outputs:r
            float outputs:g
            float outputs:b
        }

        def Shader "Normal"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./textures/../textures/normal.png@
            float2 inputs:st.connect = </Looks/Textured/Reader.outputs:result>
            float4 inputs:scale = (2, 2, 2, 1)
            float4 inputs:bias = (-1, -1, -1, 0)
            token inputs:sourceColorSpace = "raw"
            token inputs:wrapS = "black"
            float3 outputs:rgb
        }
    }

    def Material "Graph"
    {
        color3f inputs:tint = (0.2, 0.4, 0.6)
        float inputs:shine = 0.3
        token outputs:surface.connect = </Looks/Graph/Group.outputs:surface>

        def NodeGraph "Group"
        {
            color3f inputs:tint.connect = </Looks/Graph.inputs:tint>
            float inputs:shine.connect = </Looks/Graph.inputs:shine>
            token outputs:surface.connect = </Looks/Graph/Group/Surface.outputs:surface>

            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </Looks/Graph/Group.inputs:tint>
                float inputs:roughness.connect = </Looks/Graph/Group.inputs:shine>
                color3f inputs:emissiveColor = (1, 1, 1)
                color3f inputs:emissiveColor.connect = </Looks/Graph/Group.inputs:glow>
                token outputs:surface
            }
        }
    }

    def Material "Contexts"
    {
        token outputs:surface.connect = </Looks/Contexts/Preview.outputs:surface>
        token outputs:mtlx:surface.connect = </Looks/Contexts/Standard.outputs:out>

        def Shader "Preview"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0, 1, 0)
            token outputs:surface
        }

        def Shader "Standard"
        {
            uniform token info:id = "ND_standard_surface_surfaceshader"
            token outputs:out
        }
    }

    def Material "Udim"
    {
        token outputs:surface.connect = </Looks/Udim/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Udim/Tiles.outputs:rgb>
            token outputs:surface
        }

        def Shader "Tiles"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./textures/rock.<UDIM>.png@
            float3 outputs:rgb
        }
    }

    def Material "Broken"
    {
        float inputs:loop.connect = </Looks/Broken/Ring.inputs:loop>
        token outputs:surface.connect = </Looks/Broken/Surface.outputs:surface>

        def NodeGraph "Ring"
        {
            float inputs:loop.connect = </Looks/Broken.inputs:loop>
        }

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Broken/Gone.outputs:rgb>
            float inputs:roughness.connect = </Looks/Broken/Noise.outputs:out>
            float inputs:metallic.connect = </Looks/Broken.inputs:loop>
            float inputs:clearcoat.connect = [</Looks/Broken/A.outputs:r>, </Looks/Broken/B.outputs:r>]
            color3f inputs:emissiveColor.connect = </Looks/Broken/Empty.outputs:rgb>
            token outputs:surface
        }

        def Shader "Noise"
        {
            uniform token info:id = "MyNoise"
            float outputs:out
        }

        def Shader "A"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./textures/grid.png@
            float outputs:r
        }

        def Shader "B"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./textures/packed.png@
            float outputs:r
        }

        def Shader "Empty"
        {
            uniform token info:id = "UsdUVTexture"
            float3 outputs:rgb
        }
    }

    def Material "Animated"
    {
        token outputs:surface.connect = </Looks/Animated/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            float inputs:roughness.timeSamples = {
                0: 0.1,
                20: 0.9,
            }
            token outputs:surface
        }
    }

    def Material "Painted"
    {
        token outputs:surface.connect = </Looks/Painted/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Painted/Color.outputs:result>
            token outputs:surface
        }

        def Shader "Color"
        {
            uniform token info:id = "UsdPrimvarReader_float3"
            string inputs:varname = "displayColor"
            float3 inputs:fallback = (0.5, 0.5, 0.5)
            float3 outputs:result
        }
    }

    def Material "Bare"
    {
    }

    def Material "Channels"
    {
        token outputs:surface.connect = </Looks/Channels/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Channels/Mask.outputs:r>
            float inputs:metallic.connect = </Looks/Channels/Metal.outputs:rgb>
            float inputs:roughness.connect = </Looks/Channels/Rough.outputs:rgb>
            float inputs:occlusion.connect = </Looks/Channels/Mask.outputs:rgba>
            float inputs:opacity.connect = </Looks/Channels/Mask.outputs:a>
            normal3f inputs:normal.connect = </Looks/Channels/Metal.outputs:rgb>
            token outputs:surface
        }

        def NodeGraph "Metal" (
            prepend references = </Looks/MetalGraph>
        )
        {
        }

        def Shader "Rough"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./textures/packed.png@
            float4 inputs:scale = (0.5, 1, 1, 1)
            float3 outputs:rgb
        }

        def Shader "Mask"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./textures/grid.png@
            float outputs:r
            float outputs:a
            float4 outputs:rgba
        }
    }

    def NodeGraph "MetalGraph"
    {
        token inputs:frame:st = "st"
        float3 outputs:rgb.connect = </Looks/MetalGraph/Texture.outputs:rgb>

        def Shader "Texture"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./textures/packed.png@
            float2 inputs:st.connect = </Looks/MetalGraph/Reader.outputs:result>
            float3 outputs:rgb
        }

        def Shader "Reader"
        {
            uniform token info:id = "UsdPrimvarReader_float2"
            token inputs:varname.connect = </Looks/MetalGraph.inputs:frame:st>
            float2 outputs:result
        }
    }
}

def Scope "Lights"
{
    def NodeGraph "Rig"
    {
        float inputs:power = 7
        color3f inputs:tint = (1, 0.5, 0.25)
        float inputs:spread.timeSamples = {
            0: 1,
            10: 2,
        }
    }

    def SphereLight "Lamp"
    {
        float inputs:intensity.connect = </Lights/Rig.inputs:power>
        color3f inputs:color.connect = </Lights/Rig.inputs:tint>
        float inputs:radius.connect = </Lights/Rig.inputs:spread>
        float inputs:exposure = 2
        float inputs:exposure.connect = </Lights/Rig.inputs:missing>
    }
}
'''

TEXTURES = [
    "textures/grid.png",
    "textures/packed.png",
    "textures/normal.png",
    "textures/rock.1001.png",
    "textures/rock.1002.png",
    "textures/rock.1011.png",
    # Outside the 1001-1100 range: never a tile.
    "textures/rock.1101.png",
]

SHADERS = ["UsdPreviewSurface", "UsdUVTexture", "UsdTransform2d", "UsdPrimvarReader_float2",
           "UsdPrimvarReader_float3"]


def write_files(out):
    os.makedirs(os.path.join(out, "textures"), exist_ok=True)
    with open(os.path.join(out, "scene.usda"), "w") as f:
        f.write(SCENE)
    for name in TEXTURES:
        with open(os.path.join(out, name), "w") as f:
            f.write(name + "\n")


def plain(value, root):
    """A JSON value: vectors as lists, assets as their authored path."""
    if value is None:
        return None
    if isinstance(value, Sdf.AssetPath):
        return value.path
    if isinstance(value, (bool, int, float, str)):
        return value
    return [plain(v, root) for v in value]


def relative(path, root):
    """A path under `root` written relative to it; others as they are."""
    if not path:
        return path
    full = os.path.realpath(path)
    base = os.path.realpath(root)
    if full.startswith(base + os.sep):
        return os.path.relpath(full, base).replace(os.sep, "/")
    return path


def producing(attribute, root):
    """`GetValueProducingAttributes` of an input, each with what it gives."""
    rows = []
    for source in UsdShade.Input(attribute).GetValueProducingAttributes():
        row = {"attribute": str(source.GetPath())}
        if UsdShade.Utils.GetType(source.GetName()) == UsdShade.AttributeType.Output:
            shader = UsdShade.Shader(source.GetPrim())
            row["shader"] = shader.GetIdAttr().Get() if shader else None
            row["output"] = UsdShade.Utils.GetBaseNameAndType(source.GetName())[0]
            row["type"] = str(source.GetTypeName())
        else:
            row["value"] = plain(source.Get(), root)
            row["value10"] = plain(source.Get(10), root)
        rows.append(row)
    return rows


def shader_record(prim, root):
    shader = UsdShade.Shader(prim)
    inputs = {}
    types = {}
    can_connect = {}
    for input in shader.GetInputs():
        inputs[input.GetBaseName()] = producing(input.GetAttr(), root)
        types[input.GetBaseName()] = str(input.GetTypeName())
        for source in input.GetConnectedSources()[0]:
            output = source.source.GetOutput(source.sourceName)
            if output:
                can_connect[input.GetBaseName()] = UsdShade.ConnectableAPI.CanConnect(input, output)
    record = {"id": shader.GetIdAttr().Get(), "inputs": inputs, "types": types,
              "canConnect": can_connect}
    file = shader.GetInput("file")
    if file and file.GetAttr().HasAuthoredValue():
        attribute = file.GetAttr()
        authored = attribute.Get().path
        layer = attribute.GetPropertyStack()[0].layer
        record["file"] = {
            "authored": authored,
            "anchored": relative(Sdf.ComputeAssetPathRelativeToLayer(layer, authored), root),
            "resolved": relative(attribute.Get().resolvedPath, root),
            "udim": UsdShade.UdimUtils.IsUdimIdentifier(authored),
            "tiles": [
                [int(tile), relative(path, root)]
                for path, tile in UsdShade.UdimUtils.ResolveUdimTilePaths(authored, layer)
            ],
        }
    return record


def upstream(prim, seen, out, root):
    """Every shader whose output a `UsdPreviewSurface` input reaches."""
    for input in UsdShade.Shader(prim).GetInputs():
        for source in input.GetValueProducingAttributes():
            if UsdShade.Utils.GetType(source.GetName()) != UsdShade.AttributeType.Output:
                continue
            path = str(source.GetPrim().GetPath())
            if path in seen:
                continue
            seen.add(path)
            out[path] = shader_record(source.GetPrim(), root)
            upstream(source.GetPrim(), seen, out, root)


def record(root):
    stage = Usd.Stage.Open(os.path.join(root, "scene.usda"))
    materials = []
    for prim in stage.Traverse():
        if not prim.IsA(UsdShade.Material):
            continue
        material = UsdShade.Material(prim)
        for contexts in [[], ["mtlx"]]:
            shader, output, kind = material.ComputeSurfaceSource(contexts[0] if contexts else "")
            row = {
                "path": str(prim.GetPath()),
                "contexts": contexts,
                "shader": str(shader.GetPath()) if shader else None,
                "output": output,
                # With no source, OpenUSD leaves the type uninitialized, so it
                # prints differently from run to run and machine to machine.
                "sourceType": str(kind) if shader else None,
                "id": shader.GetIdAttr().Get() if shader else None,
            }
            if shader and shader.GetIdAttr().Get() == "UsdPreviewSurface":
                row["surface"] = shader_record(shader.GetPrim(), root)
                nodes = {}
                upstream(shader.GetPrim(), set(), nodes, root)
                row["nodes"] = nodes
            materials.append(row)
    lights = []
    for prim in stage.Traverse():
        if not prim.HasAPI(UsdLux.LightAPI):
            continue
        light = UsdLux.LightAPI(prim)
        inputs = {}
        for input in light.GetInputs():
            if input.HasConnectedSource() or input.GetAttr().HasAuthoredConnections():
                inputs[input.GetBaseName()] = producing(input.GetAttr(), root)
        lights.append({"path": str(prim.GetPath()), "inputs": inputs})
    registry = Sdr.Registry()
    defaults = {}
    for identifier in SHADERS:
        node = registry.GetShaderNodeByIdentifier(identifier)
        defaults[identifier] = {
            name: plain(node.GetShaderInput(name).GetDefaultValue(), root)
            for name in node.GetShaderInputNames()
        }
    return {
        "version": ".".join(map(str, Usd.GetVersion()[1:])),
        "materials": materials,
        "lights": lights,
        "defaults": defaults,
    }


def write(out):
    write_files(out)
    with open(os.path.join(out, "oracle.json"), "w") as f:
        f.write(json.dumps(record(out), indent=1, sort_keys=True) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if not args.check:
        shutil.rmtree(FIXTURES, ignore_errors=True)
        write(FIXTURES)
        return
    with tempfile.TemporaryDirectory() as out:
        write(out)
        compare = filecmp.dircmp(out, FIXTURES)
        problems = []

        def walk(cmp, prefix):
            problems.extend(prefix + n for n in cmp.left_only + cmp.right_only + cmp.diff_files)
            for name, sub in cmp.subdirs.items():
                walk(sub, prefix + name + "/")

        walk(compare, "")
        # `dircmp` compares shallowly; compare every file's bytes.
        for folder, _, files in os.walk(out):
            for name in files:
                left = os.path.join(folder, name)
                right = os.path.join(FIXTURES, os.path.relpath(left, out))
                if os.path.exists(right) and not filecmp.cmp(left, right, shallow=False):
                    problems.append(os.path.relpath(left, out))
        assert not problems, f"stagehand materials oracle differs from OpenUSD: {sorted(set(problems))}"


if __name__ == "__main__":
    main()
