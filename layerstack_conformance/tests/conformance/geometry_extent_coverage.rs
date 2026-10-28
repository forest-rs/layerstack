// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Intrinsic providers for concrete point geometry and versioned shapes.

use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PathId, PrimSpec, PropertyPath,
    PropertySpec, StageOptions, Transaction, Value,
};
use layerstack_schemas::{
    Scene, Time,
    bounds::{BoundsCache, BoundsError, BoundsOptions, Range3d},
};
use std::sync::Arc;

struct Fixture {
    store: InMemoryStore,
    live: LiveStage,
    path: PathId,
}

impl Fixture {
    fn new(schema: &str, properties: &[(&str, PropertySpec)]) -> Self {
        let mut store = InMemoryStore::default();
        let path = store.path("/Shape");
        let name = store.tokens().intern(schema);
        let mut prim = PrimSpec::def().with_type_name(name);
        for (name, value) in properties {
            let mut value = value.clone();
            if *name == "axis"
                && let Some(Value::String(axis)) = &value.default
            {
                value.default = Some(Value::Token(store.tokens().intern(axis)));
            }
            prim = prim.with_property(store.tokens().intern(name), value);
        }
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(path, prim);
        store.insert_layer(layer);
        let schemas = layerstack_schemas::openusd(store.tokens());
        let live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(Arc::new(schemas)),
                ..StageOptions::default()
            },
        );
        Self { store, live, path }
    }

    fn query(&self, cache: &mut BoundsCache) -> Result<Range3d, BoundsError> {
        cache
            .untransformed_bound(&Scene::new(self.live.stage(), &self.store), self.path)
            .map(|bounds| bounds.range)
    }

    fn edit(&mut self, cache: &mut BoundsCache, name: &str, value: Value) -> layerstack::Changes {
        let property = PropertyPath::new(self.path, self.store.tokens().intern(name));
        let mut edit = Transaction::new();
        edit.set_default(EditTarget::for_layer(LayerId(1)).property(property), value);
        let report = self.live.apply(&mut self.store, &edit).unwrap();
        cache.apply_changes(&Scene::new(self.live.stage(), &self.store), &report.changes);
        report.changes
    }
}

fn attr(value: impl Into<Value>) -> PropertySpec {
    PropertySpec::attribute().with_default(value)
}

fn cache(time: Time) -> BoundsCache {
    BoundsCache::new(time, BoundsOptions::default())
}

#[test]
fn registered_shapes_have_intrinsic_fallback_extents() {
    // OpenUSD 26.8 ComputeExtentFromPlugins, with no authored extent.
    for (schema, max) in [
        ("Plane", [1., 1., 0.]),
        ("Cylinder_1", [1.; 3]),
        ("Capsule_1", [0.5, 0.5, 1.]),
    ] {
        let fixture = Fixture::new(schema, &[]);
        assert_eq!(
            fixture.query(&mut cache(Time::Default)).unwrap(),
            Range3d {
                min: max.map(|v| -v),
                max
            },
            "{schema}",
        );
    }
}

#[test]
fn shape_axes_and_signed_dimensions_match_cpp() {
    // OpenUSD 26.8 plugin results: retain signed parameters rather than abs().
    // `intrinsic_bounds` checks `Capsule_1`, whose extent OpenUSD 26.11
    // computes differently, against its oracle.
    for (schema, maxima) in [
        ("Plane", [[0., 5., -3.], [-3., 0., 5.], [-3., 5., 0.]]),
        (
            "Cylinder_1",
            [[-2., -1., -1.], [-1., -2., -1.], [-1., -1., -2.]],
        ),
    ] {
        for (axis, max) in ["X", "Y", "Z"].into_iter().zip(maxima) {
            let fixture = Fixture::new(
                schema,
                &[
                    ("axis", attr(Value::String(axis.into()))),
                    ("width", attr(-6.)),
                    ("length", attr(10.)),
                    ("height", attr(-4.)),
                    ("radiusTop", attr(-1.)),
                    ("radiusBottom", attr(-3.)),
                ],
            );
            // Both BBoxCache implementations discard inverted signed extents
            // during aggregation; the intrinsic provider's exact endpoints
            // are covered separately before aggregation.
            assert!(
                Range3d {
                    min: max.map(|v| -v),
                    max
                }
                .is_empty()
            );
            assert!(
                fixture.query(&mut cache(Time::Default)).unwrap().is_empty(),
                "{schema} {axis}"
            );
            let positive = Fixture::new(
                schema,
                &[
                    ("axis", attr(Value::String(axis.into()))),
                    ("width", attr(6.)),
                    ("length", attr(10.)),
                    ("height", attr(4.)),
                    ("radiusTop", attr(1.)),
                    ("radiusBottom", attr(3.)),
                ],
            );
            let max = match (schema, axis) {
                ("Plane", "X") => [0., 5., 3.],
                ("Plane", "Y") => [3., 0., 5.],
                ("Plane", _) => [3., 5., 0.],
                (_, "X") => [2., 3., 3.],
                (_, "Y") => [3., 2., 3.],
                _ => [3., 3., 2.],
            };
            assert_eq!(
                positive.query(&mut cache(Time::Default)).unwrap(),
                Range3d {
                    min: max.map(|v| -v),
                    max
                },
                "{schema} {axis}"
            );
        }
        let fixture = Fixture::new(schema, &[("axis", attr(Value::String("invalid".into())))]);
        assert_eq!(
            fixture.query(&mut cache(Time::Default)),
            Err(BoundsError::ExtentUnavailable(fixture.path))
        );
    }
}

#[test]
fn double_shape_parameters_round_once_to_float_extents() {
    // Exact half-ULP inputs, independently evaluated with OpenUSD 26.8, and
    // with 26.11 for `Capsule_1`, whose ends sit where its slanted side
    // meets them.
    for (schema, min, max) in [
        ("Plane", [-1., -3., 0.], [1., 3., 0.]),
        ("Cylinder_1", [-1.; 3], [1.; 3]),
        (
            "Capsule_1",
            [
                -1.096_585_631_370_544_4,
                -1.096_585_631_370_544_4,
                -1.064_658_641_815_185_5,
            ],
            [
                1.096_585_631_370_544_4,
                1.096_585_631_370_544_4,
                2.546_585_798_263_55,
            ],
        ),
    ] {
        let fixture = Fixture::new(
            schema,
            &[
                ("width", attr(2.000_000_119_209_289_6)),
                ("length", attr(6.000_000_238_418_579)),
                ("height", attr(2.000_000_119_209_289_6)),
                ("radiusTop", attr(1.000_000_059_604_644_8)),
                ("radiusBottom", attr(0.1)),
            ],
        );
        assert_eq!(
            fixture.query(&mut cache(Time::Default)).unwrap(),
            Range3d { min, max }
        );
    }
}

#[test]
fn shape_samples_refresh_cached_bounds_and_authored_extents_win() {
    // OpenUSD 26.11 for `Capsule_1`, whose extent is not linear in its
    // radii.
    for (schema, input, first, middle, second) in [
        ("Plane", "width", [1., 1., 0.], [2., 1., 0.], [3., 1., 0.]),
        (
            "Cylinder_1",
            "radiusTop",
            [2., 2., 1.],
            [4., 4., 1.],
            [6., 6., 1.],
        ),
        (
            "Capsule_1",
            "radiusTop",
            [
                3.605_551_242_828_369,
                3.605_551_242_828_369,
                7.105_551_242_828_369,
            ],
            [
                14.560_219_764_709_473,
                14.560_219_764_709_473,
                29.060_220_718_383_79,
            ],
            [
                33.541_019_439_697_266,
                33.541_019_439_697_266,
                67.041_023_254_394_53,
            ],
        ),
    ] {
        // Plane intentionally resolves requested time (AOUSD Core §12.3–12.5).
        // OpenUSD 26.8's Plane plugin omits time; these values instead match its
        // static extent computation with the resolved numeric dimensions.
        let mut fixture = Fixture::new(
            schema,
            &[(
                input,
                PropertySpec::attribute()
                    .with_time_samples(vec![(0., Value::Double(2.)), (2., Value::Double(6.))]),
            )],
        );
        let mut cached = cache(Time::at(0.));
        assert_eq!(fixture.query(&mut cached).unwrap().max, first);
        cached.set_time(Time::at(2.));
        assert_eq!(fixture.query(&mut cached).unwrap().max, second);
        cached.set_time(Time::at(1.));
        assert_eq!(fixture.query(&mut cached).unwrap().max, middle);
        fixture.edit(
            &mut cached,
            "extent",
            Value::from(vec![[-9_f32; 3], [9.; 3]]),
        );
        assert_eq!(fixture.query(&mut cached).unwrap().max, [9.; 3]);
        fixture.edit(&mut cached, "extent", Value::Blocked);
        assert_eq!(fixture.query(&mut cached).unwrap().max, middle);
        let (dimension, maximum) = if schema == "Plane" {
            ("length", [2., 4., 0.])
        } else if schema == "Cylinder_1" {
            ("height", [4., 4., 4.])
        } else {
            (
                "height",
                [
                    4.366_062_164_306_641,
                    4.366_062_164_306_641,
                    10.116_062_164_306_64,
                ],
            )
        };
        fixture.edit(&mut cached, dimension, Value::Double(8.));
        assert_eq!(fixture.query(&mut cached).unwrap().max, maximum);
        assert_eq!(
            fixture.query(&mut cached),
            fixture.query(&mut cache(Time::at(1.)))
        );
    }
}

#[test]
fn tet_mesh_uses_inherited_point_geometry_and_tracks_time() {
    let mut fixture = Fixture::new(
        "TetMesh",
        &[(
            "points",
            PropertySpec::attribute().with_time_samples(vec![
                (0., Value::from(vec![[-1_f32, 2., 3.], [4., -5., 6.]])),
                (2., Value::from(vec![[1_f32, 4., 5.], [6., -3., 8.]])),
            ]),
        )],
    );
    let mut cached = cache(Time::at(0.));
    assert_eq!(
        fixture.query(&mut cached).unwrap(),
        Range3d {
            min: [-1., -5., 3.],
            max: [4., 2., 6.]
        }
    );
    cached.set_time(Time::at(1.));
    assert_eq!(
        fixture.query(&mut cached).unwrap(),
        Range3d {
            min: [0., -4., 4.],
            max: [5., 3., 7.]
        }
    );
    fixture.edit(
        &mut cached,
        "extent",
        Value::from(vec![[-9_f32; 3], [9.; 3]]),
    );
    assert_eq!(fixture.query(&mut cached).unwrap().max, [9.; 3]);
}

#[test]
fn edits_to_authored_shape_dimensions_invalidate_bounds_and_instancers() {
    for (schema, input, initial, before, after) in [
        ("Plane", "width", 2., [1., 1., 0.], [4., 1., 0.]),
        ("Plane", "length", 2., [1., 1., 0.], [1., 4., 0.]),
        ("Cylinder_1", "radiusTop", 1., [1.; 3], [8., 8., 1.]),
        ("Cylinder_1", "radiusBottom", 1., [1.; 3], [8., 8., 1.]),
        // OpenUSD 26.11: the wider end's sphere sits beyond the height.
        (
            "Capsule_1",
            "radiusTop",
            0.5,
            [0.5, 0.5, 1.],
            [
                60.530_982_971_191_406,
                60.530_982_971_191_406,
                121.030_982_971_191_4,
            ],
        ),
        (
            "Capsule_1",
            "radiusBottom",
            0.5,
            [0.5, 0.5, 1.],
            [
                60.530_982_971_191_406,
                60.530_982_971_191_406,
                0.533_186_495_304_107_7,
            ],
        ),
    ] {
        for ignore_visibility in [false, true] {
            // Pre-author the input: changing its default must be info-only,
            // rather than the structural resync caused by property creation.
            let mut fixture = Fixture::new(schema, &[(input, attr(initial))]);
            let instancer = fixture.store.path("/Instances");
            let target = EditTarget::for_layer(LayerId(1));
            let mut edit = layerstack_schemas::SchemaEdit::new(
                fixture.live.stage(),
                &mut fixture.store,
                target,
            );
            let instances =
                layerstack_schemas::usd_geom::PointInstancer::define(&mut edit, instancer)
                    .expect("outside any instance");
            instances.set_prototypes(&mut edit, &[layerstack::TargetPath::Prim(fixture.path)]);
            instances.set_proto_indices(&mut edit, &[0]);
            instances.set_positions(&mut edit, &[[10., 0., 0.]]);
            let transaction = edit.finish();
            fixture
                .live
                .apply(&mut fixture.store, &transaction)
                .unwrap();
            let options = BoundsOptions {
                ignore_visibility,
                ..BoundsOptions::default()
            };
            let mut cached = BoundsCache::new(Time::Default, options.clone());
            assert_eq!(fixture.query(&mut cached).unwrap().max, before);
            let scene = Scene::new(fixture.live.stage(), &fixture.store);
            assert_eq!(
                cached
                    .world_bound(&scene, instancer)
                    .unwrap()
                    .aligned_range()
                    .max,
                [before[0] + 10., before[1], before[2]]
            );
            let changes = fixture.edit(&mut cached, input, Value::Double(8.));
            assert!(changes.resynced.is_empty(), "{schema} {input}");
            assert!(changes.changed_info_only.contains(&fixture.path));
            assert_eq!(
                fixture.query(&mut cached).unwrap().max,
                after,
                "{schema} {input}"
            );
            let mut fresh = BoundsCache::new(Time::Default, options);
            assert_eq!(fixture.query(&mut cached), fixture.query(&mut fresh));
            let scene = Scene::new(fixture.live.stage(), &fixture.store);
            let actual = cached.world_bound(&scene, instancer).unwrap();
            assert_eq!(actual, fresh.world_bound(&scene, instancer).unwrap());
            assert_eq!(
                actual.aligned_range().max,
                [after[0] + 10., after[1], after[2]]
            );
        }
    }
}
