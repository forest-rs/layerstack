// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A host that fetches sources asynchronously, such as a browser, supplies
//! them as they arrive: storage reports one it has not received as
//! [`IoErrorKind::Unavailable`], the document lists what the stage wants
//! ([`StageDocument::unavailable_assets`]), and
//! [`StageDocument::load_requested_assets`] reads what was supplied
//! without reading resident layers again.

use super::*;
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
    let doc =
        StageDocument::open_with(host, "root.usda", StageOptions::default(), loading).unwrap();
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

/// A variant switch after an edit reaches an unfetched source: it is
/// wanted, not missing, and supplying it loads it without a reload, so the
/// edit and the dirty layer survive.
#[test]
fn a_variant_switch_after_an_edit_wants_its_source_and_keeps_the_edit() {
    let mut doc = open();
    doc.storage_mut().supply("a.usda", look("a").as_bytes());
    doc.load_requested_assets().unwrap();
    let root = doc.stage().stage().root_layer().unwrap();
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

    let root = doc.stage().stage().root_layer().unwrap();
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
