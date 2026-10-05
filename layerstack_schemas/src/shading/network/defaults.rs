// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Definition defaults come from generated OpenUSD usdShaders data, not USD
//! schema fallbacks. Synthesized inputs do not require interning new USD names.
use super::*;
use crate::shading::nodes::*;
fn input(name: &'static str, ty: &str, value: Value) -> (&'static str, PropertyType, Value) {
    let zero = match &value {
        Value::Float(_) => Value::Float(0.),
        Value::Vec2f(_) => Value::Vec2f([0.; 2]),
        Value::Vec3f(_) => Value::Vec3f([0.; 3]),
        Value::Vec4f(_) => Value::Vec4f([0.; 4]),
        Value::Int(_) => Value::Int(0),
        Value::String(_) => Value::String("".into()),
        Value::Asset(_) => Value::Asset("".into()),
        Value::Matrix4d(_) => Value::Matrix4d(alloc::boxed::Box::new([0.; 16])),
        _ => value.clone(),
    };
    (name, PropertyType::new(ty, false, zero), value)
}
pub(super) fn inputs(id: &str) -> Vec<(&'static str, PropertyType, Value)> {
    let f = |name, value| input(name, "float", Value::Float(value));
    let v2 = |name, value| input(name, "float2", Value::Vec2f(value));
    let v3 = |name, ty, value| input(name, ty, Value::Vec3f(value));
    let v4 = |name, value| input(name, "float4", Value::Vec4f(value));
    let token = |name, value: &str| input(name, "token", Value::String(value.into()));
    match id {
        PreviewSurface::ID => alloc::vec![
            v3(
                "diffuseColor",
                "color3f",
                PreviewSurface::diffuse_color_default()
            ),
            v3(
                "emissiveColor",
                "color3f",
                PreviewSurface::emissive_color_default()
            ),
            v3(
                "specularColor",
                "color3f",
                PreviewSurface::specular_color_default()
            ),
            v3("normal", "normal3f", PreviewSurface::normal_default()),
            f("roughness", PreviewSurface::roughness_default()),
            f("metallic", PreviewSurface::metallic_default()),
            f("opacity", PreviewSurface::opacity_default()),
            f(
                "opacityThreshold",
                PreviewSurface::opacity_threshold_default()
            ),
            f("ior", PreviewSurface::ior_default()),
            f("clearcoat", PreviewSurface::clearcoat_default()),
            f(
                "clearcoatRoughness",
                PreviewSurface::clearcoat_roughness_default()
            ),
            f("occlusion", PreviewSurface::occlusion_default()),
            f("displacement", PreviewSurface::displacement_default()),
            input(
                "useSpecularWorkflow",
                "int",
                Value::Int(PreviewSurface::use_specular_workflow_default())
            ),
            token("opacityMode", PreviewSurface::opacity_mode_default()),
        ],
        UvTexture::ID => alloc::vec![
            input("file", "asset", Value::Asset(UvTexture::file_default())),
            v2("st", UvTexture::st_default()),
            v4("fallback", UvTexture::fallback_default()),
            v4("scale", UvTexture::scale_default()),
            v4("bias", UvTexture::bias_default()),
            token("wrapS", UvTexture::wrap_s_default()),
            token("wrapT", UvTexture::wrap_t_default()),
            token("sourceColorSpace", UvTexture::source_color_space_default())
        ],
        Transform2d::ID => alloc::vec![
            v2("in", Transform2d::in_value_default()),
            f("rotation", Transform2d::rotation_default()),
            v2("scale", Transform2d::scale_default()),
            v2("translation", Transform2d::translation_default())
        ],
        _ => {
            let fallback = match id {
                PrimvarReaderFloat::ID => input(
                    "fallback",
                    "float",
                    Value::Float(PrimvarReaderFloat::fallback_default()),
                ),
                PrimvarReaderFloat2::ID => v2("fallback", PrimvarReaderFloat2::fallback_default()),
                PrimvarReaderFloat3::ID => v3(
                    "fallback",
                    "float3",
                    PrimvarReaderFloat3::fallback_default(),
                ),
                PrimvarReaderFloat4::ID => v4("fallback", PrimvarReaderFloat4::fallback_default()),
                PrimvarReaderInt::ID => input(
                    "fallback",
                    "int",
                    Value::Int(PrimvarReaderInt::fallback_default()),
                ),
                PrimvarReaderString::ID => input(
                    "fallback",
                    "string",
                    Value::String(PrimvarReaderString::fallback_default()),
                ),
                PrimvarReaderPoint::ID => v3(
                    "fallback",
                    "point3f",
                    PrimvarReaderPoint::fallback_default(),
                ),
                PrimvarReaderNormal::ID => v3(
                    "fallback",
                    "normal3f",
                    PrimvarReaderNormal::fallback_default(),
                ),
                PrimvarReaderVector::ID => v3(
                    "fallback",
                    "vector3f",
                    PrimvarReaderVector::fallback_default(),
                ),
                PrimvarReaderMatrix::ID => input(
                    "fallback",
                    "matrix4d",
                    Value::Matrix4d(alloc::boxed::Box::new(core::array::from_fn(|i| {
                        PrimvarReaderMatrix::fallback_default()[i / 4][i % 4]
                    }))),
                ),
                _ => return Vec::new(),
            };
            alloc::vec![
                fallback,
                input(
                    "varname",
                    "string",
                    Value::String(PrimvarReaderFloat::varname_default())
                )
            ]
        }
    }
}
