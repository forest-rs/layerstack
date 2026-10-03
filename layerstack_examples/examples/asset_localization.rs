// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Inspect dependencies, localize copied layers, then optionally save a USDZ.
//! Run with an output path to write the package; no argument only inspects it.

use layerstack::asset_dependencies::{DependencyTarget, collect_dependencies};
use layerstack::{
    InMemoryStore, Layer, LayerId, LayerStore, PrimSpec, PropertySpec, PropertyType, Reference,
    Value,
};
use layerstack_usdz::localize::{LocalizationTarget, localize_asset};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let mut store = InMemoryStore::default();
    let root = store.path("/");
    let model = store.path("/Model");
    let model_name = store.tokens.intern("Model");
    let image = store.tokens.intern("image");
    let mut scene = Layer::new(LayerId(1));
    scene.default_prim = Some(model_name);
    scene.insert_prim(root, PrimSpec::default().with_children(vec![model_name]));
    scene.insert_prim(
        model,
        PrimSpec::def().with_reference(Reference::with_asset_default_prim(
            LayerId(2),
            "source/model.usda",
        )),
    );
    let mut asset = Layer::new(LayerId(2));
    asset.default_prim = Some(model_name);
    asset.insert_prim(root, PrimSpec::default().with_children(vec![model_name]));
    asset.insert_prim(
        model,
        PrimSpec::def().with_property(
            image,
            PropertySpec::typed_attribute(PropertyType::new(
                "asset",
                false,
                Value::Asset("".into()),
            ))
            .with_default(Value::Asset("source/albedo.png".into())),
        ),
    );
    store.insert_layer(scene);
    store.insert_layer(asset);
    let inventory = collect_dependencies(&store, LayerId(1), |dependency| {
        match dependency.layer_hint {
            Some(layer) => vec![DependencyTarget::Layer(layer)],
            None => vec![DependencyTarget::Asset("source/albedo.png".into())],
        }
    });
    println!(
        "Inventory: {} layers, {} assets, complete={}",
        inventory.layers.len(),
        inventory.assets().len(),
        inventory.is_complete()
    );
    // A real host obtains image bytes from its resolver. This sample retains
    // a placeholder to demonstrate byte ownership; it is not a decodable PNG.
    let plan = localize_asset(&store, LayerId(1), "scene.usdc", |dependency| {
        Ok(match dependency.layer_hint {
            Some(source) => LocalizationTarget::layer(source, "models/model.usda"),
            None => LocalizationTarget::asset("textures/albedo.png", &b"placeholder image"[..]),
        })
    })?;
    let package = plan.write_usdz(store.tokens(), store.paths())?;
    println!(
        "Localization: {} layers, {} assets, {} bytes",
        plan.layers.len(),
        plan.assets.len(),
        package.len()
    );
    if let Some(path) = std::env::args_os().nth(1) {
        std::fs::write(path, package)?;
    }
    Ok(())
}
