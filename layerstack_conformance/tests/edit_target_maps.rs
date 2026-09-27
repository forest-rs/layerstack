// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Edit targets map spec paths and relationship targets as OpenUSD's do.
//!
//! The scene: `/Inst` inherits `/Class`, `/Spec` specializes `/Base`,
//! `/Rock` has a local variant `{look=a}`, and `/Ref` references `/Asset`
//! in another layer, which has a variant `{v=x}`. For each case,
//! `scripts/edit_target_maps_oracle.py` records in
//! `fixtures/edit_target_maps/oracle.json` what OpenUSD 26.08's edit
//! target of that node (`Usd.EditTarget(layer, node)`, or
//! `ForLocalDirectVariant`) does:
//!
//! - `MapToSpecPath` of stage paths inside the arc's namespace, outside it,
//!   and in the namespace its target root already maps (`/Class/X`);
//! - what `SetTargets` authors for each target alone, or that it rejects
//!   it, and what the stage then reads.
//!
//! Layerstack's `EditTarget::for_node` (and `for_local_variant`) must
//! agree: a variant, inherit or specialize arc from the root keeps paths
//! outside its namespace (the root identity), a reference does not, and a
//! path whose image a more specific pair already owns does not map. The
//! authored scene, every accepted target of the root-layer cases through
//! one transaction, is `authored.usda`, which OpenUSD composes the same;
//! its undo and redo are exact.
//!
//! Spec: AOUSD Core §8 (spec paths), §12.4 (targets). OpenUSD:
//! `UsdEditTarget`, `PcpMapFunction`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::edit::{EditError, EditTarget, Rejection, Transaction};
use layerstack::{
    ArcKind, HashMap, InMemoryStore, Layer, LayerId, ListOp, PathId, PrimSpec, PropertyPath,
    Reference, SpecComponent, SpecPath, Stage, StageOptions, TargetPath, VariantSetSpec,
    VariantSpec,
};
use serde_json::{Value as Json, json};

const SCENE: &str = include_str!("../fixtures/edit_target_maps/scene.usda");
const ASSET: &str = include_str!("../fixtures/edit_target_maps/asset.usda");
const AUTHORED: &str = include_str!("../fixtures/edit_target_maps/authored.usda");
const ORACLE: &str = include_str!("../fixtures/edit_target_maps/oracle.json");

const ROOT: LayerId = LayerId(1);
const ASSET_LAYER: LayerId = LayerId(2);

/// A prim spec with one variant set, selecting its one variant.
fn with_variant(store: &mut InMemoryStore, spec: PrimSpec, set: &str, variant: &str) -> PrimSpec {
    let (set, variant) = (store.tokens.intern(set), store.tokens.intern(variant));
    let mut spec = spec;
    let mut variants = HashMap::new();
    variants.insert(variant, VariantSpec::default());
    spec.variant_sets.insert(set, VariantSetSpec { variants });
    spec.variant_set_order.push(set);
    spec.variant_selections.insert(set, variant);
    spec
}

fn scene() -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let root = store.path("/");
    let asset_path = store.path("/Asset");
    let mut asset = Layer::new(ASSET_LAYER);
    let asset_name = store.tokens.intern("Asset");
    asset.insert_prim(root, PrimSpec::default().with_children(vec![asset_name]));
    let asset_spec = with_variant(&mut store, PrimSpec::def(), "v", "x");
    asset.insert_prim(asset_path, asset_spec);
    store.insert_layer(asset);

    let names = ["Class", "Base", "Inst", "Spec", "Rock", "Light", "Ref"];
    let children = names.map(|n| store.tokens.intern(n)).to_vec();
    let [class, base, inst, spec, rock, light, reference] =
        names.map(|n| store.path(&format!("/{n}")));
    let mut layer = Layer::new(ROOT);
    layer.insert_prim(root, PrimSpec::default().with_children(children));
    layer.insert_prim(class, PrimSpec::class());
    layer.insert_prim(base, PrimSpec::def());
    layer.insert_prim(inst, PrimSpec::def().with_inherit(class));
    layer.insert_prim(spec, PrimSpec::def().with_specialize(base));
    let rock_spec = with_variant(&mut store, PrimSpec::def(), "look", "a");
    layer.insert_prim(rock, rock_spec);
    layer.insert_prim(light, PrimSpec::def());
    layer.insert_prim(
        reference,
        PrimSpec::def().with_reference(Reference::with_asset(
            ASSET_LAYER,
            asset_path,
            "asset.usda",
        )),
    );
    store.insert_layer(layer);
    store
}

fn compose(store: &mut InMemoryStore) -> Stage {
    Stage::compose(store, ROOT, StageOptions::default())
}

/// The edit target of a case, as the oracle builds OpenUSD's.
fn edit_target(store: &mut InMemoryStore, stage: &Stage, prim: PathId, arc: &str) -> EditTarget {
    if arc == "local_direct_variant" {
        let variant = SpecPath::parse("/Rock{look=a}", &mut store.tokens, &mut store.paths)
            .expect("a variant path");
        return EditTarget::for_local_variant(ROOT, &variant);
    }
    let kind = match arc {
        "inherit" => ArcKind::Inherits,
        "specialize" => ArcKind::Specializes,
        "variant" => ArcKind::Variants,
        other => panic!("no arc {other}"),
    };
    let graph = stage.explain_prim_graph(prim).expect("a graph");
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| graph.node(*id).is_some_and(|n| n.arc_kind() == kind))
        .expect("a node of the arc");
    EditTarget::for_node(stage, prim, node).expect("an edit target")
}

/// The explicit targets authored on the property spec at `path`, which is
/// on a prim or directly inside one variant of it.
fn authored_targets(store: &InMemoryStore, layer: LayerId, path: &SpecPath) -> Option<Vec<String>> {
    let prim = &store.layers[&layer].prims[&path.prim_path()];
    let properties = match path.components().last() {
        Some(SpecComponent::VariantSelection { set, variant }) => {
            &prim.variant_sets[set].variants[variant].properties
        }
        _ => &prim.properties,
    };
    let name = path.property()?;
    let targets = properties
        .iter()
        .find(|entry| entry.name == name)?
        .spec
        .targets
        .clone()?;
    Some(
        targets
            .explicit?
            .iter()
            .map(|t| t.display(&store.paths, &store.tokens))
            .collect(),
    )
}

fn layers(store: &InMemoryStore) -> Vec<Layer> {
    vec![
        store.layers[&ROOT].clone(),
        store.layers[&ASSET_LAYER].clone(),
    ]
}

/// The scene the test edits is the one OpenUSD records.
#[test]
fn the_scene_is_the_fixture() {
    let store = scene();
    let save = |id| {
        layerstack_usda::save::save_usda(&store.layers[&id], &store.tokens, &store.paths)
            .expect("saves")
    };
    for (name, text, fixture) in [
        ("scene.usda", save(ROOT), SCENE),
        ("asset.usda", save(ASSET_LAYER), ASSET),
    ] {
        if text != fixture {
            let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
            std::fs::write(&out, &text).expect("writes");
            panic!(
                "the scene is no longer fixtures/edit_target_maps/{name}; the new layer is {}: \
                 copy it there and rerun scripts/edit_target_maps_oracle.py",
                out.display()
            );
        }
    }
}

/// Every case maps spec paths and targets as OpenUSD's edit target does,
/// and each authored target undoes exactly.
#[test]
fn edit_targets_map_as_openusd_does() {
    let oracle: Json = serde_json::from_str(ORACLE).expect("the oracle parses");
    let cases: BTreeMap<String, Json> =
        serde_json::from_value(oracle["cases"].clone()).expect("cases");
    assert_eq!(cases.len(), 5);
    let mut failures = Vec::new();
    for (name, case) in &cases {
        let mut store = scene();
        let stage = compose(&mut store);
        let prim_text = match name.as_str() {
            "inherit" => "/Inst",
            "specialize" => "/Spec",
            "variant" | "local_direct_variant" => "/Rock",
            _ => "/Ref",
        };
        let arc = match name.as_str() {
            "variant_under_reference" => "variant",
            other => other,
        };
        let prim = store.path(prim_text);
        let target = edit_target(&mut store, &stage, prim, arc);

        for (path, expected) in case["spec_paths"].as_object().expect("spec paths") {
            let got = target
                .map_to_spec_path(store.path(path), &mut store.paths)
                .map(|spec| spec.display(&store.tokens));
            if json!(got) != *expected {
                failures.push(format!(
                    "{name}: MapToSpecPath({path}) is {got:?}, OpenUSD {expected}"
                ));
            }
        }

        let look = store.tokens.intern("look");
        let at = PropertyPath::new(prim, look);
        let spec = target
            .map_property_to_spec_path(at, &mut store.paths)
            .expect("the prim maps");
        for (path, expected) in case["targets"].as_object().expect("targets") {
            let wanted = TargetPath::parse(path, &mut store.tokens, &mut store.paths)
                .expect("a target path");
            let before = layers(&store);
            let mut edit = Transaction::new();
            edit.set_targets(target.property(at), ListOp::explicit(vec![wanted]));
            let got = match edit.apply(&mut store) {
                Ok(undo) => {
                    let authored = authored_targets(&store, target.layer(), &spec);
                    let reads: Vec<String> = compose(&mut store)
                        .resolve_target_list_path(at)
                        .map(|r| r.value)
                        .unwrap_or_default()
                        .iter()
                        .map(|t| t.display(&store.paths, &store.tokens))
                        .collect();
                    let after = layers(&store);
                    let redo = undo.apply(&mut store).expect("undoes");
                    if layers(&store) != before {
                        failures.push(format!("{name}: undoing {path} does not restore"));
                    }
                    let undo = redo.apply(&mut store).expect("redoes");
                    if layers(&store) != after {
                        failures.push(format!("{name}: redoing {path} differs"));
                    }
                    undo.apply(&mut store).expect("undoes again");
                    json!({"authored": authored, "reads": reads})
                }
                Err(EditError::Rejected {
                    reason: Rejection::UnmappableTarget(t),
                    ..
                }) if t == wanted => Json::Null,
                Err(e) => json!(format!("error: {e}")),
            };
            if got != *expected {
                failures.push(format!(
                    "{name}: target {path} gives {got}, OpenUSD {expected}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A local variant reached as a node of the prim's graph maps as the
/// local variant target does.
#[test]
fn a_local_variant_node_is_the_local_variant_target() {
    let mut store = scene();
    let stage = compose(&mut store);
    let rock = store.path("/Rock");
    let node = edit_target(&mut store, &stage, rock, "variant");
    let local = edit_target(&mut store, &stage, rock, "local_direct_variant");
    for path in ["/Rock", "/Rock/Pebble", "/Light", "/Class/X"] {
        let path = store.path(path);
        assert_eq!(
            node.map_to_spec_path(path, &mut store.paths),
            local.map_to_spec_path(path, &mut store.paths)
        );
    }
}

/// Every accepted target of the root-layer cases, authored through one
/// transaction, saves as `authored.usda`, which OpenUSD composes to the
/// targets the stage reads; undo and redo are exact.
#[test]
fn authored_targets_compose_as_openusd_composes_them() {
    let oracle: Json = serde_json::from_str(ORACLE).expect("the oracle parses");
    let mut store = scene();
    let stage = compose(&mut store);
    let look = store.tokens.intern("look");
    let mut edit = Transaction::new();
    let mut prims = Vec::new();
    for (name, prim_text, arc) in [
        ("inherit", "/Inst", "inherit"),
        ("specialize", "/Spec", "specialize"),
        ("variant", "/Rock", "variant"),
    ] {
        let prim = store.path(prim_text);
        let target = edit_target(&mut store, &stage, prim, arc);
        let accepted: Vec<TargetPath> = oracle["cases"][name]["targets"]
            .as_object()
            .expect("targets")
            .iter()
            .filter(|(_, outcome)| !outcome.is_null())
            .map(|(path, _)| {
                TargetPath::parse(path, &mut store.tokens, &mut store.paths).expect("a path")
            })
            .collect();
        edit.set_targets(
            target.property(PropertyPath::new(prim, look)),
            ListOp::explicit(accepted),
        );
        prims.push((prim_text, prim));
    }
    let before = layers(&store);
    let undo = edit.apply(&mut store).expect("every accepted target maps");
    let text = layerstack_usda::save::save_usda(&store.layers[&ROOT], &store.tokens, &store.paths)
        .expect("saves");
    if text != AUTHORED {
        let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("edit_target_maps.usda");
        std::fs::write(&out, &text).expect("writes");
        panic!(
            "the edits no longer author fixtures/edit_target_maps/authored.usda; the new layer is \
             {}: copy it there and rerun scripts/edit_target_maps_oracle.py",
            out.display()
        );
    }
    let recomposed = compose(&mut store);
    for (text, prim) in prims {
        let reads: Vec<String> = recomposed
            .resolve_target_list_path(PropertyPath::new(prim, look))
            .map(|r| r.value)
            .unwrap_or_default()
            .iter()
            .map(|t| t.display(&store.paths, &store.tokens))
            .collect();
        assert_eq!(json!(reads), oracle["composed"][text], "{text}.look");
    }
    let after = layers(&store);
    let redo = undo.apply(&mut store).expect("undoes");
    assert_eq!(layers(&store), before);
    redo.apply(&mut store).expect("redoes");
    assert_eq!(layers(&store), after);
}
