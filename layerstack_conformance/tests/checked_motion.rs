// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Computed motion must not silently replace corrupt retained data with defaults.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{ArrayReadError, DeferredArraySource, LayerId, Time, TypedArray, Value};
use layerstack_schemas::{
    Scene,
    point_instancer::{InstanceTransformOptions, PointInstancerError},
    point_motion::PointMotionError,
    usd_geom::{PointBased, PointInstancer},
};
use std::sync::Arc;
#[path = "support/schema_scene.rs"]
mod support;

#[derive(Debug)]
struct Failed {
    error: ArrayReadError,
    kind: Value,
}
impl DeferredArraySource for Failed {
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
        Err(&self.error)
    }
    fn element_kind(&self) -> Value {
        self.kind.clone()
    }
}

#[test]
fn point_motion_preserves_required_and_optional_decode_failures() {
    for name in ["points", "velocities", "accelerations"] {
        for time in [Time::Default, Time::held(1.), Time::at(1.5)] {
            let (mut store, _) = support::scene(
                "#usda 1.0\ndef Points \"P\" {\n point3f[] points = [(1,2,3)]\n vector3f[] velocities.timeSamples = { 1: [(1,0,0)], 2: [(1,0,0)] }\n vector3f[] accelerations.timeSamples = { 1: [(1,0,0)], 2: [(1,0,0)] }\n}",
            );
            let path = store.path("/P");
            let token = store.tokens.lookup(name).unwrap();
            let error = ArrayReadError::InvalidData("corrupt point motion".into());
            let failed = Value::TypedArray(TypedArray::Deferred(Arc::new(Failed {
                error: error.clone(),
                kind: Value::Vec3f([0.; 3]),
            })));
            let spec = store
                .layers
                .get_mut(&LayerId(1))
                .unwrap()
                .prims
                .get_mut(&path)
                .unwrap()
                .property_mut(token)
                .unwrap();
            spec.default = Some(failed.clone());
            spec.time_samples = Some(vec![(1., failed.clone()), (2., failed)].into());
            let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
            let live = layerstack::LiveStage::compose(
                &mut store,
                LayerId(1),
                layerstack::StageOptions {
                    schemas: Some(schemas),
                    ..Default::default()
                },
            );
            let scene = Scene::new(live.stage(), &store);
            let prim = PointBased::new(&scene, path).unwrap();
            let expected = PointMotionError::Decode {
                property: layerstack::PropertyPath::new(path, token),
                error,
            };
            assert_eq!(prim.motion_inputs(time), Err(expected.clone()));
            assert_eq!(
                prim.compute_points_at_times(&[time, time], time),
                Err(expected)
            );
        }
    }
}

#[test]
fn instancer_transform_and_mask_queries_preserve_decode_failures() {
    for (name, kind) in [
        ("protoIndices", Value::Int(0)),
        ("positions", Value::Vec3f([0.; 3])),
        ("scales", Value::Vec3f([0.; 3])),
        ("orientationsf", Value::Quatf([0.; 4])),
        ("velocities", Value::Vec3f([0.; 3])),
        ("accelerations", Value::Vec3f([0.; 3])),
        ("angularVelocities", Value::Vec3f([0.; 3])),
        ("ids", Value::Int64(0)),
        ("invisibleIds", Value::Int64(0)),
    ] {
        for time in [Time::Default, Time::held(1.), Time::at(1.5)] {
            let (mut store, _) = support::scene(
                "#usda 1.0\ndef PointInstancer \"I\" {\n rel prototypes = </I/P>\n int[] protoIndices = [0]\n point3f[] positions = [(1,2,3)]\n float3[] scales = [(1,1,1)]\n quatf[] orientationsf = [(1,0,0,0)]\n vector3f[] velocities = [(1,0,0)]\n vector3f[] accelerations = [(1,0,0)]\n vector3f[] angularVelocities = [(1,0,0)]\n int64[] ids = [7]\n int64[] invisibleIds = [8]\n def Cube \"P\" {}\n}",
            );
            let path = store.path("/I");
            let token = store.tokens.lookup(name).unwrap();
            let error = ArrayReadError::BudgetExceeded { limit: 17 };
            let failed = Value::TypedArray(TypedArray::Deferred(Arc::new(Failed {
                error: error.clone(),
                kind: kind.clone(),
            })));
            let spec = store
                .layers
                .get_mut(&LayerId(1))
                .unwrap()
                .prims
                .get_mut(&path)
                .unwrap()
                .property_mut(token)
                .unwrap();
            spec.default = Some(failed.clone());
            spec.time_samples = Some(vec![(1., failed.clone()), (2., failed)].into());
            let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
            let live = layerstack::LiveStage::compose(
                &mut store,
                LayerId(1),
                layerstack::StageOptions {
                    schemas: Some(schemas),
                    ..Default::default()
                },
            );
            let scene = Scene::new(live.stage(), &store);
            let prim = PointInstancer::new(&scene, path).unwrap();
            let expected = PointInstancerError::Decode {
                property: layerstack::PropertyPath::new(path, token),
                error,
            };
            assert_eq!(
                prim.prepare_instance_transforms(time, time, InstanceTransformOptions::default())
                    .unwrap_err(),
                expected.clone()
            );
            assert_eq!(
                prim.compute_instance_transforms_at_times(
                    &[time, time],
                    time,
                    InstanceTransformOptions::default()
                ),
                Err(expected.clone())
            );
            if matches!(name, "ids" | "invisibleIds") {
                assert_eq!(prim.try_compute_mask(time), Err(expected));
            }
        }
    }
}
