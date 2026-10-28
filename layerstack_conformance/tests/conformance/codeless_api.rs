// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Runtime API libraries use the same checked authoring as generated Rust schemas.
use layerstack::{
    AssetResolveError, AssetResolver, CannotApply, EditTarget, InMemoryStore, Layer, LayerId,
    LiveStage, PathInterner, PrimSpec, ResolvedAsset, SchemaKind, SchemaRegistry, StageOptions,
    TokenInterner, Value,
    schema::{SchemaDeclaration, read_generated_schema},
};
use layerstack_schemas::{ApplyError, Domain, SchemaEdit, usd_geom::Cube};
use std::sync::Arc;
struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
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
class "PipelineTintAPI" { color3f pipeline:tint = (0.25, 0.5, 1); }
class "PipelineSlotAPI" { float pipeline:__INSTANCE_NAME__:gain = 2; }
"#;
    let library = layerstack_usda::read_usda(
        source,
        LayerId(9),
        &store.tokens,
        &store.paths,
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
        read_generated_schema(&library, &declared, &store.tokens, &store.paths).unwrap();
    let mut builder = SchemaRegistry::builder();
    layerstack_schemas::register(&mut builder, &[Domain::UsdGeom], &store.tokens).unwrap();
    for definition in definitions {
        builder.register(definition);
    }
    let schemas = Arc::new(builder.build(&store.tokens));
    assert!(
        schemas.issues().is_empty(),
        "runtime schema declarations form a valid registry"
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
#[test]
fn runtime_single_and_multiple_apply_publish_defaults_and_undo() {
    let (mut store, mut live) = scene();
    let path = store.path("/Cube");
    let before = store.layer(LayerId(1)).unwrap().clone();
    let tint = store.property_path("/Cube.pipeline:tint");
    let gain = store.property_path("/Cube.pipeline:main:gain");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    edit.apply_api(path, "PipelineTintAPI", None).unwrap();
    edit.apply_api(path, "PipelineTintAPI", None).unwrap();
    edit.apply_api(path, "PipelineSlotAPI", Some("main"))
        .unwrap();
    let transaction = edit.finish();
    assert_eq!(*store.layer(LayerId(1)).unwrap(), before);
    let inverse = live.apply(&mut store, &transaction).unwrap().inverse;
    let scene = layerstack_schemas::Scene::new(live.stage(), &store);
    assert!(scene.has_api(path, "PipelineTintAPI", None));
    assert!(scene.has_api(path, "PipelineSlotAPI", Some("main")));
    assert_eq!(
        live.view(&store)
            .attribute(tint)
            .and_then(|tint| tint.get(layerstack::Time::Default))
            .unwrap()
            .value,
        Value::Vec3f([0.25, 0.5, 1.])
    );
    assert_eq!(
        live.view(&store)
            .attribute(gain)
            .and_then(|gain| gain.get(layerstack::Time::Default))
            .unwrap()
            .value,
        Value::Float(2.)
    );
    live.apply(&mut store, &inverse).unwrap();
    assert_eq!(*store.layer(LayerId(1)).unwrap(), before);
}
#[test]
fn rejected_runtime_application_adds_nothing_and_pending_definition_is_validated() {
    let (mut store, mut live) = scene();
    let cube = store.path("/Cube");
    let sphere = store.path("/Sphere");
    let missing = store.path("/Missing");
    let created = store.path("/Created");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert_eq!(
        edit.apply_api(missing, "PipelineTintAPI", None),
        Err(ApplyError::CannotApply(CannotApply::NoSuchPrim))
    );
    assert_eq!(
        edit.apply_api(cube, "UnknownAPI", None),
        Err(ApplyError::CannotApply(CannotApply::NotAnAppliedSchema))
    );
    assert!(matches!(
        edit.apply_api(sphere, "PipelineTintAPI", None),
        Err(ApplyError::CannotApply(CannotApply::NotATarget { .. }))
    ));
    assert_eq!(
        edit.apply_api(cube, "PipelineTintAPI", Some("main")),
        Err(ApplyError::CannotApply(CannotApply::UnexpectedInstanceName))
    );
    assert_eq!(
        edit.apply_api(cube, "PipelineSlotAPI", None),
        Err(ApplyError::CannotApply(CannotApply::MissingInstanceName))
    );
    assert!(
        edit.apply_api(cube, "PipelineSlotAPI", Some("invalid.name"))
            .is_err()
    );
    assert!(edit.transaction().is_empty());
    Cube::define(&mut edit, created).expect("outside any instance");
    edit.apply_api(created, "PipelineTintAPI", None).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    assert!(
        layerstack_schemas::Scene::new(live.stage(), &store).has_api(
            created,
            "PipelineTintAPI",
            None
        )
    );
}
