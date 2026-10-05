// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Supply custom local geometry bounds while the cache owns transforms and reuse.
//!
//! OpenUSD registers extent callbacks for derived `UsdGeomBoundable` types through
//! `UsdGeomRegisterComputeExtentFunction`. Its bounding-box cache uses authored
//! extents before computing them. Layerstack keeps registration and external
//! geometry revisions explicit and records provider inputs for invalidation.
//! <https://openusd.org/release/api/boundable_compute_extent_8h.html>
//! <https://openusd.org/release/api/class_usd_geom_b_box_cache.html>
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PropertySpec, PropertyType,
    SchemaDefinition, SchemaRegistry, Specifier, StageOptions, Transaction, Value,
};
use layerstack_schemas::{
    Domain, Scene, Time,
    bounds::{BoundsCache, BoundsOptions, Range3d},
    extent::{ExtentContext, ExtentError, ExtentProvider, ExtentProviders},
    value,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct PaddedBox {
    // An application may change external geometry independently of USD edits.
    padding_tenths: Arc<AtomicUsize>,
    evaluations: Arc<AtomicUsize>,
}
impl ExtentProvider for PaddedBox {
    fn compute_extent(&self, context: &mut ExtentContext<'_>) -> Result<Range3d, ExtentError> {
        self.evaluations.fetch_add(1, Ordering::Relaxed);
        let prim = context.prim();
        let size = context
            .read_value(prim, "size", value::read_double)?
            .filter(|size| size.is_finite() && *size >= 0.0)
            .ok_or_else(|| ExtentError::Failed {
                prim,
                message: "PaddedBox requires a finite, nonnegative size".into(),
            })?;
        let half_size = size * 0.5 + self.padding_tenths.load(Ordering::Relaxed) as f64 / 10.0;
        Ok(Range3d {
            min: [-half_size; 3],
            max: [half_size; 3],
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    let layer = LayerId(1);
    store.insert_layer(Layer::new(layer));
    let custom = store.tokens.intern("PaddedBox");
    let boundable = store.tokens.intern("Boundable");
    let mut builder = SchemaRegistry::builder();
    layerstack_schemas::register(&mut builder, &[Domain::UsdGeom], &mut store.tokens)?;
    builder.register(SchemaDefinition::typed(custom).with_parent(boundable));
    let schemas = Arc::new(builder.build(&mut store.tokens));
    assert!(schemas.issues().is_empty(), "{:?}", schemas.issues());
    let padding = Arc::new(AtomicUsize::new(0));
    let evaluations = Arc::new(AtomicUsize::new(0));
    let mut providers = ExtentProviders::default();
    providers
        .register(
            &schemas,
            &store.tokens,
            custom,
            Arc::new(PaddedBox {
                padding_tenths: padding.clone(),
                evaluations: evaluations.clone(),
            }),
        )
        .unwrap();
    let shape = store.path("/Shape");
    let size = store.property_path("/Shape.size");
    let label = store.property_path("/Notes.label");
    let extent = store.property_path("/Shape.extent");
    let mut live = LiveStage::compose(
        &mut store,
        layer,
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    let target = EditTarget::for_layer(layer);
    let mut author = Transaction::new();
    author.create_prim(target.prim(shape), Specifier::Def, Some(custom));
    author.create_property(
        target.property(size),
        PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)))
            .with_default(Value::Double(2.0)),
    );
    author.create_property(
        target.property(label),
        PropertySpec::typed_attribute(PropertyType::new("string", false, Value::String("".into())))
            .with_default(Value::String("initial".into())),
    );
    live.apply(&mut store, &author)?;
    let mut bounds =
        BoundsCache::with_extent_providers(Time::Default, BoundsOptions::default(), providers);
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        bounds.world_bound(&scene, shape)?.aligned_range().max,
        [1.0; 3],
        "size 2 produces half-size 1"
    );
    bounds.world_bound(&scene, shape)?;
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        1,
        "repeated reads reuse the provider result"
    );

    let mut unrelated = Transaction::new();
    unrelated.set_default(target.property(label), Value::String("edited".into()));
    let change = live.apply(&mut store, &unrelated)?;
    let scene = Scene::new(live.stage(), &store);
    bounds.apply_changes(&scene, &change.changes);
    bounds.world_bound(&scene, shape)?;
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        1,
        "unrelated edits retain the custom extent"
    );

    let mut resize = Transaction::new();
    resize.set_default(target.property(size), Value::Double(4.0));
    let change = live.apply(&mut store, &resize)?;
    let scene = Scene::new(live.stage(), &store);
    bounds.apply_changes(&scene, &change.changes);
    assert_eq!(
        bounds.world_bound(&scene, shape)?.aligned_range().max,
        [2.0; 3],
        "the edited size 4 produces half-size 2"
    );
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        2,
        "tracked size changes reevaluate the provider"
    );

    padding.store(10, Ordering::Relaxed);
    bounds.set_extent_revision(1);
    assert_eq!(
        bounds.world_bound(&scene, shape)?.aligned_range().max,
        [3.0; 3],
        "external padding expands half-size 2 to 3"
    );
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        3,
        "external geometry requires an explicit revision"
    );

    let mut override_extent = Transaction::new();
    override_extent.create_property(
        target.property(extent),
        PropertySpec::typed_attribute(PropertyType::new("float3", true, Value::Vec3f([0.0; 3])))
            .with_default(Value::from(vec![[-8.0_f32; 3], [8.0; 3]])),
    );
    let change = live.apply(&mut store, &override_extent)?;
    let scene = Scene::new(live.stage(), &store);
    bounds.apply_changes(&scene, &change.changes);
    assert_eq!(
        bounds.world_bound(&scene, shape)?.aligned_range().max,
        [8.0; 3],
        "authored extent provides the final bounds"
    );
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        3,
        "authored extent takes precedence over host computation"
    );
    println!("Custom box: half-size 1 -> 2; external padding -> 3; authored extent -> 8.");
    println!("Three provider evaluations; repeated and unrelated reads reused the result.");
    Ok(())
}
