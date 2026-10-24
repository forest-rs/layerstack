// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `SchemaEdit::define_prim` against OpenUSD's `UsdStage::DefinePrim`;
//! `layerstack_conformance/tests/define_prim.rs` checks it call by call
//! against OpenUSD itself.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, Reference, Specifier, StageOptions,
};

use super::{CannotDefine, EditTarget, PathId, SchemaEdit};

const ROOT: LayerId = LayerId(1);

/// A stage over one layer, built by `build`.
fn stage(build: impl FnOnce(&mut InMemoryStore, &mut Layer)) -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(ROOT);
    build(&mut store, &mut layer);
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(crate::openusd(&store.tokens))),
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
        .map(|ty| String::from(store.tokens.resolve(ty)));
    Some((spec.specifier, type_name))
}

#[test]
fn undefined_ancestors_are_defined_first() {
    let (mut store, mut live) = stage(|store, layer| {
        let g = store.path("/G");
        layer.define_prim(g, PrimSpec::over(), &store.paths);
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
        layer.define_prim(over, PrimSpec::over(), &store.paths);
        let xform = store.tokens.intern("Xform");
        let same = store.path("/Same");
        layer.define_prim(same, PrimSpec::def().with_type_name(xform), &store.paths);
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
        layer.define_prim(prototype, PrimSpec::def(), &store.paths);
        layer.define_prim(child, PrimSpec::def(), &store.paths);
        let spec = PrimSpec::def()
            .with_instanceable(true)
            .with_reference(Reference::new(ROOT, prototype));
        layer.define_prim(instance, spec, &store.paths);
    });
    let instance = store.path("/Instance");
    let proxy = store.path("/Instance/Child");
    let beneath = store.path("/Instance/New/Leaf");
    let prototype = store.path("/__Prototype_1/Child");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    for path in [proxy, beneath] {
        assert_eq!(
            edit.define_prim(path, "Xform").err(),
            Some(CannotDefine::BeneathInstance { instance })
        );
    }
    assert_eq!(
        edit.define_prim(prototype, "Xform").err(),
        Some(CannotDefine::InPrototype)
    );
    assert!(edit.transaction().is_empty(), "a refusal authors nothing");
    edit.define_prim(instance, "Scope")
        .expect("the instance itself may be retyped");
    assert!(!edit.transaction().is_empty());
}
