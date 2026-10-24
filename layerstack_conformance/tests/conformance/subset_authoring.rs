// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checked subset creation and explicit family authoring are atomic batches.
use crate::support::schema_scene as support;
use layerstack::{EditTarget, LayerId};
use layerstack_schemas::{
    Scene, SchemaEdit,
    subset::{SubsetAuthoringError, SubsetFamilyType, SubsetProblemKind},
    usd_geom::{GeomSubsetEdit, GeomSubsetElementType as Element, Imageable, ImageableEdit},
};
const MESH: &str = r#"#usda 1.0
def Mesh "M" {
 point3f[] points = [(0,0,0),(1,0,0),(0,1,0),(1,1,0)]
 int[] faceVertexCounts = [3,3]
 int[] faceVertexIndices = [0,1,2,1,3,2]
 def "faces" {}
}
"#;
#[test]
fn unique_subsets_validate_membership_and_publish_a_valid_partition() {
    let (mut store, mut live) = support::scene(MESH);
    let mesh = store.path("/M");
    let expected_paths = [store.path("/M/faces_1"), store.path("/M/faces_2")];
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let first =
        GeomSubsetEdit::create_unique(&mut edit, mesh, "faces", Element::Face, &[0], "f").unwrap();
    let second =
        GeomSubsetEdit::create_unique(&mut edit, mesh, "faces", Element::Face, &[1], "f").unwrap();
    assert_eq!([first.path(), second.path()], expected_paths);
    ImageableEdit::new(&edit, mesh)
        .expect("outside any instance")
        .unwrap()
        .set_subset_family_type(&mut edit, "f", SubsetFamilyType::Partition)
        .unwrap();
    let count = edit.transaction().len();
    assert_eq!(
        GeomSubsetEdit::create_unique(&mut edit, mesh, "bad", Element::Face, &[2], "f"),
        Err(SubsetAuthoringError::Indices(
            SubsetProblemKind::InvalidIndex(2)
        ))
    );
    assert_eq!(
        GeomSubsetEdit::create_unique(&mut edit, mesh, "nested/name", Element::Face, &[0], "f"),
        Err(SubsetAuthoringError::InvalidName)
    );
    assert_eq!(
        edit.transaction().len(),
        count,
        "failed child groups retain prior edits"
    );
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let geom = Imageable::new(&scene, mesh).unwrap();
    assert!(
        geom.try_validate_subset_family(&Element::Face, "f")
            .unwrap()
            .is_valid()
    );
    assert_eq!(geom.subset_family_type("f"), "partition");
    let family = store.property_path("/M.subsetFamily:f:familyType");
    assert_eq!(
        live.view(&store)
            .property(family)
            .and_then(|property| property.declaration())
            .unwrap()
            .variability,
        layerstack::Variability::Uniform
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert!(
        expected_paths
            .iter()
            .all(|&path| live.stage().prim(path, &store).is_none())
    );
}

#[test]
fn rolled_back_subset_groups_release_names_and_family_overlays() {
    let (mut store, mut live) = support::scene(MESH);
    let mesh = store.path("/M");
    let expected = store.path("/M/new");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let cancelled: Result<(), &str> = edit.group(|child| {
        GeomSubsetEdit::create_unique(child, mesh, "new", Element::Face, &[0], "f").unwrap();
        ImageableEdit::new(child, mesh)
            .expect("outside any instance")
            .unwrap()
            .set_subset_family_type(child, "f", SubsetFamilyType::Partition)
            .unwrap();
        Err("cancelled")
    });
    assert_eq!(cancelled, Err("cancelled"));
    assert!(
        edit.transaction().is_empty(),
        "no partial declarations survive cancellation"
    );
    let subset =
        GeomSubsetEdit::create_unique(&mut edit, mesh, "new", Element::Face, &[1, 1], "f").unwrap();
    assert_eq!(
        subset.path(),
        expected,
        "interned names alone do not reserve prims"
    );
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let geom = Imageable::new(&scene, mesh).unwrap();
    assert_eq!(geom.subset_family_type("f"), "unrestricted");
    assert!(
        geom.try_validate_subset_family(&Element::Face, "f")
            .unwrap()
            .is_valid(),
        "unrestricted duplicate indices remain legal"
    );
}

#[test]
fn incompatible_family_declarations_reject_without_touching_prior_edits() {
    for declaration in [
        "float subsetFamily:f:familyType = 2",
        "token[] subsetFamily:f:familyType = []",
        "rel subsetFamily:f:familyType",
    ] {
        let source = MESH.replace(
            " def \"faces\" {}",
            &format!(" {declaration}\n def \"faces\" {{}}"),
        );
        let (mut store, mut live) = support::scene(&source);
        let mesh = store.path("/M");
        let family = store.tokens.lookup("subsetFamily:f:familyType").unwrap();
        let original = store.layer(LayerId(1)).unwrap().prims()[&mesh]
            .property(family)
            .unwrap()
            .clone();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let subset =
            GeomSubsetEdit::create_unique(&mut edit, mesh, "kept", Element::Face, &[0], "f")
                .unwrap();
        let count = edit.transaction().len();
        assert_eq!(
            ImageableEdit::new(&edit, mesh)
                .expect("outside any instance")
                .unwrap()
                .set_subset_family_type(&mut edit, "f", SubsetFamilyType::Partition),
            Err(SubsetAuthoringError::IncompatibleFamilyDeclaration)
        );
        assert_eq!(
            edit.transaction().len(),
            count,
            "{declaration}: no partial family declaration queued"
        );
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        assert!(
            live.stage().prim(subset.path(), &store).is_some(),
            "prior subset edits survive failed family authoring"
        );
        assert_eq!(
            store.layer(LayerId(1)).unwrap().prims()[&mesh]
                .property(family)
                .unwrap(),
            &original,
            "{declaration}: original declaration preserved"
        );
    }
}

#[test]
fn native_openusd_agrees_on_unique_subset_names() {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !std::process::Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output()
        .is_ok_and(|out| out.status.success())
    {
        return;
    }
    let code = r#"
import json
from pxr import Usd,UsdGeom
s=Usd.Stage.CreateInMemory()
m=UsdGeom.Mesh.Define(s,'/M')
s.DefinePrim('/M/faces')
print(json.dumps([str(UsdGeom.Subset.CreateUniqueGeomSubset(m,'faces',UsdGeom.Tokens.face,[i],'f').GetPath()) for i in range(2)]))
"#;
    let out = std::process::Command::new(python)
        .args(["-c", code])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let actual: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(actual, serde_json::json!(["/M/faces_1", "/M/faces_2"]));
}
