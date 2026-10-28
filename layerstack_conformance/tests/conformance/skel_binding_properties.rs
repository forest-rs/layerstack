// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The properties a skinning query names for each input, against OpenUSD
//! 26.11, inside native instances and out.
use crate::support::schema_scene as support;
use layerstack::InMemoryStore;
use layerstack_schemas::{
    Scene,
    skel::{BindingProperty, SkinningQuery},
    usd_skel::SkelRoot,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    targets: Vec<Target>,
}
#[derive(Deserialize)]
struct Target {
    geometry: String,
    joint_indices: Option<String>,
    joint_weights: Option<String>,
    geom_bind_transform: Option<String>,
    skinning_method: Option<String>,
    blend_shapes: Option<String>,
    blend_shape_targets: Option<String>,
}

/// Checks `got` against the property C++ names: inside an instance C++
/// names the prototype's (`/__Prototype_1/<prim>.<name>`), which layerstack
/// names at the instance proxy with the prim's place in the prototype.
fn check(
    scene: &Scene<'_>,
    store: &InMemoryStore,
    geometry: &str,
    got: Option<BindingProperty>,
    want: Option<&String>,
    label: &str,
) {
    let Some(want) = want else {
        assert_eq!(got, None, "{geometry} {label}");
        return;
    };
    let got = got.unwrap_or_else(|| panic!("{geometry} {label}: none"));
    let text = got.path.display(store.paths(), store.tokens());
    match want.strip_prefix("/__Prototype_1/") {
        Some(relative) => {
            // The instance root is the geometry's second-level ancestor.
            let root: String = geometry.split('/').take(3).collect::<Vec<_>>().join("/");
            assert_eq!(text, format!("{root}/{relative}"), "{geometry} {label}");
            let location = got.prototype.expect("a prototype location");
            let instance = scene.stage().prim(store.path(&root), store).unwrap();
            assert_eq!(
                Some(location.prototype),
                instance.prototype(),
                "{geometry} {label}"
            );
            let (prim, _) = relative.split_once('.').unwrap();
            let names: Vec<&str> = location
                .relative_path
                .iter()
                .map(|token| store.tokens().resolve(*token))
                .collect();
            assert_eq!(names.join("/"), prim, "{geometry} {label}");
        }
        None => {
            assert_eq!(&text, want, "{geometry} {label}");
            assert_eq!(got.prototype, None, "{geometry} {label}");
        }
    }
}

#[test]
fn binding_properties_match_cpp() {
    let oracle: Oracle = serde_json::from_str(include_str!(
        "../../fixtures/skel_binding_properties/oracle.json"
    ))
    .unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(include_str!(
        "../../fixtures/skel_binding_properties/scene.usda"
    ));
    let scene = Scene::new(live.stage(), &store);
    let bindings = SkelRoot::new(&scene, store.path("/World"))
        .unwrap()
        .bindings_with_instance_proxies();
    let queries: Vec<(String, &SkinningQuery<'_>)> = bindings
        .bindings
        .iter()
        .filter_map(|binding| {
            let query = binding.skinning.as_ref()?;
            Some((
                store.paths().display(binding.geometry, store.tokens()),
                query,
            ))
        })
        .collect();
    assert_eq!(queries.len(), oracle.targets.len());
    for want in &oracle.targets {
        let (geometry, query) = queries
            .iter()
            .find(|(path, _)| *path == want.geometry)
            .unwrap_or_else(|| panic!("no binding for {}", want.geometry));
        let rows = [
            (
                Some(query.joint_indices_property()),
                &want.joint_indices,
                "jointIndices",
            ),
            (
                Some(query.joint_weights_property()),
                &want.joint_weights,
                "jointWeights",
            ),
            (
                query.geom_bind_transform_property(),
                &want.geom_bind_transform,
                "geomBindTransform",
            ),
            (
                query.skinning_method_property(),
                &want.skinning_method,
                "skinningMethod",
            ),
            (
                query.blend_shapes_property(),
                &want.blend_shapes,
                "blendShapes",
            ),
            (
                query.blend_shape_targets_property(),
                &want.blend_shape_targets,
                "blendShapeTargets",
            ),
        ];
        for (got, want, label) in rows {
            check(&scene, &store, geometry, got, want.as_ref(), label);
        }
    }
}
