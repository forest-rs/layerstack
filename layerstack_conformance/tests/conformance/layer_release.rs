// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Releasing the layers a stage stops using, and reading their files again
//! when it uses them again, as OpenUSD 26.11 frees and reopens them.
//!
//! `scripts/layer_release_oracle.py` records, per case, a stage on
//! `fixtures/layer_release/layers/root.usda` that stops using a layer
//! (unloads a payload, mutes a sublayer, removes a sublayer, clears a
//! reference), has the layer's file rewritten, and uses it again; the
//! `_dirty` cases edit the layer in memory first. After each step it
//! records the composed values, which layers are open and dirty, and the
//! notices. See the script for the 26.11 code each shows.
//!
//! Here each case replays against a `StageDocument` on in-memory files,
//! calling `StageDocument::release_unused_layers` where OpenUSD frees the
//! layers: right after the step that drops the stage's use. Values, open
//! layers and notices must match OpenUSD, and the release itself must
//! change nothing composed and send no notice.
//!
//! One difference is deliberate: OpenUSD frees an unused layer with edits
//! not saved and loses them, while a release keeps it, lists it in
//! `ReleasedLayers::kept_dirty`, and the stage composes the edits when it
//! uses the layer again. The `_dirty` cases check that, then check that
//! discarding the edits (`ReloadPolicy::DiscardDirty`) and releasing again
//! gives OpenUSD's result.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use layerstack::{
    EditTarget, LayerId, LayerOffset, ListPosition, LoadPolicy, Reference, ReferenceTarget,
    StageOptions, SublayerEntry, Transaction, Value,
};
use layerstack_io::{IoError, IoErrorKind, ReleasedLayers, ReloadPolicy, StageDocument, Storage};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/layer_release/oracle.json");

/// The fixture's layers, as the oracle wrote them.
const LAYERS: [(&str, &str); 6] = [
    (
        "root.usda",
        include_str!("../../fixtures/layer_release/layers/root.usda"),
    ),
    (
        "sub.usda",
        include_str!("../../fixtures/layer_release/layers/sub.usda"),
    ),
    (
        "extra.usda",
        include_str!("../../fixtures/layer_release/layers/extra.usda"),
    ),
    (
        "payload.usda",
        include_str!("../../fixtures/layer_release/layers/payload.usda"),
    ),
    (
        "payload_sub.usda",
        include_str!("../../fixtures/layer_release/layers/payload_sub.usda"),
    ),
    (
        "ref.usda",
        include_str!("../../fixtures/layer_release/layers/ref.usda"),
    ),
];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

/// The scene's files, in memory, rewritable between steps, and the
/// identifiers read from them, in order.
#[derive(Debug, Default)]
struct Files {
    files: BTreeMap<String, Vec<u8>>,
    reads: Mutex<Vec<String>>,
}

impl Storage for Files {
    fn identify(&self, asset: &str, _anchor: Option<&str>) -> Result<String, IoError> {
        Ok(asset.trim_start_matches("./").to_owned())
    }

    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.reads.lock().unwrap().push(identifier.to_owned());
        self.files
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, "no such file"))
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.files.insert(identifier.to_owned(), bytes.to_vec());
        Ok(())
    }
}

/// The paths of the notices a stage sent, as the oracle records them.
type Heard = Arc<Mutex<Vec<Json>>>;

struct Case {
    doc: StageDocument<Files>,
    heard: Heard,
}

impl Case {
    fn open() -> Self {
        let files = Files {
            files: LAYERS
                .iter()
                .map(|(name, text)| ((*name).to_owned(), text.as_bytes().to_vec()))
                .collect(),
            reads: Mutex::default(),
        };
        let mut doc = StageDocument::open(files, "root.usda", StageOptions::default()).unwrap();
        let heard: Heard = Arc::default();
        let sink = Arc::clone(&heard);
        let (_, live) = doc.parts_mut();
        live.subscribe_changes(move |notice| {
            let (paths, tokens) = (notice.store.paths(), notice.store.tokens());
            let mut resynced: Vec<String> = notice
                .changes
                .resynced
                .iter()
                .map(|p| paths.display(*p, tokens))
                .chain(
                    notice
                        .changes
                        .resynced_properties
                        .iter()
                        .map(|p| p.display(paths, tokens)),
                )
                .collect();
            resynced.sort();
            let mut info: Vec<String> = notice
                .changes
                .changed_info_only
                .iter()
                .map(|p| paths.display(*p, tokens))
                .collect();
            for change in &notice.changes.changed_properties {
                info.push(change.property.display(paths, tokens));
            }
            info.sort();
            info.dedup();
            if !resynced.is_empty() || !info.is_empty() {
                sink.lock()
                    .unwrap()
                    .push(json!({"resynced": resynced, "info": info}));
            }
        });
        Self { doc, heard }
    }

    fn layer(&self, name: &str) -> LayerId {
        self.doc.find_layer_named(name).unwrap().expect(name)
    }

    fn take_heard(&self) -> Vec<Json> {
        std::mem::take(&mut *self.heard.lock().unwrap())
    }

    fn take_reads(&self) -> Vec<String> {
        std::mem::take(&mut *self.doc.storage().reads.lock().unwrap())
    }

    fn value(&mut self, path: &str) -> Json {
        let path = self.doc.store_mut().property_path(path);
        self.doc
            .view()
            .attribute(path)
            .and_then(|attribute| attribute.value::<f64>(layerstack::Time::Default))
            .map_or(Json::Null, |value| json!(value))
    }

    /// Each fixture layer as the oracle records it: `freed`, `open` or
    /// `open dirty`.
    fn layers(&self) -> Json {
        let mut state = serde_json::Map::new();
        for (name, _) in LAYERS {
            let held = self
                .doc
                .find_layer_named(name)
                .unwrap()
                .filter(|id| self.doc.store().layer(*id).is_some());
            let text = match held {
                None => "freed",
                Some(id) if self.doc.is_dirty(id) => "open dirty",
                Some(_) => "open",
            };
            state.insert(name.to_owned(), json!(text));
        }
        Json::Object(state)
    }

    fn record(&mut self, step: &Json, attributes: &[String]) -> Json {
        let values: serde_json::Map<String, Json> = attributes
            .iter()
            .map(|path| (path.clone(), self.value(path)))
            .collect();
        json!({
            "step": step,
            "values": values,
            "layers": self.layers(),
            "heard": self.take_heard(),
        })
    }

    fn set_default(&mut self, layer: &str, attribute: &str, value: f64) {
        let layer = self.layer(layer);
        let property = self.doc.store_mut().property_path(attribute);
        let mut edit = Transaction::new();
        edit.set_default(
            EditTarget::for_layer(layer).property(property),
            Value::Double(value),
        );
        self.doc.apply(&edit).unwrap();
    }

    fn take_step(&mut self, step: &Json) {
        let step: Vec<&str> = step
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        let root = self.doc.root_layer();
        match step[..] {
            ["unload", prim] => {
                let prim = self.doc.store_mut().path(prim);
                let (store, live) = self.doc.parts_mut();
                live.unload(store, prim);
                self.doc.synchronize();
            }
            ["load", prim] => {
                let prim = self.doc.store_mut().path(prim);
                let (store, live) = self.doc.parts_mut();
                live.load(store, prim, LoadPolicy::WithDescendants);
                self.doc.synchronize();
                self.doc.load_requested_assets().unwrap();
            }
            ["mute", layer] => {
                let layer = self.doc.layer_named(layer).unwrap();
                self.doc.mute_and_unmute_layers(&[layer], &[]).unwrap();
            }
            ["unmute", layer] => {
                let layer = self.doc.layer_named(layer).unwrap();
                self.doc.mute_and_unmute_layers(&[], &[layer]).unwrap();
                self.doc.load_requested_assets().unwrap();
            }
            ["remove_sublayer", asset] => {
                let index = self
                    .doc
                    .store()
                    .layer(root)
                    .unwrap()
                    .sublayers()
                    .iter()
                    .position(|entry| entry.asset.as_deref() == Some(asset))
                    .unwrap();
                let mut edit = Transaction::new();
                edit.remove_sublayer(root, index);
                self.doc.apply(&edit).unwrap();
            }
            ["append_sublayer", asset] => {
                let count = self.doc.store().layer(root).unwrap().sublayers().len();
                let mut edit = Transaction::new();
                edit.insert_sublayer(
                    root,
                    count,
                    SublayerEntry::unresolved(asset, LayerOffset::IDENTITY),
                );
                self.doc.apply(&edit).unwrap();
            }
            ["clear_references", prim] => {
                let prim = self.doc.store_mut().path(prim);
                let mut edit = Transaction::new();
                edit.clear_references(EditTarget::for_layer(root).prim(prim));
                self.doc.apply(&edit).unwrap();
            }
            ["add_reference", prim, asset, target] => {
                let prim = self.doc.store_mut().path(prim);
                let target = self.doc.store_mut().path(target);
                let mut edit = Transaction::new();
                edit.add_reference(
                    EditTarget::for_layer(root).prim(prim),
                    Reference::deferred(
                        root,
                        asset,
                        ReferenceTarget::Prim(target),
                        LayerOffset::IDENTITY,
                    ),
                    ListPosition::BackOfPrepend,
                );
                self.doc.apply(&edit).unwrap();
            }
            _ => panic!("unknown step {step:?}"),
        }
    }

    /// Releases what OpenUSD frees after a step, checking that the release
    /// changes nothing composed and sends no notice.
    fn release(&mut self, attributes: &[String]) -> ReleasedLayers {
        let before: Vec<Json> = attributes.iter().map(|path| self.value(path)).collect();
        assert_eq!(
            self.take_heard(),
            Vec::<Json>::new(),
            "notices before release"
        );
        let released = self.doc.release_unused_layers(&[]);
        self.doc.synchronize();
        assert_eq!(
            self.take_heard(),
            Vec::<Json>::new(),
            "a release sends no notice"
        );
        let after: Vec<Json> = attributes.iter().map(|path| self.value(path)).collect();
        assert_eq!(before, after, "a release changes nothing composed");
        released
    }
}

/// The identifiers of the layers released, sorted.
fn names(released: &ReleasedLayers) -> Vec<&str> {
    let mut names: Vec<&str> = released
        .released
        .iter()
        .map(|layer| layer.identifier.as_str())
        .collect();
    names.sort_unstable();
    names
}

/// The fixture layers OpenUSD freed at `step`, sorted.
fn freed(step: &Json) -> Vec<&str> {
    step["layers"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, state)| *state == "freed")
        .map(|(name, _)| name.as_str())
        .collect()
}

#[test]
fn clean_layers_are_released_and_read_again() {
    let oracle = oracle();
    let rewrites = oracle["rewrites"].as_object().unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if name.ends_with("_dirty") {
            continue;
        }
        let steps = case["steps"].as_array().unwrap();
        let attributes: Vec<String> = steps[0]["values"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut run = Case::open();
        run.take_reads();
        let mut replayed = Vec::new();
        for step in steps {
            let call = &step["step"];
            match call[0].as_str().unwrap() {
                "open" => {}
                "rewrite" => {
                    for layer in call.as_array().unwrap()[1..].iter() {
                        let layer = layer.as_str().unwrap();
                        let text = rewrites[layer].as_str().unwrap();
                        run.doc.storage_mut().write(layer, text.as_bytes()).unwrap();
                    }
                }
                _ => {
                    run.take_step(call);
                    if !freed(step).is_empty() {
                        let heard = run.take_heard();
                        let released = run.release(&attributes);
                        assert_eq!(names(&released), freed(step), "{name}: released");
                        assert!(released.kept_dirty.is_empty(), "{name}: kept dirty");
                        assert!(released.source_bytes() > 0, "{name}: bytes");
                        run.heard.lock().unwrap().extend(heard);
                    }
                }
            }
            let mut step = run.record(call, &attributes);
            // A payload loaded again is heard twice, as any payload whose
            // layers the document reads on load is: once as the stage
            // composes without them and asks for them, once as they
            // arrive. OpenUSD reads them as it composes, and is heard once.
            if let Some(heard) = step["heard"].as_array_mut() {
                heard.dedup();
            }
            // A sublayer or reference edit is heard at another grain than
            // OpenUSD's whether or not the layer was released (the
            // `change_evidence` tests cover edits), and a layer read again
            // as a sublayer is heard as any sublayer newly read is, with
            // the whole stage resynced: only the payload and muting steps'
            // notices are compared.
            if !matches!(
                call[0].as_str(),
                Some("load" | "unload" | "mute" | "unmute" | "open" | "rewrite")
            ) {
                step["heard"] = Json::Null;
            }
            replayed.push(step);
        }
        let mut expected = case["steps"].clone();
        for (step, replayed) in expected.as_array_mut().unwrap().iter_mut().zip(&replayed) {
            if replayed["heard"].is_null() {
                step["heard"] = Json::Null;
            }
        }
        assert_eq!(Json::Array(replayed), expected, "{name}");
        // The use after the rewrite read each freed layer's file again.
        let mut read = run.take_reads();
        read.sort();
        let mut rewritten: Vec<String> = steps
            .iter()
            .filter(|step| step["step"][0] == "rewrite")
            .flat_map(|step| step["step"].as_array().unwrap()[1..].to_vec())
            .map(|layer| layer.as_str().unwrap().to_owned())
            .collect();
        rewritten.sort();
        assert_eq!(read, rewritten, "{name}: files read again");
    }
}

/// OpenUSD frees an unused dirty layer and loses its edits; a release
/// keeps it and reports it, and the stage composes the edits when it uses
/// the layer again. Discarding the edits and releasing again gives
/// OpenUSD's result.
#[test]
fn dirty_layers_are_kept_and_reported() {
    let oracle = oracle();
    for case in oracle["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if !name.ends_with("_dirty") {
            continue;
        }
        let steps = case["steps"].as_array().unwrap();
        let attributes: Vec<String> = steps[0]["values"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let edit = steps[1]["step"].as_array().unwrap();
        let (layer, attribute, edited) = (
            edit[1].as_str().unwrap(),
            edit[2].as_str().unwrap(),
            edit[3].as_f64().unwrap(),
        );
        let (drop, used) = (&steps[2], &steps[3]);
        // Reloading skips a muted layer, so a muted one's edits are not
        // discarded that way; the host unmutes it first.
        let discards: &[bool] = if drop["step"][0] == "mute" {
            &[false]
        } else {
            &[false, true]
        };
        for &discard in discards {
            let mut run = Case::open();
            run.set_default(layer, attribute, edited);
            // Only values and layers: the edit's notice is not what this
            // checks.
            let edited_step = run.record(&Json::Array(edit.clone()), &attributes);
            assert_eq!(edited_step["values"], steps[1]["values"], "{name}");
            assert_eq!(edited_step["layers"], steps[1]["layers"], "{name}");
            run.take_step(&drop["step"]);
            let heard = run.take_heard();
            if matches!(drop["step"][0].as_str(), Some("unload" | "mute")) {
                assert_eq!(heard, drop["heard"].as_array().unwrap().clone(), "{name}");
            }
            let id = run.layer(layer);
            let released = run.release(&attributes);
            assert_eq!(released.kept_dirty, vec![id], "{name}: kept dirty");
            // The rest OpenUSD freed with it is released.
            let mut others = freed(drop);
            others.retain(|freed| *freed != layer);
            assert_eq!(names(&released), others, "{name}: released");
            assert!(run.doc.store().layer(id).is_some(), "{name}: kept");
            if discard {
                run.doc
                    .reload_layers(&[id], ReloadPolicy::DiscardDirty)
                    .unwrap();
                // Reading the file again is heard as any reload is.
                run.take_heard();
                let released = run.release(&attributes);
                // Reading it again read what it names, which goes too.
                assert!(
                    names(&released).contains(&layer),
                    "{name}: released once clean: {released:?} dirty {}",
                    run.doc.is_dirty(id)
                );
                assert!(released.kept_dirty.is_empty());
            }
            run.take_step(&used["step"]);
            let got = run.value(attribute);
            if discard {
                // OpenUSD's result: the edit is lost.
                assert_eq!(got, used["values"][attribute], "{name}: discarded");
                assert_eq!(
                    run.record(&used["step"], &attributes)["values"],
                    used["values"]
                );
            } else {
                // The deliberate difference: the edit is kept.
                assert_eq!(got, json!(edited), "{name}: edit kept");
                assert_ne!(got, used["values"][attribute], "{name}: OpenUSD lost it");
                assert!(run.doc.is_dirty(id), "{name}: still dirty");
            }
        }
    }
}
