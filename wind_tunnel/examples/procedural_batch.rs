// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Measure a procedural batch:
//! `procedural_batch [planned|split|existing|check] [prims] [runs]`.
//!
//! The batch defines `prims` typed prims (`Sphere` and `Cube`, alternating)
//! under four `Xform` groups of `/World`, sets three schema attributes of
//! each (`radius` or `size`, `purpose`, `doubleSided`), and gives every
//! tenth prim a `look` variant set whose selected `blur` branch applies
//! `MotionAPI` and sets its `motion:blurScale`, all through the root layer
//! of a `LiveStage` composed with the OpenUSD schemas.
//!
//! - `planned` applies it as one `Transaction`: each value edit is planned
//!   against the type or API schema an earlier edit of the same transaction
//!   authors (`planned_definition` in `layerstack/src/edit/apply.rs`).
//! - `split` applies the structure first (prims, variant sets, selections,
//!   applied schemas) and the values in a second transaction, against the
//!   prim definitions the first one composed; the second is the batch over
//!   prims that already exist, which no edit of it retypes, so it reads
//!   the composed definitions.
//! - `existing` applies the structure untimed, then times the values with
//!   each variant prim's `MotionAPI` applied again before its
//!   `motion:blurScale`: an edit of the transaction writes `apiSchemas`, so
//!   each later value edit reads its composed prim's sites
//!   (`Stage::spec_sites`) to see whether the definition changed.
//! - `check` runs both and fails unless they author the same root layer text.
//!
//! Each run starts from a fresh store; the transactions are built outside
//! the timers, and each timer covers `LiveStage::apply`, edits and
//! recomposition. Peak memory is the process's: run one mode per process
//! under `/usr/bin/time -l` (macOS) or `/usr/bin/time -v`.

use std::{hint::black_box, sync::Arc, time::Instant};

use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, ListPosition, LiveStage, PathId, PropertyPath,
    SpecPath, Specifier, StageOptions, TokenId, Transaction, Value,
};

const ROOT: LayerId = LayerId(1);
const GROUPS: usize = 4;

/// The tokens the batch authors.
struct Names {
    xform: TokenId,
    sphere: TokenId,
    cube: TokenId,
    radius: TokenId,
    size: TokenId,
    purpose: TokenId,
    render: TokenId,
    double_sided: TokenId,
    look: TokenId,
    blur: TokenId,
    motion_api: TokenId,
    blur_scale: TokenId,
}

impl Names {
    fn new(store: &InMemoryStore) -> Self {
        let tokens = store.tokens();
        Self {
            xform: tokens.intern("Xform"),
            sphere: tokens.intern("Sphere"),
            cube: tokens.intern("Cube"),
            radius: tokens.intern("radius"),
            size: tokens.intern("size"),
            purpose: tokens.intern("purpose"),
            render: tokens.intern("render"),
            double_sided: tokens.intern("doubleSided"),
            look: tokens.intern("look"),
            blur: tokens.intern("blur"),
            motion_api: tokens.intern("MotionAPI"),
            blur_scale: tokens.intern("motion:blurScale"),
        }
    }
}

/// One prim of the batch.
struct Item {
    path: PathId,
    sphere: bool,
    /// The `look=blur` branch of the prim, for every tenth prim.
    variant: Option<SpecPath>,
}

/// A fresh store holding an empty root layer, the batch's paths and names
/// interned, and a stage composed on it with the OpenUSD schemas.
struct Scene {
    store: InMemoryStore,
    live: LiveStage,
    names: Names,
    world: PathId,
    groups: Vec<PathId>,
    items: Vec<Item>,
}

impl Scene {
    fn new(prims: usize) -> Self {
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(ROOT));
        let names = Names::new(&store);
        let world = store.path("/World");
        let groups = (0..GROUPS)
            .map(|g| store.path(&format!("/World/G{g}")))
            .collect();
        let items = (0..prims)
            .map(|i| {
                let text = format!("/World/G{}/P{i}", i % GROUPS);
                let variant = (i % 10 == 0).then(|| {
                    SpecPath::parse(
                        &format!("{text}{{look=blur}}"),
                        store.tokens(),
                        store.paths(),
                    )
                    .expect("a variant path")
                });
                Item {
                    path: store.path(&text),
                    sphere: i % 2 == 0,
                    variant,
                }
            })
            .collect();
        let options = StageOptions {
            schemas: Some(Arc::new(layerstack_schemas::openusd(store.tokens()))),
            ..StageOptions::default()
        };
        let live = LiveStage::compose(&mut store, ROOT, options);
        Self {
            store,
            live,
            names,
            world,
            groups,
            items,
        }
    }

    /// Adds the structure of the batch: the groups and prims, and each
    /// variant prim's set, selection and applied schema.
    fn structure(&self, txn: &mut Transaction, item: &Item) {
        let n = &self.names;
        let root = EditTarget::for_layer(ROOT);
        let ty = if item.sphere { n.sphere } else { n.cube };
        txn.create_prim(root.prim(item.path), Specifier::Def, Some(ty));
        if let Some(variant) = &item.variant {
            txn.add_variant_set(root.prim(item.path), n.look, ListPosition::BackOfPrepend)
                .set_variant_selection(root.prim(item.path), n.look, Some(n.blur));
            let branch = EditTarget::for_local_variant(ROOT, variant);
            txn.add_applied_schema(branch.prim(item.path), n.motion_api);
        }
    }

    /// Adds the values of the batch for `item`.
    fn values(&self, txn: &mut Transaction, item: &Item) {
        let n = &self.names;
        let root = EditTarget::for_layer(ROOT);
        let property = |name| root.property(PropertyPath::new(item.path, name));
        let (extent, value) = if item.sphere {
            (n.radius, 0.5)
        } else {
            (n.size, 1.0)
        };
        txn.set_default(property(extent), Value::Double(value))
            .set_default(property(n.purpose), Value::Token(n.render))
            .set_default(property(n.double_sided), Value::Bool(true));
        if let Some(variant) = &item.variant {
            let branch = EditTarget::for_local_variant(ROOT, variant);
            txn.set_default(
                branch.property(PropertyPath::new(item.path, n.blur_scale)),
                Value::Float(0.5),
            );
        }
    }

    fn groups(&self, txn: &mut Transaction) {
        let root = EditTarget::for_layer(ROOT);
        txn.create_prim(
            root.prim(self.world),
            Specifier::Def,
            Some(self.names.xform),
        );
        for &group in &self.groups {
            txn.create_prim(root.prim(group), Specifier::Def, Some(self.names.xform));
        }
    }

    /// The whole batch as one transaction, each prim's structure followed by
    /// its values.
    fn planned(&self) -> Transaction {
        let mut txn = Transaction::new();
        self.groups(&mut txn);
        for item in &self.items {
            self.structure(&mut txn, item);
            self.values(&mut txn, item);
        }
        txn
    }

    /// The batch's values over the prims `structure` composed, each variant
    /// prim's schema applied again before its value.
    fn reapplied(&self) -> Transaction {
        let mut txn = Transaction::new();
        for item in &self.items {
            if let Some(variant) = &item.variant {
                let branch = EditTarget::for_local_variant(ROOT, variant);
                txn.add_applied_schema(branch.prim(item.path), self.names.motion_api);
            }
            self.values(&mut txn, item);
        }
        txn
    }

    /// The batch's structure, then its values, as two transactions.
    fn split(&self) -> (Transaction, Transaction) {
        let mut structure = Transaction::new();
        self.groups(&mut structure);
        let mut values = Transaction::new();
        for item in &self.items {
            self.structure(&mut structure, item);
            self.values(&mut values, item);
        }
        (structure, values)
    }

    fn apply(&mut self, txn: &Transaction) -> f64 {
        let started = Instant::now();
        let applied = self
            .live
            .apply(&mut self.store, txn)
            .unwrap_or_else(|error| panic!("{}", error.display(&self.store)));
        let elapsed = started.elapsed().as_secs_f64() * 1e3;
        black_box(applied);
        elapsed
    }

    fn text(&self) -> String {
        let layer = self.store.layer(ROOT).expect("the root layer");
        layerstack_usda::save_usda(layer, self.store.tokens(), self.store.paths()).expect("saves")
    }
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "planned".into());
    let prims: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(10_000);
    let runs: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    match mode.as_str() {
        "planned" => {
            let mut times = Vec::new();
            let mut ops = 0;
            for _ in 0..runs {
                let mut scene = Scene::new(prims);
                let txn = scene.planned();
                ops = txn.len();
                times.push(scene.apply(&txn));
            }
            println!(
                "planned prims={prims} ops={ops} runs={runs} apply_ms median={:.1} min={:.1}",
                median(times.clone()),
                times.iter().copied().fold(f64::INFINITY, f64::min)
            );
        }
        "split" => {
            let (mut first, mut second) = (Vec::new(), Vec::new());
            let mut ops = (0, 0);
            for _ in 0..runs {
                let mut scene = Scene::new(prims);
                let (structure, values) = scene.split();
                ops = (structure.len(), values.len());
                first.push(scene.apply(&structure));
                second.push(scene.apply(&values));
            }
            let total: Vec<f64> = first.iter().zip(&second).map(|(a, b)| a + b).collect();
            println!(
                "split prims={prims} ops={}+{} runs={runs} structure_ms={:.1} \
                 values_ms={:.1} total_ms={:.1} (medians)",
                ops.0,
                ops.1,
                median(first),
                median(second),
                median(total)
            );
        }
        "existing" => {
            let mut times = Vec::new();
            let mut ops = 0;
            for _ in 0..runs {
                let mut scene = Scene::new(prims);
                let (structure, _) = scene.split();
                scene.apply(&structure);
                let txn = scene.reapplied();
                ops = txn.len();
                times.push(scene.apply(&txn));
            }
            println!(
                "existing prims={prims} ops={ops} runs={runs} apply_ms median={:.1} min={:.1}",
                median(times.clone()),
                times.iter().copied().fold(f64::INFINITY, f64::min)
            );
        }
        "check" => {
            let mut planned = Scene::new(prims);
            let txn = planned.planned();
            planned.apply(&txn);
            let mut split = Scene::new(prims);
            let (structure, values) = split.split();
            split.apply(&structure);
            split.apply(&values);
            let mut existing = Scene::new(prims);
            let (structure, _) = existing.split();
            existing.apply(&structure);
            let txn = existing.reapplied();
            existing.apply(&txn);
            let (a, b) = (planned.text(), split.text());
            assert_eq!(a, b, "both author the same root layer");
            assert_eq!(a, existing.text(), "reapplying changes nothing more");
            let fresh = LiveStage::compose(
                &mut planned.store,
                ROOT,
                planned.live.stage().options().clone(),
            );
            let count = |live: &LiveStage, store: &InMemoryStore| {
                let root = store.path("/");
                live.stage().traverse(root).count()
            };
            assert_eq!(
                count(&planned.live, &planned.store),
                count(&fresh, &planned.store),
                "the live stage composes what a fresh composition does"
            );
            println!(
                "check prims={prims}: identical root layers ({} bytes), {} composed prims",
                a.len(),
                count(&fresh, &planned.store)
            );
        }
        other => panic!("unknown mode {other}: planned, split, existing or check"),
    }
}
