// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! ID edits retain list strength, source identity and array ownership.
use crate::support::schema_scene as support;
use layerstack::{EditTarget, FieldValue, Layer, LayerId, ListOp, PrimSpec, Value};
use layerstack_schemas::{
    Scene, SchemaEdit, Time,
    usd_geom::{PointInstancer, PointInstancerEdit},
};
const INSTANCER: &str = r#"#usda 1.0
def PointInstancer "P" {
 int64[] ids = [10,20,30,40,50,60,90,99]
 int[] protoIndices = [0,0,0,0,0,0,0,0]
 int64[] invisibleIds = []
 int64[] invisibleIds.timeSamples = {1:[99]}
}
"#;

fn authored_id_sequence() -> (layerstack::InMemoryStore, layerstack::LiveStage) {
    let (mut store, mut live) = support::scene(INSTANCER);
    let path = store.path("/P");
    let inactive = store.tokens.intern("inactiveIds");
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(
        path,
        PrimSpec::over().with_field(
            inactive,
            FieldValue::Int64ListOp(Box::new(ListOp::appended(vec![10, 20]))),
        ),
    );
    store.insert_layer(weak);
    let root = store.layer_mut(LayerId(1)).unwrap();
    root.sublayers_mut()
        .push(layerstack::SublayerEntry::new(LayerId(2)));
    root.prim_mut(path).unwrap().set_field(
        inactive,
        FieldValue::Int64ListOp(Box::new(ListOp {
            add: vec![30, 10],
            delete: vec![20],
            ..ListOp::default()
        })),
    );
    live.synchronize(&mut store);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let point = PointInstancerEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    point.deactivate_ids(&mut edit, &[40, 10, 40]).unwrap();
    point.activate_ids(&mut edit, &[30, 90]).unwrap();
    point.make_invisible(&mut edit, &[20, 40, 20]).unwrap();
    point.make_visible(&mut edit, &[40]).unwrap();
    let cancelled: Result<(), &str> = edit.group(|child| {
        point.deactivate_ids(child, &[99]).unwrap();
        point.make_all_visible(child);
        Err("cancelled")
    });
    assert_eq!(
        cancelled,
        Err("cancelled"),
        "the nested ID edit was cancelled"
    );
    point.deactivate_ids(&mut edit, &[50]).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(
        path,
        PrimSpec::over().with_field(
            inactive,
            FieldValue::Int64ListOp(Box::new(ListOp::appended(vec![10, 20, 60, 90]))),
        ),
    );
    store.insert_layer(weak);
    live.synchronize(&mut store);
    (store, live)
}

#[test]
fn id_edits_preserve_future_weaker_opinions_and_group_rollback() {
    let (mut store, mut live) = authored_id_sequence();
    let path = store.path("/P");
    let scene = Scene::new(live.stage(), &store);
    let point = PointInstancer::new(&scene, path).unwrap();
    assert_eq!(
        point.try_compute_mask(Time::Default).unwrap(),
        vec![false, false, true, false, false, false, true, true]
    );
    assert_eq!(point.invisible_ids().unwrap().as_slice(), &[20]);
    assert_eq!(
        point.invisible_ids_at(Time::held(1.)).unwrap().as_slice(),
        &[99],
        "default visibility helpers preserve numeric samples"
    );
    let inactive = store.tokens.lookup("inactiveIds").unwrap();
    let Some(FieldValue::Int64ListOp(op)) =
        store.layer(LayerId(1)).unwrap().prims()[&path].field(inactive)
    else {
        panic!("authored int64 list operation")
    };
    assert!(
        op.explicit.is_none(),
        "incremental edits must not freeze weaker values"
    );
    assert!(
        op.add.is_empty() && op.reorder.is_empty(),
        "legacy buckets are canonicalized"
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let point = PointInstancerEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    point
        .activate_all_ids(&mut edit)
        .make_all_visible(&mut edit);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    assert!(
        PointInstancer::new(&scene, path)
            .unwrap()
            .compute_mask(Time::Default)
            .is_empty()
    );
}

#[test]
fn explicit_inactive_lists_keep_their_cutoff_after_id_edits() {
    let (mut store, mut live) = support::scene(INSTANCER);
    let path = store.path("/P");
    let inactive = store.tokens.intern("inactiveIds");
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prim_mut(path)
        .unwrap()
        .set_field(
            inactive,
            FieldValue::Int64ListOp(Box::new(ListOp::explicit(vec![30, 10]))),
        );
    live.synchronize(&mut store);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let point = PointInstancerEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    point.deactivate_ids(&mut edit, &[20, 10]).unwrap();
    point.activate_ids(&mut edit, &[30]).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let Some(FieldValue::Int64ListOp(op)) =
        store.layer(LayerId(1)).unwrap().prims()[&path].field(inactive)
    else {
        panic!("inactive list")
    };
    assert_eq!(op.explicit.as_deref(), Some([20, 10].as_slice()));
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(
        path,
        PrimSpec::over().with_field(
            inactive,
            FieldValue::Int64ListOp(Box::new(ListOp::appended(vec![90]))),
        ),
    );
    store.insert_layer(weak);
    let root = store.layer_mut(LayerId(1)).unwrap();
    root.sublayers_mut()
        .push(layerstack::SublayerEntry::new(LayerId(2)));
    live.synchronize(&mut store);
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        PointInstancer::new(&scene, path)
            .unwrap()
            .compute_mask(Time::Default),
        vec![false, false, true, true, true, true, true, true]
    );
}

#[test]
fn variant_id_edits_read_only_the_mapped_target_site() {
    let source = INSTANCER.replace(
        "def PointInstancer \"P\" {",
        r#"def PointInstancer "P" (
 append inactiveIds = [20]
 prepend variantSets = "mode"
 variants = { string mode = "a" }
) {
 variantSet "mode" = {
  "a" (append inactiveIds = [10]) {}
 }
"#,
    );
    let (mut store, mut live) = support::scene(&source);
    let path = store.path("/P");
    let variant = layerstack::SpecPath::parse("/P{mode=a}", &store.tokens, &store.paths).unwrap();
    let target = EditTarget::for_local_variant(LayerId(1), &variant);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    let point = PointInstancerEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    point.deactivate_ids(&mut edit, &[30]).unwrap();
    point.activate_ids(&mut edit, &[10]).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let inactive = store.tokens.lookup("inactiveIds").unwrap();
    let branch = store
        .layer(LayerId(1))
        .unwrap()
        .variant_spec_at(&variant, &store.paths)
        .unwrap();
    let Some(FieldValue::Int64ListOp(op)) = layerstack::get_field(&branch.fields, &inactive) else {
        panic!("variant-local inactive list")
    };
    assert_eq!(
        op.apply_to(&[]),
        vec![30],
        "the base prim's ID20 is not copied into this variant"
    );
    assert_eq!(op.delete, vec![10]);
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        PointInstancer::new(&scene, path)
            .unwrap()
            .compute_mask(Time::Default),
        vec![true, false, false, true, true, true, true, true]
    );
}

#[derive(Debug)]
struct FailedIds(layerstack::ArrayReadError);
impl layerstack::DeferredArraySource for FailedIds {
    fn materialize(&self) -> Result<&layerstack::TypedArray, &layerstack::ArrayReadError> {
        Err(&self.0)
    }
    fn element_kind(&self) -> Value {
        Value::Int64(0)
    }
}

#[test]
fn visibility_edits_preserve_decode_errors_and_prior_collected_operations() {
    let (mut store, mut live) = support::scene(INSTANCER);
    let path = store.path("/P");
    let invisible = store.property_path("/P.invisibleIds");
    let error = layerstack::ArrayReadError::BudgetExceeded { limit: 17 };
    let mut corrupt = layerstack::Transaction::new();
    corrupt.set_default(
        EditTarget::for_layer(LayerId(1)).property(invisible),
        Value::TypedArray(layerstack::TypedArray::Deferred(std::sync::Arc::new(
            FailedIds(error.clone()),
        ))),
    );
    live.apply(&mut store, &corrupt).unwrap();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let point = PointInstancerEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    point.deactivate_ids(&mut edit, &[10]).unwrap();
    let count = edit.transaction().len();
    assert_eq!(
        point.make_invisible(&mut edit, &[20]),
        Err(layerstack_schemas::point_instancer::InstanceIdEditError::Decode(error.clone()))
    );
    assert_eq!(
        point.make_visible(&mut edit, &[20]),
        Err(layerstack_schemas::point_instancer::InstanceIdEditError::Decode(error))
    );
    assert_eq!(
        edit.transaction().len(),
        count,
        "failed visibility reads leave preceding edits intact"
    );
    point.make_all_visible(&mut edit);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let point = PointInstancer::new(&scene, path).unwrap();
    assert_eq!(
        point.compute_mask(Time::Default),
        vec![false, true, true, true, true, true, true, true]
    );
}

#[test]
fn native_openusd_agrees_on_effective_id_masks() {
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
from pxr import Usd,UsdGeom,Sdf
s=Usd.Stage.CreateInMemory()
weak=Sdf.Layer.CreateAnonymous()
wp=Sdf.CreatePrimInLayer(weak,'/P')
wp.SetInfo('inactiveIds',Sdf.Int64ListOp.Create(appendedItems=[10,20]))
s.GetRootLayer().subLayerPaths=[weak.identifier]
p=UsdGeom.PointInstancer.Define(s,'/P')
p.CreateIdsAttr([10,20,30,40,50,60,90,99])
op=Sdf.Int64ListOp();op.addedItems=[30,10];op.deletedItems=[20]
s.GetRootLayer().GetPrimAtPath('/P').SetInfo('inactiveIds',op)
p.DeactivateIds([40,10,40]);p.ActivateIds([30,90]);p.InvisIds([20,40,20],Usd.TimeCode.Default());p.VisIds([40],Usd.TimeCode.Default());p.DeactivateIds([50])
wp.SetInfo('inactiveIds',Sdf.Int64ListOp.Create(appendedItems=[10,20,60,90]))
print(json.dumps({'mask':p.ComputeMaskAtTime(Usd.TimeCode.Default()),'invisible':list(p.GetInvisibleIdsAttr().Get())}))
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
    assert_eq!(
        actual,
        serde_json::json!({"mask":[false,false,true,false,false,false,true,true],"invisible":[20]})
    );
}

#[test]
fn mask_compaction_preserves_groups_ownership_and_failed_inputs() {
    #[derive(Debug, PartialEq, Eq)]
    struct Owned(String);
    let mut data: Vec<_> = ["a0", "a1", "b0", "b1", "c0", "c1"]
        .into_iter()
        .map(|value| Owned(value.into()))
        .collect();
    let capacity = data.capacity();
    let size = std::num::NonZeroUsize::new(2).unwrap();
    PointInstancer::apply_mask_to_array(&[true, false, true], &mut data, size).unwrap();
    assert_eq!(
        data.iter()
            .map(|value| value.0.as_str())
            .collect::<Vec<_>>(),
        vec!["a0", "a1", "c0", "c1"]
    );
    assert_eq!(
        data.capacity(),
        capacity,
        "compaction retains the allocation"
    );
    let before: Vec<_> = data.iter().map(|value| value.0.clone()).collect();
    assert!(PointInstancer::apply_mask_to_array(&[true], &mut data, size).is_err());
    assert_eq!(
        data.iter().map(|value| value.0.clone()).collect::<Vec<_>>(),
        before
    );
    PointInstancer::apply_mask_to_array(&[], &mut data, size).unwrap();
    assert_eq!(data.len(), 4, "an empty computed mask means no pruning");
    PointInstancer::apply_mask_to_array(&[false, false], &mut data, size).unwrap();
    assert!(data.is_empty());
}
