// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Creates original wide/deep/animated/packaged workloads for `stage_workflows`.
//! Usage: `workflow_fixtures output_directory [array_elements] [wide_prims]`.
//! Defaults: 1,000,000 array elements and 256 prims. Other shapes keep 256 prims.
use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Value};
use layerstack_usdz::{PackageFile, write_usdz};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(args.next().ok_or("supply an output directory")?);
    let elements: usize = args.next().map_or(Ok(1_000_000), |arg| arg.parse())?;
    let wide_prims: usize = args.next().map_or(Ok(256), |arg| arg.parse())?;
    std::fs::create_dir_all(&directory)?;
    for shape in ["wide", "deep", "animated"] {
        let store = InMemoryStore::default();
        let mut layer = Layer::new(LayerId(1));
        let root = store.path("/");
        layer.insert_prim(root, PrimSpec::default());
        let mut parent = root;
        let name = store.tokens().intern("samples");
        let mut path = String::new();
        let prims = if shape == "wide" { wide_prims } else { 256 };
        for index in 0..prims {
            if shape == "deep" {
                path.push_str("/P");
            } else {
                path = format!("/P{index}");
            }
            let id = store.path(&path);
            let child = store
                .tokens()
                .intern(path.rsplit('/').next().expect("prim name"));
            layer
                .prim_mut(parent)
                .expect("parent spec")
                .authored_children
                .push(child);
            let mut prim = PrimSpec::def();
            if index < 4 || (shape == "deep" && index == 255) {
                let values = Value::array_from_iter(
                    (0..elements).map(|i| {
                        Value::Float(f32::from(
                            u16::try_from(i % 251 + index * 251).expect("bounded fixture value"),
                        ))
                    }),
                    Some(&Value::Float(0.)),
                );
                let mut property = PropertySpec::typed_attribute(
                    PropertyType::parse("float[]").expect("a registered value type"),
                );
                if shape == "animated" {
                    property.time_samples =
                        vec![(0., values.clone()), (10., values.clone())].into();
                } else {
                    property.default = Some(values.clone());
                }
                prim.set_property(name, property);
            }
            layer.insert_prim(id, prim);
            if shape == "deep" {
                parent = id;
            }
        }
        let bytes = layerstack_usdc::writer::save_layer(&layer, store.tokens(), store.paths())?;
        std::fs::write(directory.join(format!("{shape}.usdc")), &bytes)?;
        if shape == "wide" {
            let inner = write_usdz(&[
                PackageFile::new("wide.usdc", &bytes),
                PackageFile::new("data.png", b"opaque resource"),
            ])?;
            let outer = write_usdz(&[
                PackageFile::new("root.usda", b"#usda 1.0\n(subLayers=[@inner.usdz@])"),
                PackageFile::new("inner.usdz", &inner),
            ])?;
            std::fs::write(directory.join("nested.usdz"), outer)?;
        }
    }
    std::fs::write(directory.join("malformed.usdc"), b"PXR-USDC\0truncated")?;
    Ok(())
}
