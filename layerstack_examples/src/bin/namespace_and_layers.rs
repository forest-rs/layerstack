// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Coordinate namespace edits across supplied stages, then consolidate local layers.
//! OpenUSD `UsdNamespaceEditor::AddDependentStage` and `UsdUtilsFlattenLayerStack`:
//! <https://openusd.org/release/user_guides/namespace_editing.html>
//! <https://openusd.org/release/api/flatten_layer_stack_8h.html>
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, NamespaceEdit, PrimSpec, PropertyPath,
    PropertySpec, PropertyType, Reference, Stage, StageOptions, SublayerEntry, TargetPath, Value,
    flatten_layer_stack,
};

fn size(value: f64) -> PropertySpec {
    PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.)))
        .with_default(Value::Double(value))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    let asset = store.path("/Asset");
    let asset_part = store.path("/Asset/Part");
    let model = store.path("/Model");
    let part = store.path("/Model/Part");
    let renamed = store.path("/Model/Renamed");
    let used = store.path("/Use");
    let used_part = store.path("/Use/Part");
    let used_renamed = store.path("/Use/Renamed");
    let size_name = store.tokens.intern("size");
    let mut geometry = Layer::new(LayerId(5));
    geometry.insert_prim(asset, PrimSpec::def());
    geometry.insert_prim(
        asset_part,
        PrimSpec::def().with_property(size_name, size(1.)),
    );
    store.insert_layer(geometry);
    let mut model_layer = Layer::new(LayerId(1));
    model_layer.sublayers.push(SublayerEntry::new(LayerId(2)));
    model_layer.insert_prim(
        model,
        PrimSpec::def().with_reference(Reference::with_asset(LayerId(5), asset, "geometry.usda")),
    );
    store.insert_layer(model_layer);
    let mut overrides = Layer::new(LayerId(2));
    overrides.insert_prim(model, PrimSpec::over());
    overrides.insert_prim(part, PrimSpec::over().with_property(size_name, size(2.)));
    store.insert_layer(overrides);
    let mut assembly = Layer::new(LayerId(3));
    assembly.insert_prim(
        used,
        PrimSpec::def().with_reference(Reference::with_asset(LayerId(1), model, "model.usda")),
    );
    assembly.insert_prim(
        used_part,
        PrimSpec::over().with_property(size_name, size(3.)),
    );
    store.insert_layer(assembly);
    let original = store.layers.clone();
    let mut primary = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let mut dependent = LiveStage::compose(&mut store, LayerId(3), StageOptions::default());
    let edit = NamespaceEdit::prepare_with_dependents(
        primary.stage(),
        &[dependent.stage()],
        &mut store,
        &EditTarget::for_layer(LayerId(1)),
        TargetPath::Prim(part),
        TargetPath::Prim(renamed),
    )?;
    assert_eq!(
        store.layers, original,
        "preparation only produces a guarded transaction"
    );
    println!("layers to edit: {:?}", edit.layers_to_edit());
    let inverse = primary.apply(&mut store, edit.transaction())?.inverse;
    assert!(
        dependent.stage().has_prim(used_part),
        "dependent snapshot refresh is explicit"
    );
    dependent.synchronize(&mut store);
    assert!(
        primary.stage().has_prim(renamed),
        "renamed geometry appears in the primary stage"
    );
    assert!(
        !primary.stage().has_prim(part),
        "the original primary path is removed"
    );
    assert!(
        dependent.stage().has_prim(used_renamed),
        "dependent geometry moves with its source"
    );
    assert!(
        !dependent.stage().has_prim(used_part),
        "the original dependent path is removed"
    );
    assert_eq!(
        store.layers[&LayerId(5)],
        original[&LayerId(5)],
        "source geometry stays intact"
    );
    assert_eq!(
        dependent
            .stage()
            .resolve_field_path(PropertyPath::new(used_renamed, size_name))
            .unwrap()
            .value,
        Value::Double(3.),
        "dependent override survives the rename"
    );

    // Reduce only the local stack; preserve the external reference and relocation.
    // The callback anchors each asset at the intended output destination.
    let output = LayerId(4);
    let consolidated = flatten_layer_stack(&store, LayerId(1).into(), output, &mut |_, asset| {
        Some(format!("./assets/{asset}"))
    })?;
    assert_eq!(
        consolidated.report.sources.len(),
        2,
        "only the root and local override layer are consolidated"
    );
    for asset in &consolidated.report.asset_paths {
        if let Some(layer) = asset.resolved_layer {
            store.insert_asset_layer(output, &asset.output_path, layer);
        }
    }
    assert!(
        !consolidated.layer.prims[&model].references.is_empty(),
        "external geometry remains referenced"
    );
    assert_eq!(
        consolidated.layer.relocates.len(),
        1,
        "the namespace relocation remains authored"
    );
    store.insert_layer(consolidated.layer);
    let consolidated_stage = Stage::compose(&mut store, output, StageOptions::default());
    assert_eq!(
        consolidated_stage
            .resolve_field_path(PropertyPath::new(renamed, size_name))
            .unwrap()
            .value,
        Value::Double(2.),
        "local override survives consolidation"
    );
    println!(
        "consolidated {} layers; retained external geometry and namespace relocation",
        consolidated.report.sources.len()
    );

    primary.apply(&mut store, &inverse)?;
    dependent.synchronize(&mut store);
    for (id, layer) in original {
        assert_eq!(
            store.layers[&id], layer,
            "undo restores each edited source layer"
        );
    }
    assert!(
        dependent.stage().has_prim(used_part),
        "undo restores the dependent path"
    );
    Ok(())
}
