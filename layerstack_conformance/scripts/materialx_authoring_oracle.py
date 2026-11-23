# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT

"""Verify the authored USD interface and connections with stock OpenUSD."""

import sys

from pxr import Sdf, Usd, UsdShade


def shader(stage, name, identifier):
    prim_path = f"/Paint/{name}"
    node = UsdShade.Shader.Get(stage, prim_path)
    assert node, prim_path
    assert node.GetImplementationSource() == UsdShade.Tokens.id, prim_path
    assert node.GetIdAttr().Get() == identifier, prim_path
    return node


def verify_edge(consumer, output, value_type, path):
    assert consumer and output, (path, consumer, output)
    output_path = output.GetAttr().GetPath()
    edge = (path, consumer.GetAttr().GetPath(), output_path)
    assert consumer.GetTypeName() == output.GetTypeName() == value_type, edge
    assert consumer.CanConnect(output), edge
    assert not consumer.GetAttr().HasAuthoredValueOpinion(), edge
    assert consumer.GetRawConnectedSourcePaths() == [output_path], edge
    source, name, kind = consumer.GetConnectedSource()
    assert source.GetPrim() == output.GetPrim(), edge
    assert (name, kind) == (output.GetBaseName(), UsdShade.AttributeType.Output), edge


def verify_file(node, filename, path):
    file = node.GetInput("file")
    assert file.GetTypeName() == Sdf.ValueTypeNames.Asset, path
    assert file.Get().path == filename, path


def verify(path, graph):
    stage = Usd.Stage.Open(path)
    assert stage, path
    material = UsdShade.Material.Get(stage, "/Paint")
    assert material, path
    albedo = shader(stage, "Albedo", "ND_image_color3")
    surface = shader(stage, "Surface", "ND_open_pbr_surface_surfaceshader")

    verify_file(albedo, "albedo.exr", path)
    weight = surface.GetInput("base_weight")
    assert weight.GetTypeName() == Sdf.ValueTypeNames.Float, path
    assert weight.Get() == 0.75, path

    types = Sdf.ValueTypeNames
    color = albedo.GetOutput("out")
    if graph == "tinted":
        tint = shader(stage, "Tint", "ND_multiply_color3")
        verify_edge(tint.GetInput("in1"), color, types.Color3f, path)
        tint_color = tint.GetInput("in2")
        assert tint_color.GetTypeName() == types.Color3f, path
        assert tuple(tint_color.Get()) == (0.25, 0.5, 1.0), path
        color = tint.GetOutput("out")
    verify_edge(surface.GetInput("base_color"), color, types.Color3f, path)
    verify_edge(material.GetSurfaceOutput("mtlx"), surface.GetOutput("surface"), types.Token, path)
    source, name, kind = material.ComputeSurfaceSource("mtlx")
    assert source.GetPrim() == surface.GetPrim(), path
    assert (name, kind) == ("surface", UsdShade.AttributeType.Output), path

    assert not albedo.GetInput("filtertype"), path
    if graph != "packed":
        assert not albedo.GetInput("texcoord"), path
        assert not surface.GetInput("specular_roughness"), path
        assert not surface.GetInput("geometry_normal"), path
        return

    uv = shader(stage, "Texcoord", "ND_texcoord_vector2")
    placement = shader(stage, "Placement", "ND_place2d_vector2")
    packed = shader(stage, "Packed", "ND_image_vector3")
    roughness = shader(stage, "Roughness", "ND_extract_vector3")
    metalness = shader(stage, "Metalness", "ND_extract_vector3")
    normal_image = shader(stage, "NormalImage", "ND_image_vector3")
    normal_map = shader(stage, "NormalMap", "ND_normalmap_float")
    verify_file(packed, "metallic_roughness.png", path)
    verify_file(normal_image, "normal.png", path)

    assert uv.GetInput("index").Get() == 0, path
    assert tuple(placement.GetInput("scale").Get()) == (2.0, 3.0), path
    verify_edge(placement.GetInput("texcoord"), uv.GetOutput("out"), types.Float2, path)
    for image in (albedo, packed, normal_image):
        verify_edge(image.GetInput("texcoord"), placement.GetOutput("out"), types.Float2, path)

    for extract, channel, name in (
        (roughness, 1, "specular_roughness"),
        (metalness, 2, "base_metalness"),
    ):
        index = extract.GetInput("index")
        assert index.GetTypeName() == types.Int and index.Get() == channel, path
        verify_edge(extract.GetInput("in"), packed.GetOutput("out"), types.Float3, path)
        verify_edge(surface.GetInput(name), extract.GetOutput("out"), types.Float, path)

    scale = normal_map.GetInput("scale")
    assert scale.GetTypeName() == types.Float and scale.Get() == 0.5, path
    verify_edge(normal_map.GetInput("in"), normal_image.GetOutput("out"), types.Float3, path)
    verify_edge(surface.GetInput("geometry_normal"), normal_map.GetOutput("out"), types.Float3, path)


graph, *files = sys.argv[1:]
assert graph in ("sparse", "tinted", "packed")
for file_path in files:
    verify(file_path, graph)
