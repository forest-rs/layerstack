// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Payloads to dynamic formats compose their arguments from the opinions
//! stronger than them, and target the layer the arguments name.

use alloc::{format, string::String, sync::Arc, vec::Vec};

use super::*;
use crate::{
    AssetRequest, InMemoryStore, Layer, LayerOffset, MetadataDefinition, MetadataTarget,
    MetadataType, PrimSpec, Reference, ReferenceTarget, SchemaRegistry, Stage, StageOptions,
};

/// A terrain generator: `rows` from the `rows` field, `tiles` from the keys
/// of the `tiles` dictionary, and `unregistered` naming a field it may not
/// read.
#[derive(Debug)]
struct Terrain;

impl DynamicFileFormat for Terrain {
    fn name(&self) -> &str {
        "terrain"
    }
    fn extensions(&self) -> &[&str] {
        &["terrain"]
    }
    fn compose_args(&self, _asset: &str, context: &mut DynamicFormatContext<'_>) -> DynamicArgs {
        let mut args = FormatArgs::new();
        if let Ok(Some(Value::Int(rows))) = context.compose_value("rows") {
            args.insert("rows", format!("{rows}"));
        }
        if let Ok(Some(Value::Dictionary(tiles))) = context.compose_value("tiles") {
            let keys: Vec<&str> = tiles.iter().map(|(key, _)| &**key).collect();
            args.insert("tiles", keys.join(","));
        }
        if let Err(error) = context.compose_value("comment") {
            args.insert("unregistered", error.field);
        }
        DynamicArgs {
            args,
            dependency: Value::String("terrain".into()),
        }
    }
    fn can_field_change_affect_args(
        &self,
        field: &str,
        old: Option<&Value>,
        new: Option<&Value>,
        dependency: &Value,
    ) -> bool {
        assert_eq!(dependency, &Value::String("terrain".into()));
        // Fewer than one row reads as none.
        let none = |value: Option<&Value>| matches!(value, Some(Value::Int(rows)) if *rows < 1);
        !(field == "rows" && none(old) && none(new))
    }
}

fn registry(store: &mut InMemoryStore) -> Arc<SchemaRegistry> {
    let mut builder = SchemaRegistry::builder();
    for (name, type_name) in [("rows", "int"), ("tiles", "dictionary")] {
        builder
            .register_metadata(
                MetadataDefinition::new(
                    store.tokens.intern(name),
                    MetadataType::parse(type_name).unwrap(),
                    [MetadataTarget::Prim],
                ),
                &store.tokens,
            )
            .unwrap();
    }
    Arc::new(builder.build(&store.tokens))
}

fn dictionary(store: &InMemoryStore, keys: &[&str]) -> Value {
    let _ = store;
    Value::Dictionary(
        keys.iter()
            .map(|key| (Arc::from(*key), Value::Int(1)))
            .collect(),
    )
}

/// `/Site` of a parameter layer has three payloads: `</Near>`, the
/// terrain, and `</Far>`; it authors `tiles` itself. `/World` references
/// it, or, `direct`, is it, in the root layer.
fn scene(direct: bool) -> (InMemoryStore, StageOptions) {
    let mut store = InMemoryStore::default();
    let schemas = registry(&mut store);
    let (rows, tiles) = (store.tokens.intern("rows"), store.tokens.intern("tiles"));
    let (world, near, far, site) = (
        store.path("/World"),
        store.path("/Near"),
        store.path("/Far"),
        store.path("/Site"),
    );
    let host = if direct { LayerId(1) } else { LayerId(2) };
    let mut params = Layer::new(LayerId(2));
    params.insert_prim(
        near,
        PrimSpec::def()
            .with_field(rows, Value::Int(7))
            .with_field(tiles, dictionary(&store, &["a"])),
    );
    params.insert_prim(
        far,
        PrimSpec::def()
            .with_field(rows, Value::Int(9))
            .with_field(tiles, dictionary(&store, &["c"])),
    );
    let payloads = PrimSpec::def()
        .with_field(tiles, dictionary(&store, &["b"]))
        .with_payload(Reference::new(LayerId(2), near))
        .with_payload(Reference::deferred(
            host,
            "field.terrain:SDF_FORMAT_ARGS:seed=3",
            ReferenceTarget::DefaultPrim,
            LayerOffset::IDENTITY,
        ))
        .with_payload(Reference::new(LayerId(2), far));
    let mut root = Layer::new(LayerId(1));
    if direct {
        root.insert_prim(world, payloads);
    } else {
        params.insert_prim(site, payloads);
        root.insert_prim(
            world,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), site)),
        );
    }
    store.insert_layer(params);
    store.insert_layer(root);
    let options = StageOptions {
        schemas: Some(schemas),
        dynamic_formats: DynamicFormats::default().with(Arc::new(Terrain)).unwrap(),
        ..StageOptions::default()
    };
    (store, options)
}

const IDENTIFIER: &str =
    "field.terrain:SDF_FORMAT_ARGS:rows=7&seed=3&tiles=a,b&unregistered=comment";

#[test]
fn payload_arguments_compose_from_the_opinions_stronger_than_it() {
    let (mut store, options) = scene(false);
    let world = store.path("/World");
    let stage = Stage::compose(&mut store, LayerId(1), options.clone());
    // `</Far>` is weaker than the terrain: neither its `rows` nor its
    // `tiles` take part. The authored `seed` stays.
    assert_eq!(
        stage.asset_requests(),
        [AssetRequest {
            anchor: LayerId(2),
            asset_path: IDENTIFIER.into(),
            prim: world,
            arc: crate::ArcKind::Payloads,
        }]
    );
    let dependency = stage.dynamic_dependency(world).unwrap();
    assert_eq!(
        dependency.relevant_fields().collect::<Vec<_>>(),
        ["rows", "tiles"]
    );
    assert_eq!(dependency.relevant_attributes().len(), 0);
    let (site, near, far) = (store.path("/Site"), store.path("/Near"), store.path("/Far"));
    let sites: Vec<_> = dependency.sites().collect();
    assert!(sites.contains(&(LayerId(2), site)), "{sites:?}");
    assert!(sites.contains(&(LayerId(2), near)), "{sites:?}");
    assert!(!sites.contains(&(LayerId(2), far)), "{sites:?}");
    assert!(dependency.can_field_change_affect_args("rows", None, Some(&Value::Int(2))));
    assert!(!dependency.can_field_change_affect_args(
        "rows",
        Some(&Value::Int(0)),
        Some(&Value::Int(-1))
    ));
    assert!(!dependency.can_field_change_affect_args("comment", None, None));

    // The host reads the layer the arguments name; the payload targets it.
    let (grid, row) = (store.path("/Grid"), store.path("/Grid/Row0"));
    let mut generated = Layer::new(LayerId(3));
    generated.set_default_prim(Some(store.tokens.intern("Grid")));
    generated.define_prim(grid, PrimSpec::def(), &store.paths);
    generated.define_prim(row, PrimSpec::def(), &store.paths);
    store.insert_layer(generated);
    store.insert_asset_layer(LayerId(2), IDENTIFIER, LayerId(3));
    let stage = Stage::compose(&mut store, LayerId(1), options);
    assert!(stage.asset_requests().is_empty());
    assert!(stage.has_prim(store.path("/World/Row0")));
    // The child's index composes no dynamic arguments of its own.
    assert!(
        stage
            .dynamic_dependency(store.path("/World/Row0"))
            .is_none()
    );
}

#[test]
fn payloads_of_a_root_node_read_every_sibling_payload() {
    // As in OpenUSD, a payload's strength is compared with its siblings'
    // by the namespace depth of its parent, which is 0 for a prim index's
    // root node: the payloads authored on the prim itself read the opinions
    // of every payload beside them (`PcpCompareSiblingPayloadNodeStrength`).
    let (mut store, options) = scene(true);
    let stage = Stage::compose(&mut store, LayerId(1), options);
    let requested: Vec<&str> = stage
        .asset_requests()
        .iter()
        .map(|request| &*request.asset_path)
        .collect();
    assert_eq!(
        requested,
        ["field.terrain:SDF_FORMAT_ARGS:rows=7&seed=3&tiles=a,b,c&unregistered=comment"]
    );
}

#[test]
fn only_payloads_to_registered_formats_are_dynamic() {
    let (mut store, mut options) = scene(false);
    let world = store.path("/World");
    let requests = |store: &mut InMemoryStore, options: &StageOptions| -> Vec<String> {
        Stage::compose(store, LayerId(1), options.clone())
            .asset_requests()
            .iter()
            .map(|request| request.asset_path.clone())
            .collect()
    };
    // A reference to the same asset reads it as authored.
    let reference = Reference::deferred(
        LayerId(1),
        "field.terrain",
        ReferenceTarget::DefaultPrim,
        LayerOffset::IDENTITY,
    );
    let spec = store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .prims
        .get_mut(&world)
        .unwrap();
    spec.references.make_mut().append.push(reference);
    assert_eq!(
        requests(&mut store, &options),
        ["field.terrain", IDENTIFIER]
    );
    // Without the format registered, the payload reads its asset as
    // authored too.
    options.dynamic_formats = DynamicFormats::default();
    assert_eq!(
        requests(&mut store, &options),
        ["field.terrain", "field.terrain:SDF_FORMAT_ARGS:seed=3"]
    );
    let stage = Stage::compose(&mut store, LayerId(1), options);
    assert!(stage.dynamic_dependency(world).is_none());
}

#[test]
fn registration_is_explicit_and_exclusive() {
    #[derive(Debug)]
    struct Claims(&'static [&'static str]);
    impl DynamicFileFormat for Claims {
        fn name(&self) -> &str {
            "claims"
        }
        fn extensions(&self) -> &[&str] {
            self.0
        }
        fn compose_args(&self, _: &str, _: &mut DynamicFormatContext<'_>) -> DynamicArgs {
            DynamicArgs::default()
        }
    }
    let mut formats = DynamicFormats::default();
    let terrain: Arc<dyn DynamicFileFormat> = Arc::new(Terrain);
    formats.add(Arc::clone(&terrain)).unwrap();
    formats.add(terrain).unwrap();
    assert_eq!(formats.iter().len(), 1);
    assert!(matches!(
        formats.add(Arc::new(Claims(&["terrain"]))),
        Err(DynamicFormatRegistrationError::DuplicateExtension { registered, .. }) if registered == "terrain"
    ));
    assert!(matches!(
        formats.add(Arc::new(Claims(&["usd"]))),
        Err(DynamicFormatRegistrationError::BuiltinExtension { .. })
    ));
    assert!(matches!(
        formats.add(Arc::new(Claims(&["T"]))),
        Err(DynamicFormatRegistrationError::InvalidExtension { .. })
    ));
    assert!(matches!(
        formats.add(Arc::new(Claims(&[]))),
        Err(DynamicFormatRegistrationError::NoExtensions { .. })
    ));
    let name = |asset: &str| formats.for_asset(asset).map(DynamicFileFormat::name);
    assert_eq!(name("a/b.TERRAIN:SDF_FORMAT_ARGS:rows=1"), Some("terrain"));
    assert_eq!(name("a/b.usda"), None);
    assert_eq!(name("p.usdz[b.terrain]"), None);
}
