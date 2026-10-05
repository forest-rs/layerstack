// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Read a codeless API schema library at runtime and apply it by name.
//! The host supplies `SchemaDeclaration` entries from the library's plugInfo.json;
//! this example spells them explicitly, without global plugin discovery.
//! AOUSD Core §13.3; OpenUSD `UsdPrim::ApplyAPI`.
//! <https://openusd.org/release/tut_generating_new_schema.html>
use layerstack::{
    AssetResolveError, AssetResolver, EditTarget, InMemoryStore, Layer, LayerId, LiveStage,
    PathInterner, PrimSpec, ResolvedAsset, SchemaKind, SchemaRegistry, StageOptions, TokenInterner,
    Value,
    schema::{SchemaDeclaration, read_generated_schema},
};
use layerstack_schemas::{Domain, SchemaEdit};
use std::sync::Arc;
struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}
fn scene() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let source = r#"#usda 1.0
class "PipelineTintAPI" { color3f pipeline:tint = (0.25, 0.5, 1) }
class "PipelineSlotAPI" { float pipeline:__INSTANCE_NAME__:gain = 2 }
"#;
    let library = layerstack_usda::read_usda(
        source,
        LayerId(9),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .emitted
    .layer;
    let tint = store.tokens.intern("PipelineTintAPI");
    let slot = store.tokens.intern("PipelineSlotAPI");
    let cube = store.tokens.intern("Cube");
    let mut tint_declaration = SchemaDeclaration::new(tint, SchemaKind::SingleApplyApi);
    tint_declaration.can_only_apply_to.push(cube);
    let declared = [
        tint_declaration,
        SchemaDeclaration::new(slot, SchemaKind::MultipleApplyApi),
    ];
    let definitions =
        read_generated_schema(&library, &declared, &mut store.tokens, &store.paths).unwrap();
    let mut builder = SchemaRegistry::builder();
    layerstack_schemas::register(&mut builder, &[Domain::UsdGeom], &mut store.tokens).unwrap();
    for definition in definitions {
        builder.register(definition);
    }
    let schemas = Arc::new(builder.build(&mut store.tokens));
    assert!(
        schemas.issues().is_empty(),
        "runtime declarations form a valid registry"
    );
    let cube_path = store.path("/Cube");
    let sphere_path = store.path("/Sphere");
    let sphere = store.tokens.intern("Sphere");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(cube_path, PrimSpec::def().with_type_name(cube));
    root.insert_prim(sphere_path, PrimSpec::def().with_type_name(sphere));
    store.insert_layer(root);
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    (store, live)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut store, mut live) = scene();
    let cube = store.path("/Cube");
    let gain = store.property_path("/Cube.pipeline:main:gain");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    edit.apply_api(cube, "PipelineTintAPI", None)?;
    edit.apply_api(cube, "PipelineSlotAPI", Some("main"))?;
    let transaction = edit.finish();
    let inverse = live.apply(&mut store, &transaction)?.inverse;
    let scene = layerstack_schemas::Scene::new(live.stage(), &store);
    assert!(
        scene.has_api(cube, "PipelineTintAPI", None),
        "runtime API is applied"
    );
    assert!(
        scene.has_api(cube, "PipelineSlotAPI", Some("main")),
        "named instance is applied"
    );
    let value = live
        .stage()
        .resolve_field_with_schema(gain.prim_path(), gain.property(), &store)
        .expect("runtime schema supplies its fallback")
        .value;
    assert_eq!(
        value,
        Value::Float(2.),
        "codeless fallback is visible without a typed getter"
    );
    println!("runtime PipelineSlotAPI:main supplies gain {value:?}");
    live.apply(&mut store, &inverse)?;
    assert!(
        !layerstack_schemas::Scene::new(live.stage(), &store).has_api(
            cube,
            "PipelineTintAPI",
            None
        ),
        "undo removes the authored application"
    );
    Ok(())
}
