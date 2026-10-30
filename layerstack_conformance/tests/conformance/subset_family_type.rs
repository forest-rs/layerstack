// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The family type subset creation authors, against OpenUSD call by call.
//!
//! `scripts/subset_family_type_oracle.py` calls C++
//! `UsdGeomSubset::CreateGeomSubset` and `CreateUniqueGeomSubset` with and
//! without a family name and type on a new stage, recording after each the
//! root layer's attribute specs and `GetFamilyType` of each family. The
//! test replays the calls through `GeomSubsetEdit::create` and
//! `create_unique`, applying each before the next.

use crate::support::schema_scene as support;

use std::collections::BTreeMap;

use layerstack::{EditTarget, InMemoryStore, LayerId, PropertyKind, Variability};
use layerstack_schemas::subset::SubsetFamilyType;
use layerstack_schemas::usd_geom::{GeomSubsetEdit, GeomSubsetElementType as Element, Imageable};
use layerstack_schemas::{Scene, SchemaEdit};
use serde::Deserialize;
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/subset_family_type/oracle.json");

const SCENE: &str = r#"#usda 1.0
def Mesh "M"
{
    int[] faceVertexCounts = [3, 3]
    int[] faceVertexIndices = [0, 1, 2, 1, 3, 2]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0)]
}
"#;

const FAMILIES: [&str; 3] = ["", "parts", "looks"];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    calls: Vec<Call>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Call {
    call: String,
    subset: String,
    specs: BTreeMap<String, Spec>,
    family_types: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Spec {
    #[serde(rename = "type")]
    type_name: String,
    custom: bool,
    uniform: bool,
    default: Json,
}

/// Every attribute spec of the root layer.
fn specs(store: &InMemoryStore) -> BTreeMap<String, Spec> {
    let layer = store.layer(LayerId(1)).unwrap();
    let mut found = BTreeMap::new();
    for (path, prim) in layer.prims().iter() {
        for entry in &prim.properties {
            let spec = &entry.spec;
            if spec.kind != PropertyKind::Attribute {
                continue;
            }
            found.insert(
                format!(
                    "{}.{}",
                    store.paths().display(*path, store.tokens()),
                    store.tokens().resolve(entry.name)
                ),
                Spec {
                    type_name: spec
                        .type_name
                        .as_ref()
                        .map(|ty| ty.type_name().to_string())
                        .unwrap_or_default(),
                    custom: spec.custom,
                    uniform: spec.variability == Variability::Uniform,
                    default: spec.default.as_ref().map_or(Json::Null, |value| {
                        crate::support::layer_json::value(value, store)
                    }),
                },
            );
        }
    }
    found
}

/// One subset creation call.
struct Create {
    label: &'static str,
    unique: bool,
    name: &'static str,
    indices: &'static [i32],
    family: &'static str,
    family_type: Option<SubsetFamilyType>,
}

#[test]
fn subset_creation_authors_family_types_as_openusd_does() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, mut live) = support::scene(SCENE);
    let mesh = store.path("/M");
    use SubsetFamilyType::{NonOverlapping, Partition, Unrestricted};
    let create = |label, unique, name, indices, family, family_type| Create {
        label,
        unique,
        name,
        indices,
        family,
        family_type,
    };
    let calls = [
        create(
            "create a: type, no family",
            false,
            "a",
            &[0],
            "",
            Some(Partition),
        ),
        create(
            "create b: family parts, no type",
            false,
            "b",
            &[0],
            "parts",
            None,
        ),
        create(
            "create c: parts partition",
            false,
            "c",
            &[0],
            "parts",
            Some(Partition),
        ),
        create(
            "create d: parts nonOverlapping",
            false,
            "d",
            &[1],
            "parts",
            Some(NonOverlapping),
        ),
        create(
            "create c again: parts unrestricted",
            false,
            "c",
            &[1],
            "parts",
            Some(Unrestricted),
        ),
        create(
            "create d again: parts, no type",
            false,
            "d",
            &[0],
            "parts",
            None,
        ),
        create(
            "unique c: looks nonOverlapping",
            true,
            "c",
            &[0],
            "looks",
            Some(NonOverlapping),
        ),
        create(
            "unique c: no family, unrestricted",
            true,
            "c",
            &[1],
            "",
            Some(Unrestricted),
        ),
    ];
    assert_eq!(calls.len(), oracle.calls.len());
    for (call, expected) in calls.into_iter().zip(&oracle.calls) {
        let Create {
            label,
            unique,
            name,
            indices,
            family,
            family_type,
        } = call;
        assert_eq!(label, expected.call);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let create = if unique {
            GeomSubsetEdit::create_unique
        } else {
            GeomSubsetEdit::create
        };
        let subset = create(
            &mut edit,
            mesh,
            name,
            Element::Face,
            indices,
            family,
            family_type,
        )
        .unwrap_or_else(|error| panic!("{label}: {error}"));
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let geom = Imageable::get(&scene, mesh).unwrap();
        let actual = Call {
            call: label.into(),
            subset: store.paths().display(subset.path(), store.tokens()),
            specs: specs(&store),
            family_types: FAMILIES
                .iter()
                .map(|family| ((*family).to_owned(), geom.subset_family_type(family)))
                .collect(),
        };
        assert_eq!(&actual, expected, "{label}");
    }
}
