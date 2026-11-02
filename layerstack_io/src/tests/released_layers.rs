// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`StageDocument::release_unused_layers`]: which layers count as used,
//! what a release reports, and that a host fetching sources
//! asynchronously supplies a released layer again when the stage next
//! asks for it.

use super::*;
use layerstack::{EditTarget, LoadPolicy, PropertyPath, Time, Transaction, Value};

/// A host's byte map that drops a source once a layer read from it no
/// longer needs it ([`Storage::release_source`]), as the browser host
/// does: a source not held is unavailable until supplied again.
#[derive(Debug, Default)]
struct Host {
    supplied: BTreeMap<String, Vec<u8>>,
    reads: Reads,
}

impl Host {
    fn supply(&mut self, identifier: &str, bytes: &str) {
        self.supplied
            .insert(identifier.into(), bytes.as_bytes().to_vec());
    }
}

impl Storage for Host {
    fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
        Ok(asset.into())
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.reads.push(identifier.into());
        self.supplied
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::Unavailable, "not supplied").at(identifier))
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.supplied.insert(identifier.into(), bytes.to_vec());
        Ok(())
    }
    fn release_source(&mut self, identifier: &str) {
        self.supplied.remove(identifier);
    }
}

const ROOT: &str = r#"#usda 1.0
(
    subLayers = [@sub.usda@]
)
def "P" (payload = @payload.usda@</P>) {}
def "Clipped" (
    clips = {
        dictionary default = {
            double2[] active = [(0, 0)]
            asset[] assetPaths = [@clip.usda@]
            asset manifestAssetPath = @manifest.usda@
            string primPath = "/C"
            double2[] times = [(0, 0), (10, 10)]
        }
    }
)
{
    double size
}
"#;
const SUB: &str = "#usda 1.0\ndef \"B\" {\n double y = 2\n}\n";
const PAYLOAD: &str =
    "#usda 1.0\n(\n subLayers = [@payload_sub.usda@]\n)\ndef \"P\" {\n double p = 4\n}\n";
const PAYLOAD_SUB: &str = "#usda 1.0\nover \"P\" {\n double q = 5\n}\n";
const CLIP: &str = "#usda 1.0\ndef \"C\" {\n double size.timeSamples = {\n 0: 1,\n 10: 2,\n }\n}\n";
const MANIFEST: &str = "#usda 1.0\nover \"C\" {\n double size\n}\n";

fn supplied() -> Host {
    let mut host = Host::default();
    for (identifier, text) in [
        ("root.usda", ROOT),
        ("sub.usda", SUB),
        ("payload.usda", PAYLOAD),
        ("payload_sub.usda", PAYLOAD_SUB),
        ("clip.usda", CLIP),
        ("manifest.usda", MANIFEST),
    ] {
        host.supply(identifier, text);
    }
    host
}

fn open() -> StageDocument<Host> {
    StageDocument::open(supplied(), "root.usda", StageOptions::default()).unwrap()
}

fn double(doc: &StageDocument<Host>, attribute: &str) -> Option<f64> {
    doc.view()
        .attribute(attribute)
        .and_then(|a| a.value::<f64>(Time::Default))
}

fn named(doc: &StageDocument<Host>, identifier: &str) -> LayerId {
    doc.find_layer_named(identifier).unwrap().expect(identifier)
}

fn identifiers(released: &ReleasedLayers) -> Vec<&str> {
    let mut identifiers: Vec<&str> = released
        .released
        .iter()
        .map(|layer| layer.identifier.as_str())
        .collect();
    identifiers.sort_unstable();
    identifiers
}

fn unload(doc: &mut StageDocument<Host>, prim: &str) {
    let prim = doc.store_mut().path(prim);
    let (store, live) = doc.parts_mut();
    live.unload(store, prim);
    doc.synchronize();
}

fn load(doc: &mut StageDocument<Host>, prim: &str) {
    let prim = doc.store_mut().path(prim);
    let (store, live) = doc.parts_mut();
    live.load(store, prim, LoadPolicy::WithDescendants);
    doc.synchronize();
    doc.load_requested_assets().unwrap();
}

#[test]
fn a_composed_stage_releases_nothing() {
    let mut doc = open();
    let held = doc.store().layer_count();
    let released = doc.release_unused_layers(&[]);
    assert!(released.is_empty(), "{released:?}");
    assert_eq!(doc.store().layer_count(), held);
}

#[test]
fn an_unloaded_payload_is_released_with_what_it_reads() {
    let mut doc = open();
    unload(&mut doc, "/P");
    let released = doc.release_unused_layers(&[]);
    assert_eq!(identifiers(&released), ["payload.usda", "payload_sub.usda"]);
    assert!(released.kept_dirty.is_empty());
    assert_eq!(
        released.source_bytes(),
        (PAYLOAD.len() + PAYLOAD_SUB.len()) as u64
    );
    assert!(released.released.iter().all(|layer| !layer.muted));
    let payload = named(&doc, "payload.usda");
    assert!(doc.store().layer(payload).is_none());
    // The identifier keeps its id.
    assert_eq!(doc.identifier(payload), Some("payload.usda"));
    assert!(doc.release_unused_layers(&[]).is_empty(), "once");
}

#[test]
fn a_kept_layer_stays_without_what_it_reads() {
    let mut doc = open();
    unload(&mut doc, "/P");
    let payload = named(&doc, "payload.usda");
    let released = doc.release_unused_layers(&[payload]);
    // As an `SdfLayerRefPtr` holds a layer, not the layers it sublayers.
    assert_eq!(identifiers(&released), ["payload_sub.usda"]);
    assert!(doc.store().layer(payload).is_some());
}

#[test]
fn unused_dirty_layers_are_kept_and_reported() {
    let mut doc = open();
    let anonymous = doc.create_anonymous_layer().unwrap();
    let payload = named(&doc, "payload.usda");
    let p = PropertyPath::parse("/P.p", doc.store().tokens(), doc.store().paths()).unwrap();
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(payload).property(p),
        Value::Double(40.0),
    );
    doc.apply(&edit).unwrap();
    unload(&mut doc, "/P");
    let released = doc.release_unused_layers(&[]);
    assert_eq!(identifiers(&released), ["payload_sub.usda"]);
    // In id order.
    assert_eq!(released.kept_dirty, [anonymous, payload]);
    load(&mut doc, "/P");
    assert_eq!(double(&doc, "/P.p"), Some(40.0), "the edit is kept");
    // This host dropped the released sublayer's bytes: it supplies them
    // again, and the layer is read again.
    assert_eq!(double(&doc, "/P.q"), None);
    doc.storage_mut().supply("payload_sub.usda", PAYLOAD_SUB);
    doc.load_requested_assets().unwrap();
    assert_eq!(double(&doc, "/P.q"), Some(5.0), "read again");
}

#[test]
fn clip_layers_held_for_queries_are_kept() {
    let mut doc = open();
    let size =
        PropertyPath::parse("/Clipped.size", doc.store().tokens(), doc.store().paths()).unwrap();
    let prepared = doc.prepare_clip_query(size, Time::at(0.0), 8, 32);
    assert!(prepared.plan.is_complete(), "{prepared:?}");
    let clip = named(&doc, "clip.usda");
    assert!(doc.store().layer(clip).is_some());
    let released = doc.release_unused_layers(&[]);
    assert!(released.is_empty(), "{released:?}");
    let value = doc
        .view()
        .attribute(size)
        .and_then(|a| a.value::<f64>(Time::at(0.0)));
    assert_eq!(value, Some(1.0));
}

#[test]
fn a_host_supplies_a_released_payload_again() {
    let mut doc = open();
    unload(&mut doc, "/P");
    doc.release_unused_layers(&[]);
    assert!(
        !doc.storage().supplied.contains_key("payload.usda"),
        "bytes released"
    );
    doc.storage().reads.clear();
    load(&mut doc, "/P");
    let wanted: Vec<String> = doc
        .unavailable_assets()
        .into_iter()
        .map(|asset| asset.identifier)
        .collect();
    assert_eq!(wanted, ["payload.usda"]);
    assert_eq!(double(&doc, "/P.p"), None);
    doc.storage_mut()
        .supply("payload.usda", &PAYLOAD.replace("p = 4", "p = 44"));
    doc.load_requested_assets().unwrap();
    let wanted: Vec<String> = doc
        .unavailable_assets()
        .into_iter()
        .map(|asset| asset.identifier)
        .collect();
    assert_eq!(wanted, ["payload_sub.usda"]);
    doc.storage_mut().supply("payload_sub.usda", PAYLOAD_SUB);
    doc.load_requested_assets().unwrap();
    assert!(doc.unavailable_assets().is_empty());
    assert_eq!(double(&doc, "/P.p"), Some(44.0));
    assert_eq!(double(&doc, "/P.q"), Some(5.0));
    assert!(!doc.is_dirty(doc.root_layer()), "the root stays clean");
}

#[test]
fn a_host_supplies_a_released_muted_layer_on_unmute() {
    let mut doc = open();
    let sub = named(&doc, "sub.usda");
    doc.mute_and_unmute_layers(&[sub], &[]).unwrap();
    let released = doc.release_unused_layers(&[]);
    assert_eq!(identifiers(&released), ["sub.usda"]);
    assert!(released.released[0].muted);
    assert_eq!(doc.muted_identifiers(), ["sub.usda"]);
    doc.mute_and_unmute_layers(&[], &[sub]).unwrap();
    doc.load_requested_assets().unwrap();
    let wanted: Vec<String> = doc
        .unavailable_assets()
        .into_iter()
        .map(|asset| asset.identifier)
        .collect();
    assert_eq!(wanted, ["sub.usda"]);
    doc.storage_mut()
        .supply("sub.usda", &SUB.replace("y = 2", "y = 20"));
    doc.load_requested_assets().unwrap();
    assert_eq!(double(&doc, "/B.y"), Some(20.0));
    assert_eq!(
        doc.store().layer(sub).map(Layer::id),
        Some(sub),
        "the same id"
    );
    assert!(
        !doc.is_dirty(doc.root_layer()),
        "bindings are not authored content"
    );
}
