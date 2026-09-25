// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An authoring session over two rocks that reference one asset, through
//! `layerstack::edit` and a `LiveStage`.
//!
//! Every step asserts the effective and authored values C++ OpenUSD 26.8
//! produces for the same edits (through `Usd.EditTarget`, `UsdEditContext`
//! and `Sdf` authoring), and that the live stage matches a fresh
//! composition. The last step saves both layers with
//! `layerstack_usda::save`, reopens them, and checks the same values.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::edit::{EditError, Rejection};
use layerstack::{
    Address, ArcKind, AssetResolveError, AssetResolver, EditTarget, InMemoryStore,
    InterpolationType, Layer, LayerId, LiveStage, PathId, PathInterner, PropertyPath,
    ResolvedAsset, ResolvedValue, SpecPath, Stage, StageOptions, TokenInterner, Transaction, Value,
};
use layerstack_usda::{emit, lower, parser::parse_cst};

const ROCK_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Rock"
)

def Xform "Rock" (
    variants = {
        string shape = "round"
    }
    prepend variantSets = "shape"
)
{
    double size = 1
    color3f[] primvars:color = [(0.5, 0.5, 0.5)] (
        interpolation = "constant"
    )
    double spin.timeSamples = {
        0: 0,
        10: 100,
    }

    variantSet "shape" = {
        "jagged" {
            double roughness = 0.9
            color3f[] primvars:tint = [(0.4, 0.3, 0.2)]
        }
        "round" {
            double roughness = 0.1
        }
    }
}
"#;

const SCENE_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{
    def Xform "RockA" (
        prepend references = @./rock.usda@</Rock>
    )
    {
    }

    def Xform "RockB" (
        prepend references = @./rock.usda@</Rock> (offset = 10; scale = 2)
    )
    {
    }
}
"#;

const SCENE: LayerId = LayerId(1);
const ROCK: LayerId = LayerId(2);

struct MemoryResolver {
    rock: String,
    by_name: BTreeMap<String, LayerId>,
    pending: Vec<Layer>,
}

impl AssetResolver for MemoryResolver {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let name = asset_path.trim_start_matches("./");
        if let Some(id) = self.by_name.get(name) {
            return Ok(ResolvedAsset {
                layer_id: *id,
                resolved_path: Arc::from(name),
                layer: None,
            });
        }
        if name != "rock.usda" {
            return Err(AssetResolveError::NotFound);
        }
        self.by_name.insert(name.to_string(), ROCK);
        let rock = self.rock.clone();
        let layer = emit_layer(&rock, ROCK, tokens, paths, self);
        Ok(ResolvedAsset {
            layer_id: ROCK,
            resolved_path: Arc::from(name),
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

fn emit_layer(
    source: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut MemoryResolver,
) -> Layer {
    let cst = parse_cst(source);
    assert!(cst.diagnostics.is_empty(), "{:?}", cst.diagnostics);
    let ast = lower::lower(&cst.tree, source);
    assert!(ast.diagnostics.is_empty(), "{:?}", ast.diagnostics);
    let result = emit::emit(&ast.layer, layer_id, tokens, paths, resolver);
    resolver.pending.extend(result.resolved_layers);
    result.layer
}

fn load(scene: &str, rock: &str) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let mut resolver = MemoryResolver {
        rock: rock.to_string(),
        by_name: BTreeMap::new(),
        pending: Vec::new(),
    };
    let scene = emit_layer(
        scene,
        SCENE,
        &mut store.tokens,
        &mut store.paths,
        &mut resolver,
    );
    store.insert_layer(scene);
    for layer in resolver.pending.drain(..) {
        store.insert_layer(layer);
    }
    store
}

/// A probe value, as the C++ reference reports it.
#[derive(Clone, Debug, PartialEq)]
enum Probe {
    Num(f64),
    Text(String),
    Color([f64; 3]),
    Samples(Vec<(f64, f64)>),
    Absent,
}

fn num(value: &Value) -> Probe {
    match value {
        Value::Double(d) => Probe::Num((d * 1e6).round() / 1e6),
        other => panic!("not a double: {other:?}"),
    }
}

fn scalar(stage: &Stage, path: PropertyPath) -> Option<Value> {
    match stage.resolve_property_path(path)?.value {
        ResolvedValue::Scalar(value) => Some(value),
        _ => None,
    }
}

fn spec(store: &mut InMemoryStore, text: &str) -> SpecPath {
    SpecPath::parse(text, &mut store.tokens, &mut store.paths).expect("spec path")
}

/// The probe of the C++ reference: effective values of both rocks and the
/// authored state of the edited specs.
fn probe(stage: &Stage, store: &mut InMemoryStore) -> BTreeMap<String, Probe> {
    let mut out = BTreeMap::new();
    let shape = store.tokens.intern("shape");
    for name in ["RockA", "RockB"] {
        let prim = store.path(&format!("/World/{name}"));
        let prop =
            |store: &mut InMemoryStore, p: &str| store.property_path(&format!("/World/{name}.{p}"));
        let size = prop(store, "size");
        let roughness = prop(store, "roughness");
        let spin = prop(store, "spin");
        let color = prop(store, "primvars:color");
        for (key, path) in [("size", size), ("roughness", roughness)] {
            let value = scalar(stage, path).map_or(Probe::Absent, |v| num(&v));
            out.insert(format!("{name}.{key}"), value);
        }
        let selection = stage.variant_selections(prim, store).get(&shape).copied();
        out.insert(
            format!("{name}.shape"),
            selection.map_or(Probe::Absent, |v| {
                Probe::Text(store.tokens.resolve(v).to_string())
            }),
        );
        for time in [2.0, 14.0] {
            // The stage default interpolation, as `UsdAttribute::Get(time)`.
            let value = stage
                .resolve_property_path_at_time(spin, time, InterpolationType::default())
                .map_or(Probe::Absent, |r| num(&r.value));
            out.insert(format!("{name}.spin@{time}"), value);
        }
        let color = match scalar(stage, color) {
            Some(Value::Array(items)) => match items.first() {
                Some(Value::Vec3f(c)) => {
                    Probe::Color(c.map(|x| (f64::from(x) * 1e6).round() / 1e6))
                }
                other => panic!("unexpected color {other:?}"),
            },
            _ => Probe::Absent,
        };
        out.insert(format!("{name}.color"), color);
    }
    let authored = |store: &mut InMemoryStore, layer: LayerId, path: &str| {
        let path = spec(store, path);
        match store.layers[&layer].property_at(&path, &store.paths) {
            Some(spec) => spec.default.as_ref().map_or(Probe::Absent, num),
            None => Probe::Text("<absent>".into()),
        }
    };
    let entries = [
        ("scene:/World/RockA.size", SCENE, "/World/RockA.size"),
        ("rock:/Rock.size", ROCK, "/Rock.size"),
        (
            "rock:/Rock{shape=jagged}.roughness",
            ROCK,
            "/Rock{shape=jagged}.roughness",
        ),
    ];
    for (key, layer, path) in entries {
        let value = authored(store, layer, path);
        out.insert(key.into(), value);
    }
    let rock_a = spec(store, "/World/RockA");
    let selection = store.layers[&SCENE].variant_selection_at(&rock_a, shape, &store.paths);
    out.insert(
        "scene:/World/RockA{shape}".into(),
        Probe::Text(selection.map_or("<absent>".into(), |v| store.tokens.resolve(v).to_string())),
    );
    let spin = spec(store, "/Rock.spin");
    let samples = store.layers[&ROCK]
        .property_at(&spin, &store.paths)
        .and_then(|p| p.time_samples.clone())
        .unwrap_or_default()
        .iter()
        .map(|(t, v)| match num(v) {
            Probe::Num(v) => (*t, v),
            _ => unreachable!("numbers"),
        })
        .collect();
    out.insert("rock:/Rock.spin.samples".into(), Probe::Samples(samples));
    out
}

/// The C++ reference probe of each step: the initial probe with the
/// step's changes.
fn expected(step: &str) -> BTreeMap<String, Probe> {
    use Probe::{Color, Num, Samples, Text};
    let grey = Color([0.5, 0.5, 0.5]);
    let mut out: BTreeMap<String, Probe> = [
        ("RockA.color", grey.clone()),
        ("RockA.roughness", Num(0.1)),
        ("RockA.shape", Text("round".into())),
        ("RockA.size", Num(1.0)),
        ("RockA.spin@14", Num(100.0)),
        ("RockA.spin@2", Num(20.0)),
        ("RockB.color", grey),
        ("RockB.roughness", Num(0.1)),
        ("RockB.shape", Text("round".into())),
        ("RockB.size", Num(1.0)),
        ("RockB.spin@14", Num(20.0)),
        ("RockB.spin@2", Num(0.0)),
        ("rock:/Rock.size", Num(1.0)),
        (
            "rock:/Rock.spin.samples",
            Samples(vec![(0.0, 0.0), (10.0, 100.0)]),
        ),
        ("rock:/Rock{shape=jagged}.roughness", Num(0.9)),
        ("scene:/World/RockA.size", Text("<absent>".into())),
        ("scene:/World/RockA{shape}", Text("<absent>".into())),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let a = [
        ("RockA.size", Num(2.0)),
        ("scene:/World/RockA.size", Num(2.0)),
    ];
    let b = [("RockB.size", Num(3.0)), ("rock:/Rock.size", Num(3.0))];
    let c = [
        ("RockA.roughness", Num(0.95)),
        ("RockA.shape", Text("jagged".into())),
        ("rock:/Rock{shape=jagged}.roughness", Num(0.95)),
        ("scene:/World/RockA{shape}", Text("jagged".into())),
    ];
    let d = [
        ("RockA.spin@2", Num(70.0)),
        ("RockB.spin@14", Num(70.0)),
        (
            "rock:/Rock.spin.samples",
            Samples(vec![(0.0, 0.0), (2.0, 70.0), (10.0, 100.0)]),
        ),
    ];
    let e = [
        ("RockA.size", Num(4.0)),
        ("RockB.size", Num(4.0)),
        ("rock:/Rock.size", Num(4.0)),
    ];
    let f = [
        ("RockA.size", Num(2.0)),
        ("RockB.size", Num(7.0)),
        ("rock:/Rock.size", Num(7.0)),
        ("scene:/World/RockA.size", Num(2.0)),
    ];
    let changes: Vec<(&str, Probe)> = match step {
        "initial" => Vec::new(),
        "a" => a.to_vec(),
        "b" => [&a[..], &b].concat(),
        "c" => [&a[..], &b, &c].concat(),
        "d" => [&a[..], &b, &c, &d].concat(),
        "e" => e.to_vec(),
        "f" | "g" => f.to_vec(),
        "h" => [&f[..], &c, &d].concat(),
        _ => unreachable!("step {step}"),
    };
    for (key, value) in changes {
        out.insert(key.to_string(), value);
    }
    out
}

struct Session {
    store: InMemoryStore,
    live: LiveStage,
}

impl Session {
    fn open(scene: &str, rock: &str) -> Self {
        let mut store = load(scene, rock);
        let live = LiveStage::compose(&mut store, SCENE, StageOptions::default());
        Self { store, live }
    }

    /// Checks the step's probe against the C++ reference, and the live
    /// stage against a fresh composition.
    fn check(&mut self, step: &str) {
        let live = probe(self.live.stage(), &mut self.store);
        assert_eq!(live, expected(step), "step {step}: probe matches C++");
        let fresh = Stage::compose(&mut self.store, SCENE, StageOptions::default());
        assert_eq!(
            probe(&fresh, &mut self.store),
            live,
            "step {step}: live stage matches a fresh composition"
        );
    }

    fn target(&self, prim: PathId, pick: impl Fn(ArcKind, LayerId) -> bool) -> EditTarget {
        let stage = self.live.stage();
        let graph = stage.explain_prim_graph(prim).expect("graph");
        let node = graph
            .depth_first()
            .into_iter()
            .find(|id| {
                let node = graph.node(*id).expect("node");
                pick(node.arc_kind(), node.layer_stack())
            })
            .expect("node");
        EditTarget::for_node(stage, prim, node).expect("target")
    }

    fn apply(&mut self, txn: &Transaction) -> Transaction {
        self.live
            .apply(&mut self.store, txn)
            .expect("transaction applies")
            .inverse
    }
}

#[test]
fn rocks_authoring_session_matches_openusd() {
    let mut s = Session::open(SCENE_USDA, ROCK_USDA);
    s.check("initial");
    let original_scene = s.store.layers[&SCENE].clone();
    let (rock_a, rock_b) = (s.store.path("/World/RockA"), s.store.path("/World/RockB"));
    let prop = |s: &mut Session, p: &str| s.store.property_path(p);
    let size_a = prop(&mut s, "/World/RockA.size");
    let size_b = prop(&mut s, "/World/RockB.size");
    let roughness_a = prop(&mut s, "/World/RockA.roughness");
    let spin_b = prop(&mut s, "/World/RockB.spin");
    let (shape, jagged) = (
        s.store.tokens.intern("shape"),
        s.store.tokens.intern("jagged"),
    );
    let mut undo = Vec::new();

    // a: a local override in the root layer.
    let root = EditTarget::for_layer(SCENE);
    let mut edit = Transaction::new();
    edit.set_default(root.property(size_a), Value::Double(2.0));
    undo.push(s.apply(&edit));
    s.check("a");

    // b: the shared definition, through RockB's reference.
    let through_b = s.target(rock_b, |kind, stack| {
        kind == ArcKind::References && stack == ROCK
    });
    assert_eq!(
        through_b
            .map_property_to_spec_path(size_b, &mut s.store.paths)
            .map(|p| p.display(&s.store.tokens))
            .as_deref(),
        Some("/Rock.size")
    );
    let mut edit = Transaction::new();
    edit.set_default(through_b.property(size_b), Value::Double(3.0));
    undo.push(s.apply(&edit));
    s.check("b");

    // c: select `jagged` on RockA, then edit inside that variant through
    // the reference.
    let mut edit = Transaction::new();
    edit.set_variant_selection(root.prim(rock_a), shape, Some(jagged));
    undo.push(s.apply(&edit));
    let in_jagged = s.target(rock_a, |kind, stack| {
        kind == ArcKind::Variants && stack == ROCK
    });
    assert_eq!(
        in_jagged
            .map_property_to_spec_path(roughness_a, &mut s.store.paths)
            .map(|p| p.display(&s.store.tokens))
            .as_deref(),
        Some("/Rock{shape=jagged}.roughness")
    );
    let mut edit = Transaction::new();
    edit.set_default(in_jagged.property(roughness_a), Value::Double(0.95));
    undo.push(s.apply(&edit));
    s.check("c");

    // d: a time sample at stage time 14 through `(offset = 10; scale = 2)`
    // lands at layer time 2.
    assert_eq!(through_b.map_to_spec_time(14.0), 2.0);
    let mut edit = Transaction::new();
    edit.set_time_sample(through_b.property(spin_b), 14.0, Value::Double(70.0));
    undo.push(s.apply(&edit));
    s.check("d");

    // e: undo newest first; the override is removed, not rewritten, so the
    // scene layer is as loaded and a later shared edit reaches RockA.
    while let Some(inverse) = undo.pop() {
        s.apply(&inverse);
    }
    assert_eq!(s.store.layers[&SCENE], original_scene);
    let rock_size = Address::spec(ROCK, spec(&mut s.store, "/Rock.size"));
    let mut edit = Transaction::new();
    edit.set_default(rock_size.clone(), Value::Double(4.0));
    s.apply(&edit);
    s.check("e");

    // f: prepare an edit, let something else rewrite the shared spec, and
    // apply the prepared edit: it is stale although RockA's effective
    // value did not change.
    let mut edit = Transaction::new();
    edit.set_default(root.property(size_a), Value::Double(2.0));
    s.apply(&edit);
    let mut prepared = Transaction::new();
    prepared.set_default(rock_size.clone(), Value::Double(5.0));
    prepared.expect_unchanged(&mut s.store);
    let rock_size_path = s.store.property_path("/Rock.size");
    s.store
        .layers
        .get_mut(&ROCK)
        .and_then(|layer| layer.property_mut(rock_size_path))
        .expect("size spec")
        .default = Some(Value::Double(7.0));
    assert_eq!(s.live.notify_changed_layers(&s.store), [ROCK]);
    s.live.recompose(&mut s.store);
    let stale = s.live.apply(&mut s.store, &prepared);
    assert!(
        matches!(stale, Err(EditError::StaleGeneration { layer: ROCK, .. })),
        "{stale:?}"
    );
    s.check("f");

    // g: a batch across both layers with one invalid edit leaves both
    // layers unchanged: a type mismatch, then an unmappable path.
    let before = (
        s.store.layers[&SCENE].clone(),
        s.store.layers[&ROCK].clone(),
    );
    let generations = (
        s.store.layers[&SCENE].generation(),
        s.store.layers[&ROCK].generation(),
    );
    // `/Rock.roughness` has no spec outside the variants; its composed
    // declaration is a double.
    let roughness_b = prop(&mut s, "/World/RockB.roughness");
    let roughness = through_b.property(roughness_b);
    let mut batch = Transaction::new();
    batch
        .set_default(root.property(size_b), Value::Double(10.0))
        .set_default(rock_size.clone(), Value::Double(11.0))
        .set_default(roughness, Value::string("rough"));
    assert!(matches!(
        s.live.apply(&mut s.store, &batch),
        Err(EditError::Rejected {
            op: 2,
            reason: Rejection::TypeMismatch { .. }
        })
    ));
    let mut batch = Transaction::new();
    batch
        .set_default(root.property(size_b), Value::Double(10.0))
        .set_default(through_b.property(size_a), Value::Double(11.0));
    assert!(matches!(
        s.live.apply(&mut s.store, &batch),
        Err(EditError::Rejected {
            op: 1,
            reason: Rejection::Unmappable
        })
    ));
    assert_eq!(
        (
            s.store.layers[&SCENE].clone(),
            s.store.layers[&ROCK].clone()
        ),
        before
    );
    assert_eq!(
        (
            s.store.layers[&SCENE].generation(),
            s.store.layers[&ROCK].generation(),
        ),
        generations
    );
    s.check("g");

    // h: select `jagged` again and author inside it and on the timeline,
    // then save both layers, reopen them, and read the same values.
    let jagged_roughness = spec(&mut s.store, "/Rock{shape=jagged}.roughness");
    let spin = spec(&mut s.store, "/Rock.spin");
    let mut edit = Transaction::new();
    edit.set_variant_selection(root.prim(rock_a), shape, Some(jagged))
        .set_default(Address::spec(ROCK, jagged_roughness), Value::Double(0.95))
        .set_time_sample(Address::spec(ROCK, spin), 2.0, Value::Double(70.0));
    s.apply(&edit);
    s.check("h");
    let save = |s: &Session, layer: LayerId| {
        layerstack_usda::save::save_usda(&s.store.layers[&layer], &s.store.tokens, &s.store.paths)
            .expect("saves")
    };
    let (scene_text, rock_text) = (save(&s, SCENE), save(&s, ROCK));
    let mut reopened = Session::open(&scene_text, &rock_text);
    reopened.check("h");
}
