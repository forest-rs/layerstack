# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT

"""Verify the authored USD interface and connections with stock OpenUSD."""

import sys

from pxr import Sdf, Usd, UsdShade


def verify(path):
    stage = Usd.Stage.Open(path)
    assert stage, path
    material = UsdShade.Material.Get(stage, "/Paint")
    albedo = UsdShade.Shader.Get(stage, "/Paint/Albedo")
    surface = UsdShade.Shader.Get(stage, "/Paint/Surface")
    assert material and albedo and surface, path

    assert albedo.GetImplementationSource() == UsdShade.Tokens.id, path
    assert surface.GetImplementationSource() == UsdShade.Tokens.id, path
    assert albedo.GetIdAttr().Get() == "ND_image_color3", path
    assert surface.GetIdAttr().Get() == "ND_open_pbr_surface_surfaceshader", path

    file = albedo.GetInput("file")
    assert file.GetTypeName() == Sdf.ValueTypeNames.Asset, path
    assert file.Get().path == "albedo.exr", path
    weight = surface.GetInput("base_weight")
    assert weight.GetTypeName() == Sdf.ValueTypeNames.Float, path
    assert weight.Get() == 0.75, path
    assert not albedo.GetInput("filtertype"), path
    assert not albedo.GetInput("texcoord"), path
    assert not surface.GetInput("specular_roughness"), path
    assert not surface.GetInput("geometry_normal"), path

    color = albedo.GetOutput("out")
    base_color = surface.GetInput("base_color")
    assert color.GetTypeName() == base_color.GetTypeName() == Sdf.ValueTypeNames.Color3f, path
    assert base_color.CanConnect(color), path
    assert not base_color.GetAttr().HasAuthoredValueOpinion(), path
    assert base_color.GetRawConnectedSourcePaths() == [color.GetAttr().GetPath()], path
    source, name, kind = base_color.GetConnectedSource()
    assert source.GetPrim() == albedo.GetPrim(), path
    assert (name, kind) == ("out", UsdShade.AttributeType.Output), path

    terminal = material.GetSurfaceOutput("mtlx")
    output = surface.GetOutput("surface")
    assert terminal.GetTypeName() == output.GetTypeName() == Sdf.ValueTypeNames.Token, path
    assert terminal.CanConnect(output), path
    assert terminal.GetRawConnectedSourcePaths() == [output.GetAttr().GetPath()], path
    source, name, kind = material.ComputeSurfaceSource("mtlx")
    assert source.GetPrim() == surface.GetPrim(), path
    assert (name, kind) == ("surface", UsdShade.AttributeType.Output), path


for file_path in sys.argv[1:]:
    verify(file_path)
