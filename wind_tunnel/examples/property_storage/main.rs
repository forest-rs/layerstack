// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Measure alternative property layouts on authored USDA or USDC input.
//!
//! Run with `cargo run --release -p wind_tunnel --example property_storage -- MODE OP FILE`.
//! Modes are `native`, `soa`, `tile` (64 rows), `packed`, `inline`, `soa-packed`,
//! `tile-packed`, `handles` and `share`. The `import` operation times full layer loading.
//! `save` borrows declarations and target lists directly from each layout. `query` times a shuffled
//! sweep of ordinary properties, copying a snapshot and editing one frequently occurring
//! property. Experimental edits measure storage detachment only. `verify` compares the
//! directly saved USDA and USDC bytes and the reconstructed layer with the ordinary reader.
//! `verify-edit` compares the edited layer and held snapshot to their native equivalents.
//! These alternative layouts cover ordinary prims with distinct paths. Variant branch
//! specs stay in native storage. The experiment has no stage or edit journal adapter.
mod columns;
mod edges;
mod handles;
mod sharing;

use columns::Columns;
use edges::{Edges, Mode};
use layerstack::property::PropertyKind;
use layerstack::{
    InMemoryStore, Layer, LayerId, ListOp, NoAssets, PropertyPath, TargetPath, Value,
};
use std::{env, fs, hint::black_box, mem, sync::Arc, time::Instant};

#[derive(Clone)]
struct Properties {
    columns: Option<Columns>,
    handles: Option<handles::Handles>,
    sharing: Option<sharing::Sharing>,
    edges: Edges,
    mode: Mode,
}

impl Properties {
    fn new(name: &str) -> Self {
        let block = match name {
            "soa" | "soa-packed" => Some(usize::MAX),
            "tile" | "tile-packed" => Some(64),
            "native" | "packed" | "inline" | "handles" | "share" => None,
            _ => panic!("unknown storage"),
        };
        let mode = if name.ends_with("packed") {
            Mode::Packed
        } else if name == "inline" {
            Mode::Inline
        } else {
            Mode::Native
        };
        Self {
            columns: block.map(Columns::new),
            handles: (name == "handles").then(handles::Handles::default),
            sharing: (name == "share").then(sharing::Sharing::default),
            edges: Edges::default(),
            mode,
        }
    }
    fn capture(&mut self, path: layerstack::PathId, prim: &mut layerstack::PrimSpec) {
        if let Some(sharing) = &mut self.sharing {
            sharing.capture(prim);
        }
        if self.mode != Mode::Native {
            for e in &mut prim.properties {
                if e.spec.targets.is_some() {
                    let op = Arc::make_mut(&mut e.spec).targets.take().unwrap();
                    self.edges
                        .insert(PropertyPath::new(path, e.name), op, self.mode);
                }
            }
        }
        if let Some(h) = &mut self.handles {
            h.insert(path, mem::take(&mut prim.properties));
        }
        if let Some(c) = &mut self.columns {
            c.insert(path, mem::take(&mut prim.properties));
        }
    }
    fn query<'a>(
        &'a self,
        layer: &'a Layer,
        key: PropertyPath,
    ) -> (PropertyKind, Option<&'a Value>, usize) {
        let (kind, default, spec) = if let Some(c) = &self.columns {
            c.get(key).unwrap()
        } else {
            let s = self
                .handles
                .as_ref()
                .map(|h| h.get(key))
                .unwrap_or_else(|| layer.property(key).unwrap());
            (s.kind, s.default.as_ref(), Some(s))
        };
        let count = if self.mode == Mode::Native {
            spec.and_then(|s| s.targets.as_deref()).map(edges::count)
        } else {
            self.edges
                .count(key)
                .or_else(|| spec.and_then(|s| s.targets.as_deref()).map(edges::count))
        };
        (kind, default, count.unwrap_or(usize::MAX))
    }
    fn edit(&mut self, layer: &mut Layer, key: PropertyPath, kind: PropertyKind) {
        if kind == PropertyKind::Attribute {
            if let Some(c) = &mut self.columns {
                c.set_default(key, Value::Blocked);
            } else if let Some(h) = &mut self.handles {
                h.get_mut(key).default = Some(Value::Blocked);
            } else {
                layer.property_mut(key).unwrap().default = Some(Value::Blocked);
            }
        } else {
            let mut op = Box::new(ListOp::new());
            op.explicit = Some(vec![TargetPath::Prim(key.prim_path())]);
            if self.mode != Mode::Native
                && layer
                    .prim(key.prim_path())
                    .is_some_and(|p| p.outer_variant_sites().is_empty())
            {
                // Keep imported runs immutable while a snapshot holds the arena.
                // An edit owns its replacement and leaves unrelated runs in place.
                self.edges.insert(key, op, Mode::Inline);
            } else if let Some(c) = &mut self.columns {
                c.set_targets(key, op);
            } else if let Some(h) = &mut self.handles {
                h.get_mut(key).targets = Some(op);
            } else {
                layer.property_mut(key).unwrap().targets = Some(op);
            }
        }
    }
    fn restore(self, layer: &mut Layer) {
        if let Some(h) = self.handles {
            h.restore(layer);
        }
        if let Some(c) = self.columns {
            c.restore(layer);
        }
        if self.mode != Mode::Native {
            let prims = layer.prims_mut();
            for key in self.edges.keys() {
                prims
                    .get_mut(&key.prim_path())
                    .unwrap()
                    .property_mut(key.property())
                    .unwrap()
                    .targets = self.edges.expand(key);
            }
        }
    }
}

impl layerstack_usda::save::PropertySource for Properties {
    fn properties<'a>(
        &'a self,
        path: layerstack::PathId,
        prim: &'a layerstack::PrimSpec,
    ) -> impl Iterator<Item = layerstack_usda::save::PropertyView<'a>> {
        let columns = self.columns.iter().flat_map(move |c| c.row(path));
        let handles = self.handles.iter().flat_map(move |h| h.row(path));
        // Captured rows are removed from the prim, so exactly one iterator has entries.
        columns
            .chain(handles)
            .chain(layerstack_usda::save::NativeProperties.properties(path, prim))
            .map(move |mut p| {
                if self.mode != Mode::Native
                    && let Some(targets) = self.edges.borrow(PropertyPath::new(path, p.name))
                {
                    p.targets = Some(targets);
                }
                p
            })
    }
}

fn import(bytes: &[u8], store: &InMemoryStore, properties: &mut Properties, mode: &str) -> Layer {
    if bytes.starts_with(b"PXR-USDC") {
        let result = if mode == "native" {
            layerstack_usdc::read_usdc(
                bytes,
                LayerId(1),
                store.tokens(),
                store.paths(),
                &mut NoAssets,
            )
        } else {
            layerstack_usdc::read_usdc_with_prim_visitor(
                bytes,
                LayerId(1),
                store.tokens(),
                store.paths(),
                &mut NoAssets,
                &mut |p, s| properties.capture(p, s),
            )
        }
        .unwrap();
        assert!(
            result.diagnostics.is_empty(),
            "USDC diagnostics: {:?}",
            result.diagnostics
        );
        result.layer
    } else {
        let text = std::str::from_utf8(bytes).unwrap();
        let result = if mode == "native" {
            layerstack_usda::read_usda(
                text,
                LayerId(1),
                store.tokens(),
                store.paths(),
                &mut NoAssets,
            )
        } else {
            layerstack_usda::read_usda_with_prim_visitor(
                text,
                LayerId(1),
                store.tokens(),
                store.paths(),
                &mut NoAssets,
                &mut |p, s| properties.capture(p, s),
            )
        };
        assert!(
            !result.has_errors() && !result.is_rejected(),
            "USDA diagnostics: {:?}",
            result.diagnostics().collect::<Vec<_>>()
        );
        result.emitted.layer
    }
}

fn main() {
    let args: Vec<_> = env::args().collect();
    assert_eq!(args.len(), 4, "expected MODE OP FILE");
    let mode = &args[1];
    let operation = &args[2];
    let bytes = fs::read(&args[3]).unwrap();
    let start = Instant::now();
    let store = InMemoryStore::default();
    let mut properties = Properties::new(mode);
    let mut layer = import(&bytes, &store, &mut properties, mode);
    if let Some(c) = &mut properties.columns {
        c.finish();
    }
    if let Some(h) = &mut properties.handles {
        h.finish();
    }
    if let Some(sharing) = &mut properties.sharing {
        sharing.finish();
    }
    let read_ns = start.elapsed().as_nanos();
    black_box((&layer, &properties));
    if operation == "stats" {
        assert_eq!(mode, "native", "stats counts native spec handles");
        let mut pointers = std::collections::HashSet::new();
        let mut entries = 0;
        for (_, prim) in layer.prims().iter() {
            for p in &prim.properties {
                pointers.insert(Arc::as_ptr(&p.spec));
                entries += 1;
            }
        }
        println!("{} {}", entries, pointers.len());
        return;
    }
    if operation == "import" {
        println!("{read_ns}");
        return;
    }
    let text = || {
        if mode == "native" {
            layerstack_usda::save_usda(&layer, store.tokens(), store.paths())
        } else {
            layerstack_usda::save::save_usda_from(
                &layer,
                store.tokens(),
                store.paths(),
                &properties,
            )
        }
    };
    let binary = || {
        if mode == "native" {
            layerstack_usdc::save_usdc(&layer, store.tokens(), store.paths())
        } else {
            layerstack_usdc::writer::save_usdc_from(
                &layer,
                store.tokens(),
                store.paths(),
                &properties,
            )
        }
    };
    if let Some(path) = operation.strip_prefix("write=") {
        if path.ends_with(".usda") {
            fs::write(path, text().unwrap()).unwrap();
        } else {
            fs::write(path, binary().unwrap()).unwrap();
        }
        return;
    }
    if operation == "save" || operation == "profile-save" {
        let rounds = if operation == "profile-save" { 200 } else { 1 };
        let start = Instant::now();
        let mut len = 0;
        for _ in 0..rounds {
            len = if bytes.starts_with(b"PXR-USDC") {
                black_box(binary().unwrap()).len()
            } else {
                black_box(text().unwrap()).len()
            };
        }
        println!("{read_ns} {} {len}", start.elapsed().as_nanos() / rounds);
        return;
    }
    if operation == "verify" {
        let native = import(&bytes, &store, &mut Properties::new("native"), "native");
        assert_eq!(
            text().unwrap(),
            layerstack_usda::save_usda(&native, store.tokens(), store.paths()).unwrap(),
            "direct USDA differs"
        );
        assert_eq!(
            binary().unwrap(),
            layerstack_usdc::save_usdc(&native, store.tokens(), store.paths()).unwrap(),
            "direct USDC differs"
        );
        properties.restore(&mut layer);
        assert_eq!(native, layer, "reconstructed layer differs in {mode}");
        return;
    }
    let mut keys: Vec<_> = if let Some(c) = &properties.columns {
        c.keys().collect()
    } else if let Some(h) = &properties.handles {
        h.keys().collect()
    } else {
        layer
            .prims()
            .iter()
            .flat_map(|(&p, s)| {
                s.properties
                    .iter()
                    .map(move |e| PropertyPath::new(p, e.name))
            })
            .collect()
    };
    keys.sort_unstable();
    if keys.is_empty() {
        return;
    }
    // Deterministic Fisher-Yates permutation before opening the timing window.
    let mut seed = 773_u64;
    for i in (1..keys.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        keys.swap(
            i,
            usize::try_from(seed % u64::try_from(i + 1).unwrap()).unwrap(),
        );
    }
    let t = Instant::now();
    let mut checksum = 0_usize;
    for &key in &keys {
        let (k, v, n) = black_box(properties.query(&layer, black_box(key)));
        checksum = checksum
            .wrapping_add((k as usize) + (v.is_some() as usize))
            .wrapping_add(n);
    }
    let lookup = t.elapsed().as_nanos();
    // Exercise the modal (most frequent) property name, so a one-row column
    // cannot hide the cost of editing a shared column.
    let mut counts = std::collections::BTreeMap::new();
    for k in &keys {
        *counts.entry(k.property()).or_insert(0_usize) += 1;
    }
    let name = counts.into_iter().max_by_key(|(n, c)| (*c, *n)).unwrap().0;
    let key = *keys.iter().find(|k| k.property() == name).unwrap();
    let (kind, value, n) = properties.query(&layer, key);
    let original = value.cloned();
    let t = Instant::now();
    let (old_layer, old_props) = (layer.clone(), properties.clone());
    let snapshot = t.elapsed().as_nanos();
    let t = Instant::now();
    properties.edit(&mut layer, key, kind);
    let edit = t.elapsed().as_nanos();
    assert_eq!(
        old_props.query(&old_layer, key),
        (kind, original.as_ref(), n),
        "the edit changed the held snapshot"
    );
    let observed = properties.query(&layer, key);
    if kind == PropertyKind::Attribute {
        assert_eq!(observed.1, Some(&Value::Blocked), "edited default");
    } else {
        assert_eq!(observed.2, 1, "edited target count");
    }
    if operation == "verify-edit" {
        let mut expected = import(&bytes, &store, &mut Properties::new("native"), "native");
        let mut original_layer = old_layer;
        old_props.restore(&mut original_layer);
        assert_eq!(
            original_layer, expected,
            "snapshot content changed in {mode}"
        );
        Properties::new("native").edit(&mut expected, key, kind);
        assert_eq!(
            layerstack_usda::save::save_usda_from(
                &layer,
                store.tokens(),
                store.paths(),
                &properties
            ),
            layerstack_usda::save_usda(&expected, store.tokens(), store.paths()),
            "edited USDA output differs in {mode}"
        );
        assert_eq!(
            layerstack_usdc::writer::save_usdc_from(
                &layer,
                store.tokens(),
                store.paths(),
                &properties
            ),
            layerstack_usdc::save_usdc(&expected, store.tokens(), store.paths()),
            "edited USDC output differs in {mode}"
        );
        properties.restore(&mut layer);
        assert_eq!(layer, expected, "edited layers differ in {mode}");
    }
    println!(
        "{read_ns} {lookup} {} {snapshot} {edit} {checksum}",
        keys.len()
    );
}
