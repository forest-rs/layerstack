// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `SchemaEdit::define_prim`, `override_prim` and `create_class_prim`
//! against OpenUSD's `UsdStage::DefinePrim`, `OverridePrim` and
//! `CreateClassPrim`; `layerstack_conformance/tests/conformance/define_prim.rs`
//! checks them call by call
//! against OpenUSD itself.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, Reference, Specifier, StageOptions,
};

use super::{CannotEdit, EditTarget, PathId, SchemaEdit};

const ROOT: LayerId = LayerId(1);

/// A stage over one layer, built by `build`.
fn stage(build: impl FnOnce(&mut InMemoryStore, &mut Layer)) -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(ROOT);
    build(&mut store, &mut layer);
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(crate::openusd(store.tokens()))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, ROOT, options);
    (store, live)
}

/// Defines each `(path, schema)` in one edit and applies it; the number
/// of operations it collected.
fn define(store: &mut InMemoryStore, live: &mut LiveStage, prims: &[(&str, &str)]) -> usize {
    let paths: Vec<PathId> = prims.iter().map(|(path, _)| store.path(path)).collect();
    let mut edit = SchemaEdit::new(live.stage(), store, EditTarget::for_layer(ROOT));
    for (&path, (_, schema)) in paths.iter().zip(prims) {
        edit.define_prim(path, schema).expect("definable");
    }
    let transaction = edit.finish();
    live.apply(store, &transaction).expect("applies");
    transaction.len()
}

/// The root layer's spec at `path`: its specifier and type name.
fn spec(store: &mut InMemoryStore, path: &str) -> Option<(Option<Specifier>, Option<String>)> {
    let path = store.path(path);
    let spec = store.layer(ROOT)?.prim(path)?;
    let type_name = spec
        .type_name
        .map(|ty| String::from(store.tokens().resolve(ty)));
    Some((spec.specifier, type_name))
}

#[test]
fn undefined_ancestors_are_defined_first() {
    let (mut store, mut live) = stage(|store, layer| {
        let g = store.path("/G");
        layer.define_prim(g, PrimSpec::over(), store.paths());
    });
    define(&mut store, &mut live, &[("/G/H/I", "Xform")]);
    assert_eq!(spec(&mut store, "/G"), Some((Some(Specifier::Def), None)));
    assert_eq!(spec(&mut store, "/G/H"), Some((Some(Specifier::Def), None)));
    assert_eq!(
        spec(&mut store, "/G/H/I"),
        Some((Some(Specifier::Def), Some("Xform".into())))
    );
    let leaf = store.path("/G/H/I");
    assert!(
        live.stage()
            .prim(leaf, &store)
            .is_some_and(layerstack::Prim::is_defined),
        "the prim is defined, its ancestors too"
    );
}

#[test]
fn an_over_is_upgraded_and_a_defined_prim_left_alone() {
    let (mut store, mut live) = stage(|store, layer| {
        let over = store.path("/Over");
        layer.define_prim(over, PrimSpec::over(), store.paths());
        let xform = store.tokens().intern("Xform");
        let same = store.path("/Same");
        layer.define_prim(same, PrimSpec::def().with_type_name(xform), store.paths());
    });
    define(&mut store, &mut live, &[("/Over", "Xform")]);
    assert_eq!(
        spec(&mut store, "/Over"),
        Some((Some(Specifier::Def), Some("Xform".into())))
    );
    assert_eq!(
        define(&mut store, &mut live, &[("/Same", "Xform"), ("/Same", "")]),
        0,
        "already defined as the schema"
    );
    define(&mut store, &mut live, &[("/Same", "Scope")]);
    assert_eq!(
        spec(&mut store, "/Same"),
        Some((Some(Specifier::Def), Some("Scope".into())))
    );
}

#[test]
fn prims_defined_earlier_in_the_edit_count_as_defined() {
    let (mut store, mut live) = stage(|_, _| {});
    // `/Seq` gets `def` once, as `/Seq/A`'s ancestor; the second `/Seq/A`
    // is already an `Xform`; the last retypes it.
    let ops = define(
        &mut store,
        &mut live,
        &[
            ("/Seq/A", "Xform"),
            ("/Seq/A/B", ""),
            ("/Seq/A", "Xform"),
            ("/Seq", ""),
        ],
    );
    assert_eq!(
        ops, 4,
        "`/Seq`'s specifier, `/Seq/A`'s and its type, `/Seq/A/B`'s"
    );
    assert_eq!(spec(&mut store, "/Seq"), Some((Some(Specifier::Def), None)));
    assert_eq!(
        spec(&mut store, "/Seq/A/B"),
        Some((Some(Specifier::Def), None))
    );
}

#[test]
fn the_pseudo_root_is_never_authored() {
    let (mut store, mut live) = stage(|_, _| {});
    assert_eq!(define(&mut store, &mut live, &[("/", "Xform")]), 0);
}

#[test]
fn a_path_beneath_an_instance_or_in_a_prototype_is_refused() {
    let (mut store, live) = stage(|store, layer| {
        let prototype = store.path("/Prototype");
        let child = store.path("/Prototype/Child");
        let instance = store.path("/Instance");
        layer.define_prim(prototype, PrimSpec::def(), store.paths());
        layer.define_prim(child, PrimSpec::def(), store.paths());
        let spec = PrimSpec::def()
            .with_instanceable(true)
            .with_reference(Reference::new(ROOT, prototype));
        layer.define_prim(instance, spec, store.paths());
    });
    let instance = store.path("/Instance");
    let proxy = store.path("/Instance/Child");
    let beneath = store.path("/Instance/New/Leaf");
    let prototype = store.path("/__Prototype_1/Child");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    for path in [proxy, beneath] {
        assert_eq!(
            edit.define_prim(path, "Xform").err(),
            Some(CannotEdit::BeneathInstance { instance })
        );
    }
    assert_eq!(
        edit.define_prim(prototype, "Xform").err(),
        Some(CannotEdit::InPrototype)
    );
    assert!(edit.transaction().is_empty(), "a refusal authors nothing");
    edit.define_prim(instance, "Scope")
        .expect("the instance itself may be retyped");
    assert!(!edit.transaction().is_empty());
}

/// Every route that names a prim by path refuses an instance proxy and a
/// prototype prim through a local layer, before it authors anything; the
/// instance itself and a target that maps the proxy elsewhere are allowed.
#[cfg(feature = "usd-lux")]
#[test]
fn handles_and_applied_schemas_refuse_instance_proxies() {
    use layerstack::CannotApply;
    use layerstack::prim_index::ArcKind;

    use crate::usd_geom::{Xform, XformEdit};
    use crate::usd_lux::ShapingApi;
    use crate::{ApplySchemaError, PrimEdit};

    let (mut store, live) = stage(|store, layer| {
        let prototype = store.path("/Prototype");
        let child = store.path("/Prototype/Child");
        let instance = store.path("/Instance");
        layer.define_prim(prototype, PrimSpec::def(), store.paths());
        layer.define_prim(
            child,
            PrimSpec::def().with_type_name(store.tokens().intern("Xform")),
            store.paths(),
        );
        let spec = PrimSpec::def()
            .with_instanceable(true)
            .with_reference(Reference::new(ROOT, prototype));
        layer.define_prim(instance, spec, store.paths());
    });
    let instance = store.path("/Instance");
    let proxy = store.path("/Instance/Child");
    let missing = store.path("/Instance/New");
    let prototype = store.path("/__Prototype_1/Child");
    let nowhere = store.path("/Nowhere");
    let beneath = CannotEdit::BeneathInstance { instance };

    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    for (path, refusal) in [
        (proxy, beneath),
        (missing, beneath),
        (prototype, CannotEdit::InPrototype),
    ] {
        assert_eq!(edit.can_edit(path), Err(refusal));
        assert_eq!(PrimEdit::new(&edit, path), Err(refusal));
        assert_eq!(XformEdit::new(&edit, path), Err(refusal));
        assert_eq!(
            ShapingApi::apply(&mut edit, path).err(),
            Some(ApplySchemaError::CannotEdit(refusal))
        );
        assert_eq!(
            edit.apply_api(path, "NoSuchAPI", None),
            Err(ApplySchemaError::CannotEdit(refusal)),
            "checked before the schema"
        );
    }
    assert!(edit.transaction().is_empty(), "a refusal authors nothing");
    assert_eq!(
        edit.apply_api(nowhere, "ShapingAPI", None),
        Err(ApplySchemaError::CannotApply(CannotApply::NoSuchPrim))
    );
    assert_eq!(edit.can_edit(instance), Ok(()));
    PrimEdit::new(&edit, instance)
        .expect("the instance itself may be edited")
        .expect("a prim");
    ShapingApi::apply(&mut edit, instance).expect("the instance itself may be edited");
    assert!(!edit.transaction().is_empty());
    drop(edit);

    // Through the reference's node, the proxy maps to `/Prototype/Child`.
    let graph = live
        .stage()
        .prim(instance, &store)
        .expect("the instance")
        .composition_graph();
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| {
            graph
                .node(*id)
                .is_some_and(|node| node.arc_kind() == ArcKind::References)
        })
        .expect("the reference node");
    let target = EditTarget::for_node(live.stage(), instance, node).expect("an edit target");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    assert_eq!(edit.can_edit(proxy), Ok(()));
    XformEdit::new(&edit, proxy)
        .expect("a target that maps the proxy elsewhere may edit it")
        .expect("an Xform")
        .set_visibility(&mut edit, crate::usd_geom::ImageableVisibility::Invisible);
    Xform::define(&mut edit, missing).expect("a new prim through the reference");
    assert!(!edit.transaction().is_empty());
}

#[test]
fn appended_generic_edits_keep_their_place_in_one_transaction() {
    let (mut store, mut live) = stage(|_, _| {});
    let rock = store.path("/Rock");
    let lamp = store.path("/Lamp");
    let mut generic = layerstack::Transaction::new();
    generic.create_prim(
        EditTarget::for_layer(ROOT).prim(lamp),
        Specifier::Over,
        None,
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    edit.define_prim(rock, "Xform").expect("definable");
    let defined = edit.transaction().len();
    edit.append(generic);
    assert_eq!(edit.transaction().len(), defined + 1, "after the define");
    // The handles still see what this edit defined, not what it appended.
    assert!(edit.is_a(rock, "Xform"));
    assert!(
        edit.prim_path("/Lamp")
            .is_some_and(|lamp| !edit.is_a(lamp, "Xform"))
    );
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).expect("applies");
    assert_eq!(
        spec(&mut store, "/Rock"),
        Some((Some(Specifier::Def), Some("Xform".into())))
    );
    assert_eq!(
        spec(&mut store, "/Lamp"),
        Some((Some(Specifier::Over), None))
    );
}

/// `CreateClassPrim` makes classes only in the local layer stack, or
/// through an arc's target: a layer outside the stack that maps every path
/// to itself is refused, before anything else is checked.
#[test]
fn classes_are_created_in_the_local_layer_stack() {
    use super::ClassPrimError;

    let (mut store, live) = stage(|_, _| {});
    let outside = LayerId(2);
    store.insert_layer(Layer::new(outside));
    let base = store.path("/Base");
    let root = store.path("/");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(outside));
    assert_eq!(
        edit.create_class_prim(base).map(|_| ()),
        Err(ClassPrimError::NotLocal)
    );
    assert!(edit.transaction().is_empty(), "nothing is authored");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    assert_eq!(
        edit.create_class_prim(root).map(|_| ()),
        Err(ClassPrimError::DefinedNotClass),
        "the pseudo-root is defined"
    );
    edit.create_class_prim(base).expect("a new class");
    assert_eq!(
        edit.override_prim(base).map(|_| ()),
        Ok(()),
        "the class is there"
    );
    assert_eq!(edit.transaction().len(), 2, "`def`, then `class`");
}
