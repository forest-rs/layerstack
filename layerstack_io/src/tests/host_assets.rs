// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A host that fetches sources asynchronously, such as a browser, supplies
//! them as they arrive: storage reports one it has not received as
//! [`IoErrorKind::Unavailable`], the document lists what the stage wants
//! ([`StageDocument::unavailable_assets`]), and
//! [`StageDocument::load_requested_assets`] reads what was supplied
//! without reading resident layers again.

use super::*;
use core::sync::atomic::{AtomicUsize, Ordering};
use layerstack::{
    AssetUse, LayerOffset, PropertyPath, PropertyType, SublayerEntry, Time, Transaction,
};
use layerstack_usdz::PackageFile;

/// A host's byte map: a source it has not been given is unavailable, one
/// it confirmed absent is not found.
#[derive(Debug, Default)]
struct Host {
    supplied: BTreeMap<String, Vec<u8>>,
    absent: BTreeSet<String>,
    /// Sources whose read fails ([`IoErrorKind::Storage`]).
    broken: BTreeSet<String>,
    reads: Reads,
}

impl Host {
    fn supply(&mut self, identifier: &str, bytes: &[u8]) {
        self.supplied.insert(identifier.into(), bytes.to_vec());
    }
}

impl Storage for Host {
    fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
        Ok(asset.into())
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.reads.push(identifier.into());
        if let Some(bytes) = self.supplied.get(identifier) {
            return Ok(bytes.clone());
        }
        let kind = if self.absent.contains(identifier) {
            IoErrorKind::NotFound
        } else if self.broken.contains(identifier) {
            IoErrorKind::Storage
        } else {
            IoErrorKind::Unavailable
        };
        Err(IoError::new(kind, format!("{identifier} not supplied")))
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.supply(identifier, bytes);
        Ok(())
    }
}

const ROOT: &str = r#"#usda 1.0
(
    subLayers = [@sub.usda@]
)
def "Child" (prepend references = @child.usda@</Asset>) {}
def "Model" (
    variants = { string look = "a" }
    prepend variantSets = "look"
) {
    variantSet "look" = {
        "a" (prepend references = @a.usda@</Look>) {}
        "b" (prepend references = @b.usda@</Look>) {}
    }
}
def "Gone" (prepend references = @gone.usda@</Asset>) {}
"#;

const SUB: &str = "#usda 1.0\ndef \"FromSub\" {}\n";
const CHILD: &str =
    "#usda 1.0\ndef \"Asset\" (prepend references = @grand.usda@</Grand>) {\n int child = 1\n}\n";
const GRAND: &str = "#usda 1.0\ndef \"Grand\" {\n int grand = 2\n}\n";

fn look(name: &str) -> String {
    format!("#usda 1.0\ndef \"Look\" {{\n string name = \"{name}\"\n}}\n")
}

/// Opens `root.usda`, the only source supplied, with `gone.usda`
/// confirmed absent.
fn open() -> StageDocument<Host> {
    let mut host = Host::default();
    host.supply("root.usda", ROOT.as_bytes());
    host.absent.insert("gone.usda".into());
    StageDocument::open(host, "root.usda", StageOptions::default()).unwrap()
}

/// The wanted list as (identifier, anchor identifier, arc), sorted: the
/// document lists it by anchor layer id, which a host does not choose.
fn wanted(doc: &StageDocument<Host>) -> Vec<(String, String, AssetUse)> {
    let mut wanted: Vec<(String, String, AssetUse)> = doc
        .unavailable_assets()
        .into_iter()
        .map(|asset| {
            assert_eq!(
                asset.identifier, asset.asset_path,
                "the host identifies paths as written"
            );
            let anchor = doc.identifier(asset.anchor).unwrap_or("anonymous").into();
            (asset.identifier, anchor, asset.used_by)
        })
        .collect();
    wanted.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    wanted
}

fn want(identifier: &str, anchor: &str, arc: AssetUse) -> (String, String, AssetUse) {
    (identifier.into(), anchor.into(), arc)
}

fn int(doc: &StageDocument<Host>, attribute: &str) -> Option<i32> {
    doc.view()
        .attribute(attribute)
        .and_then(|a| a.value::<i32>(Time::Default))
}

fn look_name(doc: &StageDocument<Host>) -> Option<String> {
    doc.view()
        .attribute("/Model.name")
        .and_then(|a| a.value::<String>(Time::Default))
}

fn missing(doc: &StageDocument<Host>) -> Vec<(String, AssetUse)> {
    doc.asset_status()
        .unresolved
        .into_iter()
        .map(|asset| (asset.asset_path, asset.used_by))
        .collect()
}

/// Opening with only the root composes what it has and lists the rest,
/// sublayer included, as wanted, not missing; a source confirmed absent is
/// missing and not wanted.
#[test]
fn opening_with_only_the_root_lists_what_the_stage_wants() {
    let doc = open();
    assert_eq!(
        wanted(&doc),
        [
            want("a.usda", "root.usda", AssetUse::Reference),
            want("child.usda", "root.usda", AssetUse::Reference),
            want("sub.usda", "root.usda", AssetUse::Sublayer),
        ]
    );
    assert!(doc.view().prim("/Child").is_some());
    assert!(doc.view().prim("/FromSub").is_none());
    // Each lists the prims composed without it: those whose arcs name it,
    // the pseudo-root for the stage's own sublayer.
    let store = doc.store();
    let mut waiting: Vec<(String, Vec<String>)> = doc
        .unavailable_assets()
        .into_iter()
        .map(|asset| {
            let prims = asset
                .prims
                .iter()
                .map(|&prim| store.paths().display(prim, store.tokens()))
                .collect();
            (asset.asset_path, prims)
        })
        .collect();
    waiting.sort();
    assert_eq!(
        waiting,
        [
            ("a.usda".into(), vec!["/Model".into()]),
            ("child.usda".into(), vec!["/Child".into()]),
            ("sub.usda".into(), vec!["/".into()]),
        ]
    );
    assert_eq!(missing(&doc), [("gone.usda".into(), AssetUse::Reference)]);
    let status = doc.asset_status();
    assert_eq!(status.unavailable, doc.unavailable_assets());
    assert!(!status.is_complete());
    let [gone] = &status.unresolved[..] else {
        panic!("{:?}", status.unresolved);
    };
    assert_eq!(gone.identifier.as_deref(), Some("gone.usda"));
    assert_eq!(gone.reason, Some(IoErrorKind::NotFound));
    assert!(gone.is_missing());
    // The only failure is the confirmed absence; nothing unavailable is
    // reported as failed, and nothing failed the open.
    let failures: Vec<&str> = doc
        .load_report()
        .asset_failures
        .iter()
        .map(|failure| failure.asset_path.as_str())
        .collect();
    assert_eq!(failures, ["gone.usda"]);
}

/// A source whose read fails is unresolved, not missing, under recovery:
/// its arc contributes nothing, and the status names the source and the
/// failure.
#[test]
fn a_source_that_fails_to_read_is_unresolved_with_its_reason() {
    let mut host = Host::default();
    host.supply("root.usda", ROOT.as_bytes());
    host.broken.insert("gone.usda".into());
    let loading = LoadOptions {
        import_policy: ImportPolicy::AllowRecovery,
        ..LoadOptions::default()
    };
    let doc = StageDocument::open(
        host,
        "root.usda",
        OpenOptions {
            loading,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let status = doc.asset_status();
    let [gone] = &status.unresolved[..] else {
        panic!("{:?}", status.unresolved);
    };
    assert_eq!(gone.asset_path, "gone.usda");
    assert_eq!(gone.used_by, AssetUse::Reference);
    assert_eq!(gone.identifier.as_deref(), Some("gone.usda"));
    assert_eq!(gone.reason, Some(IoErrorKind::Storage));
    assert!(!gone.is_missing());
}

/// Supplying some sources and loading composes them and leaves the rest
/// wanted; a supplied layer that wants more adds to the list, round after
/// round, until the stage lacks only what is confirmed missing. No
/// resident layer is read again.
#[test]
fn supplied_sources_load_round_by_round() {
    let mut doc = open();
    doc.storage_mut().supply("sub.usda", SUB.as_bytes());
    doc.storage_mut().supply("child.usda", CHILD.as_bytes());
    let loaded = doc.load_requested_assets().unwrap();
    assert_eq!(loaded.layers.len(), 2);
    assert!(loaded.asset_failures.is_empty());
    assert!(doc.view().prim("/FromSub").is_some());
    assert_eq!(int(&doc, "/Child.child"), Some(1));
    assert_eq!(
        wanted(&doc),
        [
            want("a.usda", "root.usda", AssetUse::Reference),
            want("grand.usda", "child.usda", AssetUse::Reference),
        ]
    );

    doc.storage_mut().supply("grand.usda", GRAND.as_bytes());
    doc.storage_mut().supply("a.usda", look("a").as_bytes());
    let before = doc.storage().reads.iter().count();
    doc.load_requested_assets().unwrap();
    let mut reads: Vec<String> = doc.storage().reads.iter().skip(before).collect();
    reads.sort();
    assert_eq!(reads, ["a.usda", "grand.usda"]);
    assert_eq!(int(&doc, "/Child.grand"), Some(2));
    assert_eq!(look_name(&doc).as_deref(), Some("a"));
    assert!(wanted(&doc).is_empty());
    let status = doc.asset_status();
    assert!(status.unavailable.is_empty());
    assert_eq!(missing(&doc), [("gone.usda".into(), AssetUse::Reference)]);

    // Nothing left to read: loading again reads nothing.
    let before = doc.storage().reads.iter().count();
    let loaded = doc.load_requested_assets().unwrap();
    assert!(loaded.layers.is_empty());
    assert_eq!(doc.storage().reads.iter().count(), before);
}

/// The stage flattened to USDA text, for comparing two compositions.
fn flattened(doc: &mut StageDocument<Host>) -> String {
    use layerstack::stage::flatten::{AssetPaths, FlattenRequirements, LossPolicy};
    let requirements = FlattenRequirements {
        losses: LossPolicy::RefuseRequired,
        asset_paths: AssetPaths::AsAuthored,
        ..FlattenRequirements::default()
    };
    let flat = doc.flatten(&requirements).unwrap();
    String::from_utf8(doc.encode_layer(&flat.layer, "flat.usda").unwrap()).unwrap()
}

/// Counts the stage's compositions: one change notice each.
fn count_compositions(doc: &mut StageDocument<Host>) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let (_, stage) = doc.parts_mut();
    // The subscription lives as long as the stage.
    let _subscription = stage.subscribe_changes(move |_| {
        seen.fetch_add(1, Ordering::Relaxed);
    });
    count
}

/// Reading ahead reads each round's sources without composing: the layers
/// read list what they need themselves, as an open whose storage held
/// every source reads them, until nothing is unavailable. One composition
/// then builds the stage that composing after every round builds, and
/// reads nothing more.
#[test]
fn reading_ahead_reads_every_round_and_composes_once() {
    let mut ahead = open();
    let compositions = count_compositions(&mut ahead);
    ahead.storage_mut().supply("sub.usda", SUB.as_bytes());
    ahead.storage_mut().supply("child.usda", CHILD.as_bytes());
    let read = ahead.read_requested_assets().unwrap();
    assert_eq!(read.layers.len(), 2);
    assert_eq!(read.reads.len(), 2);
    assert!(read.compositions.is_empty());
    // The stage is as it composed at the open; the layer read asks for
    // `grand.usda` itself, with no prim composed without it yet.
    assert!(ahead.view().prim("/FromSub").is_none());
    assert_eq!(int(&ahead, "/Child.child"), None);
    assert_eq!(
        wanted(&ahead),
        [
            want("a.usda", "root.usda", AssetUse::Reference),
            want("grand.usda", "child.usda", AssetUse::Reference),
        ]
    );
    let grand = ahead
        .unavailable_assets()
        .into_iter()
        .find(|asset| asset.asset_path == "grand.usda")
        .unwrap();
    assert!(grand.prims.is_empty());

    ahead.storage_mut().supply("grand.usda", GRAND.as_bytes());
    ahead.storage_mut().supply("a.usda", look("a").as_bytes());
    let read = ahead.read_requested_assets().unwrap();
    assert_eq!(read.layers.len(), 2);
    assert!(wanted(&ahead).is_empty());
    assert_eq!(compositions.load(Ordering::Relaxed), 0);

    let before = ahead.storage().reads.iter().count();
    let loaded = ahead.load_requested_assets().unwrap();
    assert!(loaded.layers.is_empty() && loaded.reads.is_empty());
    // One composition, of the four layers read ahead.
    let [composition] = &loaded.compositions[..] else {
        panic!("{:?}", loaded.compositions);
    };
    assert_eq!(composition.layers, 4);
    assert_eq!(ahead.storage().reads.iter().count(), before);
    assert_eq!(compositions.load(Ordering::Relaxed), 1);
    assert!(wanted(&ahead).is_empty());
    assert_eq!(int(&ahead, "/Child.grand"), Some(2));
    assert_eq!(look_name(&ahead).as_deref(), Some("a"));
    assert_eq!(missing(&ahead), [("gone.usda".into(), AssetUse::Reference)]);

    let mut rounds = open();
    for sources in [
        [("sub.usda", SUB), ("child.usda", CHILD)],
        [("grand.usda", GRAND), ("a.usda", &look("a"))],
    ] {
        for (identifier, text) in sources {
            rounds.storage_mut().supply(identifier, text.as_bytes());
        }
        rounds.load_requested_assets().unwrap();
    }
    assert_eq!(flattened(&mut ahead), flattened(&mut rounds));
}

/// A layer read ahead says how it uses each source it needs: as a
/// sublayer, a payload or a reference.
#[test]
fn reading_ahead_lists_how_each_layer_read_uses_its_sources() {
    let mut doc = open();
    doc.storage_mut().supply(
        "child.usda",
        b"#usda 1.0\n(\n    subLayers = [@deep_sub.usda@]\n)\ndef \"Asset\" (\n    prepend payload = @deep_payload.usda@</P>\n    prepend references = @deep_reference.usda@</R>\n) {}\n",
    );
    doc.read_requested_assets().unwrap();
    assert_eq!(
        wanted(&doc),
        [
            want("a.usda", "root.usda", AssetUse::Reference),
            want("deep_payload.usda", "child.usda", AssetUse::Payload),
            want("deep_reference.usda", "child.usda", AssetUse::Reference),
            want("deep_sub.usda", "child.usda", AssetUse::Sublayer),
            want("sub.usda", "root.usda", AssetUse::Sublayer),
        ]
    );
    // Composing replaces them with the stage's own requests, which name
    // the prims composed without them.
    doc.load_requested_assets().unwrap();
    let mut prims: Vec<(String, AssetUse, Vec<String>)> = doc
        .unavailable_assets()
        .into_iter()
        .map(|asset| {
            let store = doc.store();
            let prims = asset
                .prims
                .iter()
                .map(|&prim| store.paths().display(prim, store.tokens()))
                .collect();
            (asset.asset_path, asset.used_by, prims)
        })
        .collect();
    prims.sort();
    assert_eq!(
        prims,
        [
            ("a.usda".into(), AssetUse::Reference, vec!["/Model".into()]),
            (
                "deep_payload.usda".into(),
                AssetUse::Payload,
                vec!["/Child".into()]
            ),
            (
                "deep_reference.usda".into(),
                AssetUse::Reference,
                vec!["/Child".into()]
            ),
            (
                "deep_sub.usda".into(),
                AssetUse::Sublayer,
                vec!["/Child".into()]
            ),
            ("sub.usda".into(), AssetUse::Sublayer, vec!["/".into()]),
        ]
    );
}

/// A strict rejection drops the round it ends, and what that round read
/// stays wanted: supplying the rejected source anew reads both.
#[test]
fn a_rejected_read_ahead_keeps_the_round_wanted() {
    const PAIR: &str = "#usda 1.0\ndef \"Asset\" (\n    prepend references = [@fine.usda@</Look>, @zbad.usda@</Look>]\n) {}\n";
    let mut doc = open();
    doc.storage_mut().supply("child.usda", PAIR.as_bytes());
    doc.read_requested_assets().unwrap();
    doc.storage_mut()
        .supply("fine.usda", look("fine").as_bytes());
    doc.storage_mut().broken.insert("zbad.usda".into());
    let error = doc.read_requested_assets().unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Storage);
    let wanted_now = wanted(&doc);
    assert!(wanted_now.contains(&want("fine.usda", "child.usda", AssetUse::Reference)));
    assert!(wanted_now.contains(&want("zbad.usda", "child.usda", AssetUse::Reference)));

    doc.storage_mut().broken.clear();
    doc.storage_mut()
        .supply("zbad.usda", look("zbad").as_bytes());
    let read: Vec<String> = doc
        .read_requested_assets()
        .unwrap()
        .layers
        .iter()
        .filter_map(|layer| doc.identifier(*layer).map(String::from))
        .collect();
    assert!(read.contains(&"fine.usda".into()) && read.contains(&"zbad.usda".into()));
}

/// What the layers read name stops being wanted once the stage composes
/// by any path: an edit that drops the arc that read them drops them too,
/// and loading reads none of them.
#[test]
fn composing_drops_what_the_layers_read_named() {
    let mut doc = open();
    doc.storage_mut().supply("child.usda", CHILD.as_bytes());
    doc.read_requested_assets().unwrap();
    assert!(wanted(&doc).contains(&want("grand.usda", "child.usda", AssetUse::Reference)));

    let at = EditTarget::for_layer(doc.root_layer());
    let child = doc.store_mut().path("/Child");
    let mut edit = Transaction::new();
    edit.clear_references(at.prim(child));
    doc.storage_mut().supply("grand.usda", GRAND.as_bytes());
    let before = doc.storage().reads.iter().count();
    doc.apply(&edit).unwrap();
    assert!(
        !doc.storage()
            .reads
            .iter()
            .skip(before)
            .any(|read| read == "grand.usda")
    );
    assert!(
        !wanted(&doc)
            .iter()
            .any(|(identifier, ..)| identifier == "grand.usda")
    );
}

/// An arc only composition reaches, in a variant a stronger layer selects,
/// is asked for once the stage composes: the read-ahead reads what the
/// layer read selects itself, as an open whose storage held every source
/// would, and composing asks for the rest.
#[test]
fn composing_after_reading_ahead_asks_for_what_only_composition_reaches() {
    const HOLDER_ROOT: &str = r#"#usda 1.0
def "Holder" (
    variants = { string look = "b" }
    prepend references = @holder.usda@</H>
) {}
"#;
    const HOLDER: &str = r#"#usda 1.0
def "H" (
    variants = { string look = "a" }
    prepend variantSets = "look"
) {
    variantSet "look" = {
        "a" (prepend references = @a.usda@</Look>) {}
        "b" (prepend references = @b.usda@</Look>) {}
    }
}
"#;
    let mut host = Host::default();
    host.supply("root.usda", HOLDER_ROOT.as_bytes());
    let mut doc = StageDocument::open(host, "root.usda", StageOptions::default()).unwrap();
    doc.storage_mut().supply("holder.usda", HOLDER.as_bytes());
    doc.read_requested_assets().unwrap();
    assert_eq!(
        wanted(&doc),
        [want("a.usda", "holder.usda", AssetUse::Reference)]
    );
    doc.storage_mut().supply("a.usda", look("a").as_bytes());
    doc.read_requested_assets().unwrap();
    assert!(wanted(&doc).is_empty());
    doc.load_requested_assets().unwrap();
    assert_eq!(
        wanted(&doc),
        [want("b.usda", "holder.usda", AssetUse::Reference)]
    );
    doc.storage_mut().supply("b.usda", look("b").as_bytes());
    doc.read_requested_assets().unwrap();
    doc.load_requested_assets().unwrap();
    assert!(wanted(&doc).is_empty());
    assert_eq!(
        doc.view()
            .attribute("/Holder.name")
            .and_then(|a| a.value::<String>(Time::Default))
            .as_deref(),
        Some("b")
    );
}

/// A variant switch after an edit reaches an unfetched source: it is
/// wanted, not missing, and supplying it loads it without a reload, so the
/// edit and the dirty layer survive.
#[test]
fn a_variant_switch_after_an_edit_wants_its_source_and_keeps_the_edit() {
    let mut doc = open();
    doc.storage_mut().supply("a.usda", look("a").as_bytes());
    doc.load_requested_assets().unwrap();
    let root = doc.root_layer();
    let at = EditTarget::for_layer(root);
    let child = doc.store_mut().path("/Child");
    let edited = doc.store_mut().tokens().intern("edited");
    let model = doc.store_mut().path("/Model");
    let set = doc.store_mut().tokens().intern("look");
    let b = doc.store_mut().tokens().intern("b");

    let mut edit = Transaction::new();
    edit.create_property(
        at.property(PropertyPath::new(child, edited)),
        PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
            .with_default(Value::Int(7)),
    );
    doc.apply(&edit).unwrap();
    assert!(doc.is_dirty(root));

    let mut edit = Transaction::new();
    edit.set_variant_selection(at.prim(model), set, Some(b));
    let (applied, loaded) = doc.apply(&edit).unwrap();
    assert_eq!(applied.asset_requests.len(), 1);
    assert!(loaded.layers.is_empty() && loaded.asset_failures.is_empty());
    assert!(wanted(&doc).contains(&want("b.usda", "root.usda", AssetUse::Reference)));
    assert!(
        !missing(&doc).iter().any(|(asset, _)| asset == "b.usda"),
        "wanted, not missing"
    );
    assert_eq!(look_name(&doc), None);

    doc.storage_mut().supply("b.usda", look("b").as_bytes());
    let loaded = doc.load_requested_assets().unwrap();
    assert_eq!(loaded.layers.len(), 1);
    assert_eq!(look_name(&doc).as_deref(), Some("b"));
    assert_eq!(int(&doc, "/Child.edited"), Some(7));
    assert!(doc.is_dirty(root));
    assert!(!wanted(&doc).iter().any(|(id, _, _)| id == "b.usda"));
}

/// A source storage confirms absent after it was wanted becomes missing,
/// and a removed arc leaves the wanted list.
#[test]
fn confirmed_absent_sources_become_missing() {
    let mut doc = open();
    doc.storage_mut().absent.insert("sub.usda".into());
    let loaded = doc.load_requested_assets().unwrap();
    assert_eq!(loaded.asset_failures.len(), 1);
    assert!(loaded.asset_failures[0].is_missing());
    assert!(!wanted(&doc).iter().any(|(id, _, _)| id == "sub.usda"));
    assert!(missing(&doc).contains(&("sub.usda".into(), AssetUse::Sublayer)));

    let root = doc.root_layer();
    let child = doc.store_mut().path("/Child");
    let mut edit = Transaction::new();
    edit.remove_spec(EditTarget::for_layer(root).prim(child));
    doc.apply(&edit).unwrap();
    assert!(!wanted(&doc).iter().any(|(id, _, _)| id == "child.usda"));
}

/// A package is supplied as one buffer: its members resolve inside it, and
/// an external asset it references is wanted like any other.
#[test]
fn a_package_is_one_supplied_buffer() {
    let package = layerstack_usdz::write_usdz(&[
        PackageFile::new(
            "scene.usda",
            b"#usda 1.0\n(subLayers = [@./part.usda@])\ndef \"Asset\" (prepend references = @ext.usda@</Ext>) {}\n",
        ),
        PackageFile::new("part.usda", b"#usda 1.0\nover \"Asset\" {\n int part = 1\n}\n"),
    ])
    .unwrap();
    let mut host = Host::default();
    host.supply(
        "root.usda",
        b"#usda 1.0\ndef \"Pkg\" (prepend references = @pkg.usdz@</Asset>) {}\n",
    );
    let mut doc = StageDocument::open(host, "root.usda", StageOptions::default()).unwrap();
    assert_eq!(
        wanted(&doc),
        [want("pkg.usdz", "root.usda", AssetUse::Reference)]
    );

    doc.storage_mut().supply("pkg.usdz", &package);
    doc.load_requested_assets().unwrap();
    assert_eq!(int(&doc, "/Pkg.part"), Some(1));
    assert_eq!(
        wanted(&doc),
        [want("ext.usda", "pkg.usdz", AssetUse::Reference)]
    );

    doc.storage_mut()
        .supply("ext.usda", b"#usda 1.0\ndef \"Ext\" {\n int ext = 3\n}\n");
    doc.load_requested_assets().unwrap();
    assert_eq!(int(&doc, "/Pkg.ext"), Some(3));
    assert!(doc.asset_status().is_complete());
}

/// A root that is not supplied fails the open with its kind, and an
/// unavailable dependency never fails a strict open.
#[test]
fn an_unavailable_root_fails_the_open() {
    let error =
        StageDocument::open(Host::default(), "root.usda", StageOptions::default()).unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Unavailable);
    assert_eq!(error.identifier.as_deref(), Some("root.usda"));
}

/// Opening on several threads lists the same wanted sources as on one.
#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
fn opening_on_threads_wants_the_same_sources() {
    let mut host = Host::default();
    host.supply("root.usda", ROOT.as_bytes());
    host.supply("child.usda", CHILD.as_bytes());
    host.absent.insert("gone.usda".into());
    let options = StageOptions {
        threads: 4,
        ..StageOptions::default()
    };
    let doc = StageDocument::open(host, "root.usda", options).unwrap();
    assert_eq!(
        wanted(&doc),
        [
            want("a.usda", "root.usda", AssetUse::Reference),
            want("grand.usda", "child.usda", AssetUse::Reference),
            want("sub.usda", "root.usda", AssetUse::Sublayer),
        ]
    );
    assert_eq!(int(&doc, "/Child.child"), Some(1));
    assert_eq!(missing(&doc), [("gone.usda".into(), AssetUse::Reference)]);
}

/// A sublayer of a sublayer supplied in an earlier round composes when it
/// is supplied: the live stage reads the layer stack the supplied sublayer
/// extends, gathers its sublayer into the stage's layer stack and serves
/// its opinions, as composing anew does. OpenUSD opens each sublayer as it
/// computes the layer stack (`PcpLayerStack::_BuildLayerStack`).
#[test]
fn a_sublayer_of_a_supplied_sublayer_composes() {
    let mut host = Host::default();
    host.supply(
        "scene.usda",
        b"#usda 1.0\n(\n    subLayers = [@./sub.usda@]\n)\ndef \"Ball\" {\n    int size = 2\n}\n",
    );
    let mut doc = StageDocument::open(host, "scene.usda", StageOptions::default()).unwrap();
    doc.storage_mut().supply(
        "./sub.usda",
        b"#usda 1.0\n(\n    subLayers = [@./deep/detail.usda@]\n)\nover \"Ball\" {\n    int serial = 1\n}\n",
    );
    doc.load_requested_assets().unwrap();
    assert_eq!(int(&doc, "/Ball.serial"), Some(1));
    assert_eq!(
        wanted(&doc),
        [want("./deep/detail.usda", "./sub.usda", AssetUse::Sublayer)]
    );

    doc.storage_mut().supply(
        "./deep/detail.usda",
        b"#usda 1.0\nover \"Ball\" {\n    int tag = 3\n}\n",
    );
    let loaded = doc.load_requested_assets().unwrap();
    assert_eq!(loaded.layers.len(), 1);
    assert!(wanted(&doc).is_empty());
    assert!(doc.stage().stage().asset_requests().is_empty());
    let stack: Vec<&str> = doc
        .stage()
        .stage()
        .layer_stack()
        .iter()
        .map(|layer| doc.identifier(*layer).unwrap())
        .collect();
    assert_eq!(stack, ["scene.usda", "./sub.usda", "./deep/detail.usda"]);
    assert_eq!(int(&doc, "/Ball.size"), Some(2));
    assert_eq!(int(&doc, "/Ball.tag"), Some(3));
    assert!(doc.asset_status().is_complete());
}

/// A sublayer added by asset path to a layer of the stage's layer stack
/// that is not its root composes through `apply`: one supplied loads with
/// the edit, one not supplied yet is wanted from that layer and composes
/// when it is supplied, and the layer stack gathers each in its place.
#[test]
fn a_sublayer_added_to_a_sublayer_composes() {
    let mut host = Host::default();
    host.supply(
        "scene.usda",
        b"#usda 1.0\n(\n    subLayers = [@sub.usda@]\n)\ndef \"Ball\" {\n    int size = 2\n}\n",
    );
    host.supply(
        "sub.usda",
        b"#usda 1.0\nover \"Ball\" {\n    int serial = 1\n}\n",
    );
    host.supply(
        "more.usda",
        b"#usda 1.0\nover \"Ball\" {\n    int more = 4\n}\n",
    );
    let mut doc = StageDocument::open(host, "scene.usda", StageOptions::default()).unwrap();
    let stack = |doc: &StageDocument<Host>| -> Vec<String> {
        doc.stage()
            .stage()
            .layer_stack()
            .iter()
            .map(|layer| doc.identifier(*layer).unwrap().into())
            .collect()
    };
    assert_eq!(stack(&doc), ["scene.usda", "sub.usda"]);
    let sub = doc.stage().stage().layer_stack()[1];

    let mut edit = Transaction::new();
    edit.insert_sublayer(
        sub,
        0,
        SublayerEntry::unresolved("more.usda", LayerOffset::IDENTITY),
    );
    let (_, loaded) = doc.apply(&edit).unwrap();
    assert_eq!(loaded.layers.len(), 1);
    assert_eq!(stack(&doc), ["scene.usda", "sub.usda", "more.usda"]);
    assert_eq!(int(&doc, "/Ball.more"), Some(4));
    assert!(wanted(&doc).is_empty());
    // The stage reads the added layer: an edit made to it directly is
    // found when the document synchronizes.
    let more = doc.stage().stage().layer_stack()[2];
    let property = doc.store_mut().property_path("/Ball.more");
    doc.store_mut().layer_mut(more).unwrap().set_property(
        property,
        PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
            .with_default(Value::Int(6)),
    );
    doc.synchronize();
    assert_eq!(int(&doc, "/Ball.more"), Some(6));

    let mut edit = Transaction::new();
    edit.insert_sublayer(
        sub,
        1,
        SublayerEntry::unresolved("later.usda", LayerOffset::IDENTITY),
    );
    let (_, loaded) = doc.apply(&edit).unwrap();
    assert!(loaded.layers.is_empty());
    assert_eq!(
        wanted(&doc),
        [want("later.usda", "sub.usda", AssetUse::Sublayer)]
    );
    doc.storage_mut().supply(
        "later.usda",
        b"#usda 1.0\nover \"Ball\" {\n    int later = 5\n}\n",
    );
    doc.load_requested_assets().unwrap();
    assert_eq!(
        stack(&doc),
        ["scene.usda", "sub.usda", "more.usda", "later.usda"]
    );
    assert_eq!(int(&doc, "/Ball.later"), Some(5));
    assert_eq!(int(&doc, "/Ball.serial"), Some(1));
    assert!(doc.asset_status().is_complete());
}

const CLIPPED: &str = r#"#usda 1.0
def "P" (clips = {
 dictionary a = {
  asset[] assetPaths=[@c0.usda@,@c1.usda@]
  string primPath="/C"
  double2[] active=[(0,0),(10,1)]
  asset manifestAssetPath=@manifest.usda@
 }
}) { float x; }
"#;

/// The clip sources of `CLIPPED`, which a host fetches as the plan asks.
const CLIP_SOURCES: [(&str, &str); 3] = [
    ("manifest.usda", "#usda 1.0\ndef \"C\" {\n float x\n}\n"),
    (
        "c0.usda",
        "#usda 1.0\ndef \"C\" {\n float x.timeSamples={0:1,5:2}\n}\n",
    ),
    (
        "c1.usda",
        "#usda 1.0\ndef \"C\" {\n float x.timeSamples={10:10}\n}\n",
    ),
];

/// A clip source the host has not supplied is listed as unavailable, under
/// the identifier to supply, not as a failure; supplying what each
/// preparation lists completes the plan, manifest before clips, and reads
/// none of the clips the query does not demand.
#[test]
fn clip_preparation_lists_sources_not_supplied_yet() {
    let mut host = Host::default();
    host.supply("root.usda", CLIPPED.as_bytes());
    let mut doc = StageDocument::open(host, "root.usda", StageOptions::default()).unwrap();
    let property = doc.store_mut().property_path("/P.x");
    let mut rounds = Vec::new();
    loop {
        let prepared = doc.prepare_clip_query(property, Time::at(2.), 8, 16);
        assert!(prepared.failures.is_empty(), "{prepared:?}");
        if prepared.unavailable.is_empty() {
            assert!(prepared.plan.is_complete(), "{prepared:?}");
            break;
        }
        assert!(!prepared.plan.is_complete());
        let mut round: Vec<String> = Vec::new();
        for asset in prepared.unavailable {
            assert!(
                prepared
                    .plan
                    .pending()
                    .any(|demand| demand.request == asset.request),
                "an unavailable source is one the plan still demands"
            );
            let (_, bytes) = CLIP_SOURCES
                .iter()
                .find(|(name, _)| *name == asset.identifier)
                .unwrap();
            doc.storage_mut()
                .supply(&asset.identifier, bytes.as_bytes());
            round.push(asset.identifier);
        }
        rounds.push(round);
    }
    assert_eq!(rounds, [vec!["manifest.usda"], vec!["c0.usda"]]);
    let value = doc
        .view()
        .attribute(property)
        .and_then(|a| a.value::<f32>(Time::at(2.)));
    assert_eq!(value, Some(1.4));
    assert!(!doc.storage().reads.iter().any(|read| read == "c1.usda"));
}

/// A clip source the host confirms absent is missing, as from storage
/// that reads synchronously: the plan completes with a gap.
#[test]
fn a_clip_source_confirmed_absent_completes_the_plan() {
    let mut host = Host::default();
    host.supply("root.usda", CLIPPED.as_bytes());
    host.supply("manifest.usda", CLIP_SOURCES[0].1.as_bytes());
    host.absent.insert("c0.usda".into());
    let mut doc = StageDocument::open(host, "root.usda", StageOptions::default()).unwrap();
    let property = doc.store_mut().property_path("/P.x");
    let prepared = doc.prepare_clip_query(property, Time::at(2.), 8, 16);
    assert!(prepared.unavailable.is_empty() && prepared.failures.is_empty());
    assert!(prepared.plan.is_complete(), "{prepared:?}");
}
