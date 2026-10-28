// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Preparation from a live stage's retained evidence finds the same edits,
//! and the same errors, as composing and visiting the whole stage.
//!
//! Each scene is composed as live stages, then every prim and property is
//! renamed, reparented, moved onto an occupied path or beneath itself, and
//! deleted, both ways; then some of the edits are applied and the scene is
//! prepared against again, and finally against a snapshot its layers have
//! moved on from.

use super::*;
use crate::{
    InMemoryStore, LiveStage, PrimSpec, PropertySpec, PropertyType, Reference, Relocate,
    StageOptions, SublayerEntry, VariantSetSpec, VariantSpec,
};
use alloc::{format, string::String, sync::Arc, vec, vec::Vec};

const ROOT: LayerId = LayerId(1);
const WEAK: LayerId = LayerId(2);
const ASSET: LayerId = LayerId(3);
const DEPENDENT: LayerId = LayerId(4);
const SESSION: LayerId = LayerId(5);

fn attribute(value: f64) -> PropertySpec {
    PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
        .with_default(Value::Double(value))
}

fn expression(text: &str) -> PropertySpec {
    PropertySpec::typed_attribute(PropertyType::parse("pathExpression").unwrap())
        .with_default(Value::PathExpression(Arc::from(text)))
}

/// Authors `spec` at `path` in `layer`, listed among its parent's
/// children as a layer read from USDA lists it.
fn author(store: &mut InMemoryStore, layer: &mut Layer, path: &str, spec: PrimSpec) -> PathId {
    let id = store.path(path);
    layer.define_prim(id, spec, &store.paths);
    id
}

fn targets(paths: Vec<TargetPath>) -> ListOp<TargetPath> {
    ListOp::explicit(paths)
}

/// A scene with every kind of authored path the edits repair: targets,
/// connections, absolute and relative expressions, internal references,
/// payloads, inherits, specializes and `defaultPrim`, over two layers of
/// the root layer stack.
fn paths_scene() -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let look = store.tokens.intern("look");
    let input = store.tokens.intern("inputs:value");
    let output = store.tokens.intern("outputs:value");
    let query = store.tokens.intern("expression");
    let relative = store.tokens.intern("relative");
    let mut root = Layer::new(ROOT);
    root.default_prim = Some(store.tokens.intern("World"));
    author(&mut store, &mut root, "/World", PrimSpec::def());
    author(
        &mut store,
        &mut root,
        "/World/A",
        PrimSpec::def()
            .with_property(output, attribute(1.0))
            .with_property(relative, expression("Child//")),
    );
    author(
        &mut store,
        &mut root,
        "/World/A/Child",
        PrimSpec::def().with_property(output, attribute(2.0)),
    );
    author(
        &mut store,
        &mut root,
        "/World/A/Child/Leaf",
        PrimSpec::def(),
    );
    author(&mut store, &mut root, "/World/B", PrimSpec::def());
    let class = author(&mut store, &mut root, "/Class", PrimSpec::class());
    author(&mut store, &mut root, "/Class/Part", PrimSpec::def());
    let a = store.path("/World/A");
    let child = store.path("/World/A/Child");
    let b = store.path("/World/B");
    author(
        &mut store,
        &mut root,
        "/World/C",
        PrimSpec::def().with_inherit(class).with_specialize(b),
    );
    author(
        &mut store,
        &mut root,
        "/Ref",
        PrimSpec::def().with_reference(Reference::new(ROOT, a)),
    );
    author(
        &mut store,
        &mut root,
        "/Load",
        PrimSpec::def().with_payload(Reference::new(ROOT, child)),
    );
    author(
        &mut store,
        &mut root,
        "/Watch",
        PrimSpec::def()
            .with_property(
                look,
                PropertySpec::relationship()
                    .with_targets(targets(vec![TargetPath::Prim(child), TargetPath::Prim(b)])),
            )
            .with_property(
                input,
                attribute(0.0).with_targets(targets(vec![TargetPath::Property(
                    PropertyPath::new(a, output),
                )])),
            )
            .with_property(query, expression("/World/A// /World/B")),
    );
    let mut weak = Layer::new(WEAK);
    author(
        &mut store,
        &mut weak,
        "/World/A",
        PrimSpec::over().with_property(output, attribute(3.0)),
    );
    author(&mut store, &mut weak, "/World/D", PrimSpec::def());
    root.sublayers.push(SublayerEntry::new(WEAK));
    store.insert_layer(weak);
    store.insert_layer(root);
    store
}

/// Root prims with two children each, as many root prims of a large
/// stage are, one of them targeting another's child. When `listed`, the
/// pseudo-root and each prim list their children, as a layer read from
/// USDA does; otherwise only their specs name them, as in a layer built
/// by `Layer::insert_prim`.
fn roots_scene(listed: bool) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let look = store.tokens.intern("look");
    let mut root = Layer::new(ROOT);
    for i in 0..6 {
        for name in [
            format!("/Rock{i}"),
            format!("/Rock{i}/Moss"),
            format!("/Rock{i}/Lichen"),
        ] {
            let path = store.path(&name);
            if listed {
                root.define_prim(path, PrimSpec::def(), &store.paths);
            } else {
                root.insert_prim(path, PrimSpec::def());
            }
        }
    }
    let rock = store.path("/Rock0");
    let moss = store.path("/Rock1/Moss");
    let spec = PrimSpec::def().with_property(
        look,
        PropertySpec::relationship().with_targets(targets(vec![TargetPath::Prim(moss)])),
    );
    if listed {
        root.define_prim(rock, spec, &store.paths);
    } else {
        root.insert_prim(rock, spec);
    }
    store.insert_layer(root);
    store
}

/// `/Tier` references the asset's `/Asset`, overriding its `Item` and,
/// when `relocated`, relocating its `Spare`; `/Second` references
/// `/Tier/Item`, and the dependent layer references the root layer's
/// `/Tier`.
fn arcs_scene(relocated: bool) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let size = store.tokens.intern("size");
    let look = store.tokens.intern("look");
    let mut asset = Layer::new(ASSET);
    let asset_root = author(&mut store, &mut asset, "/Asset", PrimSpec::def());
    author(
        &mut store,
        &mut asset,
        "/Asset/Item",
        PrimSpec::def().with_property(size, attribute(1.0)),
    );
    author(&mut store, &mut asset, "/Asset/Item/Deep", PrimSpec::def());
    author(&mut store, &mut asset, "/Asset/Spare", PrimSpec::def());
    store.insert_layer(asset);
    let mut root = Layer::new(ROOT);
    let tier = author(
        &mut store,
        &mut root,
        "/Tier",
        PrimSpec::def().with_reference(Reference::with_asset(ASSET, asset_root, "asset.usda")),
    );
    let item = author(
        &mut store,
        &mut root,
        "/Tier/Item",
        PrimSpec::over().with_property(size, attribute(2.0)),
    );
    author(
        &mut store,
        &mut root,
        "/Second",
        PrimSpec::def().with_reference(Reference::new(ROOT, item)),
    );
    author(&mut store, &mut root, "/Free", PrimSpec::def());
    if relocated {
        let spare = store.path("/Tier/Spare");
        let moved = store.path("/Tier/Moved");
        root.relocates_mut().push(Relocate {
            source: spare,
            target: Some(moved),
        });
    }
    store.insert_layer(root);
    let mut dependent = Layer::new(DEPENDENT);
    author(
        &mut store,
        &mut dependent,
        "/Use",
        PrimSpec::def().with_reference(Reference::with_asset(ROOT, tier, "root.usda")),
    );
    let used_item = store.path("/Use/Item");
    author(
        &mut store,
        &mut dependent,
        "/Use/Item",
        PrimSpec::over().with_property(size, attribute(3.0)),
    );
    author(
        &mut store,
        &mut dependent,
        "/Watch",
        PrimSpec::def().with_property(
            look,
            PropertySpec::relationship().with_targets(targets(vec![TargetPath::Prim(used_item)])),
        ),
    );
    store.insert_layer(dependent);
    store
}

/// Inactive, instanced and variant prims, which preparation rejects in
/// their own ways, beside plain ones it moves.
fn rejections_scene() -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let mut root = Layer::new(ROOT);
    author(&mut store, &mut root, "/World", PrimSpec::def());
    author(
        &mut store,
        &mut root,
        "/World/Off",
        PrimSpec::def().with_active(false),
    );
    author(&mut store, &mut root, "/World/Off/Under", PrimSpec::def());
    author(&mut store, &mut root, "/World/On", PrimSpec::def());
    let proto = author(&mut store, &mut root, "/Proto", PrimSpec::def());
    author(&mut store, &mut root, "/Proto/Part", PrimSpec::def());
    for name in ["/I1", "/I2"] {
        author(
            &mut store,
            &mut root,
            name,
            PrimSpec::def()
                .with_reference(Reference::new(ROOT, proto))
                .with_instanceable(true),
        );
    }
    store.insert_layer(root);
    store
}

fn variants_scene() -> InMemoryStore {
    let mut store = rejections_scene();
    let shape = store.tokens.intern("shape");
    let round = store.tokens.intern("round");
    let mut layer = store.layers[&ROOT].clone();
    let mut spec = PrimSpec::def();
    spec.rare_mut().variant_sets.insert(
        shape,
        VariantSetSpec {
            variants: [(round, VariantSpec::default())].into_iter().collect(),
        },
    );
    spec.rare_mut()
        .variant_set_names
        .make_mut()
        .prepend
        .push(shape);
    spec.rare_mut().variant_selections.insert(shape, round);
    author(&mut store, &mut layer, "/Varied", spec);
    store.insert_layer(layer);
    store
}

/// A scene's live stages: the root layer's, and the dependents'.
struct Scene {
    store: InMemoryStore,
    primary: LiveStage,
    dependents: Vec<LiveStage>,
}

impl Scene {
    fn new(mut store: InMemoryStore, options: StageOptions, dependents: &[LayerId]) -> Self {
        let primary = LiveStage::compose(&mut store, ROOT, options);
        let dependents = dependents
            .iter()
            .map(|id| LiveStage::compose(&mut store, *id, StageOptions::default()))
            .collect();
        Self {
            store,
            primary,
            dependents,
        }
    }

    fn synchronize(&mut self) {
        self.primary.synchronize(&mut self.store);
        for dependent in &mut self.dependents {
            dependent.synchronize(&mut self.store);
        }
    }

    fn name(&self, path: TargetPath) -> String {
        match path {
            TargetPath::Prim(p) => self.store.paths.display(p, &self.store.tokens),
            TargetPath::Property(p) => p.display(&self.store.paths, &self.store.tokens),
        }
    }

    /// Every layer, with its generations.
    fn generations(&self) -> Vec<(LayerId, u64, u64, Layer)> {
        let mut generations: Vec<_> = self
            .store
            .layers
            .values()
            .map(|l| (l.id, l.generation(), l.structural_generation(), l.clone()))
            .collect();
        generations.sort_unstable_by_key(|(id, ..)| *id);
        generations
    }

    /// Prepares the move both ways and checks they agree and change
    /// nothing; returns the edit.
    fn compare_move(
        &mut self,
        target: LayerId,
        from: TargetPath,
        to: TargetPath,
    ) -> Result<NamespaceEdit, NamespaceError> {
        let before = self.generations();
        let dependents: Vec<&Stage> = self.dependents.iter().map(LiveStage::stage).collect();
        let target_edit = EditTarget::for_layer(target);
        let old = coordinated::prepare_by_composition(
            self.primary.stage(),
            &dependents,
            &mut self.store,
            &target_edit,
            from,
            to,
        );
        let new = coordinated::prepare(
            self.primary.stage(),
            &dependents,
            &mut self.store,
            &target_edit,
            from,
            to,
        );
        assert_eq!(
            new,
            old,
            "{} -> {} prepared from retained evidence",
            self.name(from),
            self.name(to)
        );
        assert_eq!(self.generations(), before, "preparation changed a layer");
        new
    }

    /// Prepares the delete both ways and checks they agree.
    fn compare_delete(
        &mut self,
        target: LayerId,
        object: TargetPath,
    ) -> Result<NamespaceEdit, NamespaceError> {
        let before = self.generations();
        let target_edit = EditTarget::for_layer(target);
        let old = delete::prepare_by_composition(
            self.primary.stage(),
            &mut self.store,
            &target_edit,
            object,
        );
        let new = delete::prepare(self.primary.stage(), &mut self.store, &target_edit, object);
        assert_eq!(
            new,
            old,
            "delete {} prepared from retained evidence",
            self.name(object)
        );
        assert_eq!(self.generations(), before, "preparation changed a layer");
        new
    }

    /// The composed prims of the primary stage, the pseudo-root left
    /// out, in namespace order.
    fn prims(&mut self) -> Vec<PathId> {
        let root = self.store.path("/");
        let mut prims: Vec<PathId> = self
            .primary
            .stage()
            .traverse_all(root)
            .filter(|p| *p != root)
            .collect();
        self.store.paths.sort_paths(&mut prims, &self.store.tokens);
        prims
    }

    /// Every candidate edit of the primary stage, compared; the ones that
    /// succeeded, for applying.
    fn compare_all(&mut self, targets: &[LayerId]) -> Vec<NamespaceEdit> {
        let prims = self.prims();
        let mut prepared = Vec::new();
        let renamed = self.store.tokens.intern("Renamed");
        let inside = self.store.tokens.intern("Inside");
        for &target in targets {
            for &prim in &prims {
                let path = self.store.paths.resolve(prim).clone();
                let name = path.leaf().unwrap();
                let parent = path.parent().unwrap();
                let mut destinations = vec![
                    self.store.paths.intern(parent.join(&[renamed])),
                    self.store.paths.intern(path.join(&[inside])),
                    self.store.paths.intern(Path::root().join(&[renamed])),
                ];
                for &other in prims.iter().filter(|p| **p != prim).take(12) {
                    destinations.push(other);
                    let joined = self.store.paths.resolve(other).join(&[name]);
                    destinations.push(self.store.paths.intern(joined));
                }
                for destination in destinations {
                    if let Ok(edit) = self.compare_move(
                        target,
                        TargetPath::Prim(prim),
                        TargetPath::Prim(destination),
                    ) {
                        prepared.push(edit);
                    }
                }
                if let Ok(edit) = self.compare_delete(target, TargetPath::Prim(prim)) {
                    prepared.push(edit);
                }
                let names = self.primary.stage().property_names(prim, &self.store);
                for property in names {
                    let from = TargetPath::Property(PropertyPath::new(prim, property));
                    let property_name = String::from(self.store.tokens.resolve(property));
                    let new_name = self.store.tokens.intern(format!("{property_name}Renamed"));
                    let mut destinations = vec![PropertyPath::new(prim, new_name)];
                    for &other in prims.iter().filter(|p| **p != prim).take(6) {
                        destinations.push(PropertyPath::new(other, property));
                    }
                    for destination in destinations {
                        if let Ok(edit) =
                            self.compare_move(target, from, TargetPath::Property(destination))
                        {
                            prepared.push(edit);
                        }
                    }
                    if let Ok(edit) = self.compare_delete(target, from) {
                        prepared.push(edit);
                    }
                }
            }
        }
        prepared
    }

    /// Compares every candidate edit, then applies some that succeeded
    /// and compares again, `rounds` times.
    fn exercise(&mut self, targets: &[LayerId], rounds: usize) -> usize {
        let mut compared = 0;
        for round in 0..=rounds {
            let prepared = self.compare_all(targets);
            compared += prepared.len();
            if round == rounds || prepared.is_empty() {
                break;
            }
            // Spread the applied edits over the candidates.
            let edit = &prepared[(round * 7919) % prepared.len()];
            self.primary
                .apply(&mut self.store, edit.transaction())
                .unwrap();
            self.synchronize();
            // The live stage keeps its evidence as a new composition
            // would build it.
            let root = self.primary.stage().root_layer().unwrap();
            let options = self.primary.options().clone();
            let fresh = LiveStage::compose(&mut self.store, root, options);
            self.primary
                .stage()
                .preparation(&self.store.paths)
                .unwrap()
                .assert_current_as(
                    fresh.stage().preparation(&self.store.paths).unwrap(),
                    &self.store,
                );
        }
        compared
    }
}

#[test]
fn retained_preparation_matches_composition_for_authored_paths() {
    let mut scene = Scene::new(paths_scene(), StageOptions::default(), &[]);
    assert!(scene.exercise(&[ROOT, WEAK], 4) > 0);
}

#[test]
fn retained_preparation_matches_composition_for_root_prims() {
    for listed in [false, true] {
        let mut scene = Scene::new(roots_scene(listed), StageOptions::default(), &[]);
        assert!(scene.exercise(&[ROOT], 6) > 0);
    }
}

#[test]
fn retained_preparation_matches_composition_across_arcs_and_dependents() {
    for relocated in [false, true] {
        let mut scene = Scene::new(arcs_scene(relocated), StageOptions::default(), &[DEPENDENT]);
        scene.exercise(&[ROOT], 4);
    }
}

#[test]
fn retained_preparation_matches_composition_for_rejected_composition() {
    let mut scene = Scene::new(rejections_scene(), StageOptions::default(), &[]);
    scene.exercise(&[ROOT], 2);
    let mut scene = Scene::new(variants_scene(), StageOptions::default(), &[]);
    scene.exercise(&[ROOT], 1);
}

#[test]
fn retained_preparation_matches_composition_with_a_session_layer() {
    let mut store = paths_scene();
    let mut session = Layer::new(SESSION);
    author(&mut store, &mut session, "/World/A", PrimSpec::over());
    store.insert_layer(session);
    let options = StageOptions {
        session_layer: Some(SESSION),
        ..StageOptions::default()
    };
    let mut scene = Scene::new(store, options, &[]);
    scene.exercise(&[ROOT, SESSION], 2);
}

#[test]
fn retained_preparation_matches_composition_against_stale_snapshots() {
    let mut scene = Scene::new(paths_scene(), StageOptions::default(), &[]);
    // A new spec the stage has not seen.
    let late = scene.store.path("/World/Late");
    scene
        .store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .insert_prim(late, PrimSpec::def());
    scene.compare_all(&[ROOT]);
    scene.synchronize();
    // A new value only.
    let output = scene.store.tokens.intern("outputs:value");
    let a = scene.store.path("/World/A");
    *scene
        .store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .property_mut(PropertyPath::new(a, output))
        .unwrap() = attribute(9.0);
    scene.compare_all(&[ROOT]);
    // A notified edit not yet recomposed.
    scene.synchronize();
    scene.primary.notify_layer_edit(ROOT);
    scene.compare_all(&[ROOT]);
}
