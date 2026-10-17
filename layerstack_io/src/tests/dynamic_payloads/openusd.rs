// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! OpenUSD's dynamic file format test, `testPcpDynamicFileFormatPlugin.py`
//! (`pxr/usd/pcp/testenv`), on its own fixtures
//! (`testPcpDynamicFileFormatPlugin.testenv`), with the test format ported
//! in [`TestDynamic`] and each case run with field and attribute inputs, as
//! OpenUSD runs it with `TEST_PCP_DYNAMIC_FILE_FORMAT_TOKENS_USE_ATTRIBUTE_INPUTS`
//! unset and set.
//!
//! The fixtures are read from the OpenUSD 26.08 checkout
//! `LAYERSTACK_OPENUSD_SOURCE` names; without one, each test prints that it
//! is skipped.
//!
//! The stage composes every prim of a root layer and loads every payload,
//! where OpenUSD's `PcpCache` computes only the prim indexes a case asks
//! for, with only the payloads it requests. Its "significant changes" are
//! checked here as the layers a prim index's dynamic payloads target
//! changing ([`layerstack::DynamicDependency::target_layers`]), among the
//! prims the case computes.

use super::*;
use std::path::PathBuf;

/// The fixture directory, canonical as the filesystem identifies it, when
/// `LAYERSTACK_OPENUSD_SOURCE` names an OpenUSD checkout.
fn fixtures() -> Option<String> {
    let Some(source) = std::env::var_os("LAYERSTACK_OPENUSD_SOURCE").map(PathBuf::from) else {
        std::eprintln!("skipped: set LAYERSTACK_OPENUSD_SOURCE to an OpenUSD 26.08 checkout");
        return None;
    };
    let dir = source.join("pxr/usd/pcp/testenv/testPcpDynamicFileFormatPlugin.testenv");
    let dir = std::fs::canonicalize(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    Some(dir.to_str().unwrap().into())
}

/// The fixtures, and an anonymous root layer beside them.
struct Fixtures {
    files: Filesystem,
    anonymous: Option<(String, Vec<u8>)>,
}

impl Storage for Fixtures {
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        if let Some((identifier, _)) = &self.anonymous
            && asset == "anonymous.usda"
        {
            return Ok(identifier.clone());
        }
        self.files.identify(asset, anchor)
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        match &self.anonymous {
            Some((anonymous, bytes)) if anonymous == identifier => Ok(bytes.clone()),
            _ => self.files.read(identifier),
        }
    }
    fn write(&mut self, _: &str, _: &[u8]) -> Result<(), IoError> {
        Err(IoError::new(
            IoErrorKind::ReadOnly,
            "the fixtures are read-only",
        ))
    }
}

fn fixture_open(dir: &str, root: &str, attributes: bool) -> StageDocument<Fixtures> {
    let files = Filesystem::new(dir, []).unwrap();
    open(
        Fixtures {
            files,
            anonymous: None,
        },
        root,
        attributes,
    )
}

/// `cone.testpcpdynamic` or another fixture with `args`, as [`generated`]
/// lists it.
fn layer(file: &str, args: &[(&str, &str)]) -> String {
    with_args(file, args)
}

fn assert_read(doc: &StageDocument<Fixtures>, dir: &str, file: &str, args: &[(&str, &str)]) {
    let layers = generated(doc, &format!("{dir}/"));
    let identifier = layer(file, args);
    assert!(
        layers.contains(&identifier),
        "{identifier} was not read: {layers:?}"
    );
}

/// The layer read as `identifier` (relative to `dir`).
fn layer_id(doc: &StageDocument<Fixtures>, dir: &str, identifier: &str) -> LayerId {
    let full = format!("{dir}/{identifier}");
    doc.store()
        .layers
        .keys()
        .copied()
        .find(|&id| doc.identifier(id) == Some(full.as_str()))
        .unwrap_or_else(|| panic!("{identifier} was not read"))
}

fn prim_id<B: Storage>(doc: &StageDocument<B>, path: &str) -> layerstack::PathId {
    let store = doc.store();
    store
        .paths
        .lookup(&layerstack::Path::parse_absolute(path, &store.tokens).unwrap())
        .unwrap_or_else(|| panic!("{path} is not composed"))
}

/// Whether `prim`'s prim stack holds `path` of `layer`.
fn in_prim_stack<B: Storage>(
    doc: &StageDocument<B>,
    prim: &str,
    layer: LayerId,
    path: &str,
) -> bool {
    let (prim, path) = (prim_id(doc, prim), prim_id(doc, path));
    doc.stage()
        .stage()
        .prim_stack(prim)
        .unwrap()
        .iter()
        .any(|(source, site)| *source == layer && site.prim_path() == path)
}

fn no_errors<B: Storage>(doc: &StageDocument<B>, prim: &str) {
    let prim = prim_id(doc, prim);
    let errors: Vec<_> = doc
        .stage()
        .stage()
        .composition_errors()
        .iter()
        .filter(|error| error.prim() == Some(prim))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

/// The relevant names of the cases, with the payload-specific ones of
/// `payload_ids` (`argDict` or the prefixed attributes).
fn names(attributes: bool, payload_ids: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = INPUTS.iter().map(|name| String::from(*name)).collect();
    if attributes {
        for id in payload_ids {
            names.extend(INPUTS.iter().map(|name| format!("{id}:{name}")));
        }
    } else if !payload_ids.is_empty() {
        names.push(ARG_DICT.into());
    }
    names
}

fn verify_names<B: Storage>(
    doc: &StageDocument<B>,
    paths: &[String],
    names: &[String],
    attributes: bool,
) {
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    verify(doc, paths, &names, attributes);
}

/// `test_FileFormat`: read without arguments, the sources are their USDA;
/// with depth 3 and count 2 they generate the baseline's prims.
#[test]
fn test_file_format() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let mut store = InMemoryStore::default();
        let schemas = registry(&mut store);
        let format = TestDynamic::new(attributes);
        let read = |store: &InMemoryStore, file: &str, args: &FormatArgs| {
            let path = format!("{dir}/{file}");
            let bytes = std::fs::read(&path).unwrap();
            let identifier = args.identifier(&path);
            let source = FormatSource {
                identifier: &identifier,
                layer: LayerId(1),
                bytes: Some(&bytes),
                args,
                tokens: &store.tokens,
                paths: &store.paths,
            };
            let layer = format.read(&source).unwrap();
            layerstack_usda::save::save_usda(&layer, &store.tokens, &store.paths).unwrap()
        };
        let baseline = |store: &InMemoryStore, file: &str, asset: &str| {
            let text = std::fs::read_to_string(format!("{dir}/baseline/{file}"))
                .unwrap()
                .replace("@placeholder.usda@", &format!("@{asset}@"));
            let read = layerstack_usda::read_usda_with_registry(
                &text,
                LayerId(2),
                &store.tokens,
                &store.paths,
                &mut NoAssets,
                &schemas,
            );
            assert!(!read.emitted.rejected);
            layerstack_usda::save::save_usda(&read.emitted.layer, &store.tokens, &store.paths)
                .unwrap()
        };
        let none = FormatArgs::new();
        assert_eq!(
            read(&store, "cone.testpcpdynamic", &none),
            baseline(&store, "cone_0.usda", "")
        );
        assert_eq!(
            read(&store, "sphere.testpcpdynamic", &none),
            baseline(&store, "sphere_0.usda", "")
        );
        let args = FormatArgs::new().with(DEPTH, "3").with(NUM, "2");
        let expected = if attributes {
            "proc_attr_3_2.usda"
        } else {
            "proc_metadata_3_2.usda"
        };
        for file in ["cone.testpcpdynamic", "sphere.testpcpdynamic"] {
            let asset = format!("{dir}/{file}");
            assert_eq!(
                read(&store, file, &args),
                baseline(&store, expected, &asset),
                "{file}"
            );
        }
    }
}

/// `test_BasicRead`.
#[test]
fn test_basic_read() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        verify_names(
            &doc,
            &dynamic_paths("/RootCone", 4, 3, 40, ""),
            &names(attributes, &[]),
            attributes,
        );
        let cone = "cone.testpcpdynamic";
        assert_read(
            &doc,
            &dir,
            cone,
            &[(DEPTH, "4"), (NUM, "3"), (RADIUS, "50")],
        );
        for (depth, radius) in [("3", "25"), ("2", "12.5"), ("1", "6.25")] {
            assert_read(
                &doc,
                &dir,
                cone,
                &[(DEPTH, depth), (HEIGHT, "3"), (NUM, "3"), (RADIUS, radius)],
            );
        }
    }
}

/// `test_PayloadsInVariants`.
#[test]
fn test_payloads_in_variants() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        verify_names(
            &doc,
            &dynamic_paths("/Variant", 5, 4, 341, ""),
            &names(attributes, &[]),
            attributes,
        );
    }
}

/// `test_NestedVariants`.
#[test]
fn test_nested_variants() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        verify_names(
            &doc,
            &dynamic_paths("/ReferenceAndNestedVariants", 2, 2, 3, ""),
            &names(attributes, &[]),
            attributes,
        );
        // The height on the referencing prim overrides the one the
        // selected variant authors.
        let cone = "cone.testpcpdynamic";
        assert_read(
            &doc,
            &dir,
            cone,
            &[(DEPTH, "2"), (HEIGHT, "120"), (NUM, "2"), (RADIUS, "50")],
        );
        assert_read(
            &doc,
            &dir,
            cone,
            &[(DEPTH, "1"), (HEIGHT, "120"), (NUM, "2"), (RADIUS, "25")],
        );
    }
}

/// `test_InheritsAndVariants`.
#[test]
fn test_inherits_and_variants() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        let cone = "cone.testpcpdynamic";
        for (root, radius, child_radius) in [
            ("/Inherits", "50", "25"),
            ("/VariantWithParams", "20", "10"),
            ("/VariantWithParams2", "20", "10"),
        ] {
            verify_names(
                &doc,
                &dynamic_paths(root, 2, 3, 4, ""),
                &names(attributes, &[]),
                attributes,
            );
            assert_read(
                &doc,
                &dir,
                cone,
                &[(DEPTH, "2"), (NUM, "3"), (RADIUS, radius)],
            );
            assert_read(
                &doc,
                &dir,
                cone,
                &[
                    (DEPTH, "1"),
                    (HEIGHT, "3"),
                    (NUM, "3"),
                    (RADIUS, child_radius),
                ],
            );
        }
    }
}

/// `test_WeakerOpinions`.
#[test]
fn test_weaker_opinions() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        let cone = "cone.testpcpdynamic";
        // Specializes are weaker than payloads, and still supply their
        // arguments.
        verify_names(
            &doc,
            &dynamic_paths("/Specializes", 3, 3, 13, ""),
            &names(attributes, &[]),
            attributes,
        );
        assert_read(
            &doc,
            &dir,
            cone,
            &[(DEPTH, "3"), (NUM, "3"), (RADIUS, "50")],
        );
        assert_read(
            &doc,
            &dir,
            cone,
            &[(DEPTH, "2"), (HEIGHT, "3"), (NUM, "3"), (RADIUS, "25")],
        );
        assert_read(
            &doc,
            &dir,
            cone,
            &[(DEPTH, "1"), (HEIGHT, "3"), (NUM, "3"), (RADIUS, "12.5")],
        );
        // So do weaker siblings of the payload's parent.
        verify_names(
            &doc,
            &dynamic_paths("/WeakerParentSibling", 2, 3, 4, ""),
            &names(attributes, &[]),
            attributes,
        );
        assert_read(
            &doc,
            &dir,
            cone,
            &[(DEPTH, "2"), (NUM, "3"), (RADIUS, "50")],
        );
        assert_read(
            &doc,
            &dir,
            cone,
            &[(DEPTH, "1"), (HEIGHT, "3"), (NUM, "3"), (RADIUS, "25")],
        );
    }
}

/// `test_SiblingPayloads`.
#[test]
fn test_sibling_payloads() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        verify_names(
            &doc,
            &dynamic_paths("/SiblingPayloads", 2, 3, 4, ""),
            &names(attributes, &[]),
            attributes,
        );
        let sphere = "sphere.testpcpdynamic";
        assert_read(&doc, &dir, sphere, &[(DEPTH, "2"), (NUM, "3")]);
        assert_read(
            &doc,
            &dir,
            sphere,
            &[(DEPTH, "1"), (HEIGHT, "3"), (NUM, "3"), (RADIUS, "1.5")],
        );
    }
}

const CONE_4_3_50: [(&str, &str); 3] = [(DEPTH, "4"), (NUM, "3"), (RADIUS, "50")];

/// `test_AncestralPayloads`: the parameters on the referencing prim do
/// not map to the referenced prim's ancestral payload.
#[test]
fn test_ancestral_payloads() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        no_errors(&doc, "/SubrootReference");
        let cone = layer_id(&doc, &dir, &layer("cone.testpcpdynamic", &CONE_4_3_50));
        assert!(in_prim_stack(
            &doc,
            "/SubrootReference",
            cone,
            "/Root/Xform__3_2"
        ));
    }
}

/// `test_AncestralPayloads2`: the same through a reference from another
/// layer.
#[test]
fn test_ancestral_payloads2() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let anonymous = format!("{dir}/anonymous.usda");
        let text = "#usda 1.0\n\ndef \"Root\" (\n    TestPcp_depth = 1\n    TestPcp_num = 1\n    references = @./root.usda@</SubrootReference>\n)\n{\n    int TestPcp_depth = 1\n    int TestPcp_num = 1\n}\n";
        let storage = Fixtures {
            files: Filesystem::new(&dir, []).unwrap(),
            anonymous: Some((anonymous, text.as_bytes().to_vec())),
        };
        let doc = open(storage, "anonymous.usda", attributes);
        no_errors(&doc, "/Root");
        let full = format!("{dir}/{}", layer("cone.testpcpdynamic", &CONE_4_3_50));
        let cone = doc
            .store()
            .layers
            .keys()
            .copied()
            .find(|&id| doc.identifier(id) == Some(full.as_str()))
            .unwrap();
        assert!(in_prim_stack(&doc, "/Root", cone, "/Root/Xform__3_2"));
    }
}

/// `test_AncestralPayloads3`: ancestral payloads evaluated last, beneath
/// relocates and variants.
#[test]
fn test_ancestral_payloads3() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        no_errors(&doc, "/World/Sets/MySet/Group1/Subgroup/Relocated");
        let args = [(DEPTH, "4"), (NUM, "4"), (RADIUS, "50")];
        let cone = layer_id(&doc, &dir, &layer("cone.testpcpdynamic", &args));
        let xform = prim_id(&doc, "/Root/Xform__3_3");
        assert!(doc.store().layers[&cone].prims().contains_key(&xform));
    }
}

/// `test_AncestralPayloadsAndVariants`.
#[test]
fn test_ancestral_payloads_and_variants() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        no_errors(&doc, "/SubrootReferenceAndVariant");
        let args = [(DEPTH, "5"), (NUM, "4"), (RADIUS, "50")];
        let cone = layer_id(&doc, &dir, &layer("cone.testpcpdynamic", &args));
        assert!(in_prim_stack(
            &doc,
            "/SubrootReferenceAndVariant",
            cone,
            "/Root/Xform__4_3"
        ));
        // The branch's payload reads those arguments wherever it is
        // composed, the recursive index of the reference included, which
        // reads `/Variant`'s depth across the reference.
        let id = prim_id(&doc, "/SubrootReferenceAndVariant");
        let mut targets: Vec<String> = doc
            .stage()
            .stage()
            .dynamic_dependency(id)
            .unwrap()
            .target_layers()
            .map(|target| {
                let identifier = doc.identifier(target).unwrap();
                String::from(identifier.strip_prefix(&format!("{dir}/")).unwrap())
            })
            .collect();
        targets.sort_unstable();
        assert_eq!(
            targets,
            [
                layer(
                    "cone.testpcpdynamic",
                    &[(DEPTH, "1"), (HEIGHT, "3"), (NUM, "1"), (RADIUS, "25")]
                ),
                layer("cone.testpcpdynamic", &args),
            ]
        );
    }
}

/// `test_AncestralPayloadsMapFunction`: the ancestral payload beneath the
/// implied inherit is the second node of the index, at the payload's
/// target. (OpenUSD checks its map function too, which the composed graph
/// does not expose.)
#[test]
fn test_ancestral_payloads_map_function() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        no_errors(&doc, "/AncestralMapFunction/Model/Inherit");
        let prim = prim_id(&doc, "/AncestralMapFunction/Model/Inherit");
        let graph = doc.stage().stage().explain_prim_graph(prim).unwrap();
        let root = graph.node(graph.root().unwrap()).unwrap();
        let implied = graph.node(root.children()[0]).unwrap();
        let payload = graph.node(implied.children()[0]).unwrap();
        assert_eq!(payload.arc_kind(), layerstack::ArcKind::Payloads);
        let store = doc.store();
        assert_eq!(
            store
                .paths
                .display(payload.site().prim_path(), &store.tokens),
            "/Model/Class"
        );
    }
}

/// `test_PayloadInVariant`.
#[test]
fn test_payload_in_variant() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let doc = fixture_open(&dir, "root.usda", attributes);
        verify_names(
            &doc,
            &dynamic_paths("/PayloadInVariant", 4, 4, 85, ""),
            &names(attributes, &[]),
            attributes,
        );
        assert_read(
            &doc,
            &dir,
            "cone.testpcpdynamic",
            &[(DEPTH, "4"), (NUM, "4"), (RADIUS, "50")],
        );
    }
}

/// The layers each of `prims`' dynamic payloads target.
fn targets<B: Storage>(doc: &StageDocument<B>, prims: &[&str]) -> Vec<Vec<LayerId>> {
    prims
        .iter()
        .map(|prim| {
            let id = prim_id(doc, prim);
            doc.stage()
                .stage()
                .dynamic_dependency(id)
                .map(|dependency| dependency.target_layers().collect())
                .unwrap_or_default()
        })
        .collect()
}

/// Authors `value` as [`author`] does and returns which of `computed`
/// changed the layers their payloads target, without those beneath
/// another: OpenUSD's significant changes among the prim indexes the case
/// computed.
fn significant<B: Storage>(
    doc: &mut StageDocument<B>,
    layer: &str,
    prim: &str,
    name: &str,
    value: Value,
    attributes: bool,
    computed: &[&str],
) -> Vec<String> {
    let before = targets(doc, computed);
    author(doc, layer, prim, name, value, attributes);
    let after = targets(doc, computed);
    let changed: Vec<&str> = computed
        .iter()
        .zip(before.iter().zip(&after))
        .filter(|(_, (before, after))| before != after)
        .map(|(prim, _)| *prim)
        .collect();
    // A change is reported at the root of the namespace it changes.
    changed
        .iter()
        .filter(|prim| {
            !changed
                .iter()
                .any(|root| prim.len() > root.len() && prim.starts_with(&format!("{root}/")))
        })
        .map(|prim| String::from(*prim))
        .collect()
}

/// `test_Changes`.
#[test]
fn test_changes() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let mut doc = fixture_open(&dir, "root.usda", attributes);
        let names = names(attributes, &[]);
        let payloads = dynamic_paths("/RootSphere", 4, 3, 40, "");
        verify_names(&doc, &payloads, &names, attributes);
        let computed: Vec<&str> = [
            "/RootSphere",
            "/RootSphere/Xform__3_0",
            "/RootSphere/Xform__3_1",
            "/RootSphere/Xform__3_2",
        ]
        .into();
        let none: [&str; 0] = [];
        let change = |doc: &mut StageDocument<Fixtures>,
                      layer: &str,
                      prim: &str,
                      name: &str,
                      value: Value| {
            significant(doc, layer, prim, name, value, attributes, &computed)
        };
        assert_eq!(
            change(
                &mut doc,
                "root.usda",
                "/RootSphere",
                "documentation",
                Value::String("update documentation".into())
            ),
            none
        );
        verify_names(&doc, &payloads, &names, attributes);
        assert_eq!(
            change(
                &mut doc,
                "root.usda",
                "/RootSphere",
                HEIGHT,
                Value::Double(2.0)
            ),
            ["/RootSphere"]
        );
        verify_names(&doc, &payloads, &names, attributes);
        assert_eq!(
            change(&mut doc, "params.usda", "/Params", NUM, Value::Int(2)),
            ["/RootSphere"]
        );
        let payloads = dynamic_paths("/RootSphere", 4, 2, 15, "");
        verify_names(&doc, &payloads, &names, attributes);
        assert_eq!(
            change(&mut doc, "params.usda", "/Params", DEPTH, Value::Int(0)),
            ["/RootSphere"]
        );
        verify(&doc, &["/RootSphere".into()], &[DEPTH], attributes);
        // A depth of -1 reads as none too: no change.
        assert_eq!(
            change(&mut doc, "params.usda", "/Params", DEPTH, Value::Int(-1)),
            none
        );
        assert_eq!(
            change(&mut doc, "params.usda", "/Params", DEPTH, Value::Int(4)),
            ["/RootSphere"]
        );
        verify_names(&doc, &payloads, &names, attributes);
        // A generated prim's over changes it and its subtree only.
        let computed = [
            "/RootSphere",
            "/RootSphere/Xform__3_0",
            "/RootSphere/Xform__3_1",
        ];
        assert_eq!(
            significant(
                &mut doc,
                "root.usda",
                "/RootSphere/Xform__3_0",
                DEPTH,
                Value::Int(4),
                attributes,
                &computed,
            ),
            ["/RootSphere/Xform__3_0"]
        );
        verify_names(
            &doc,
            &dynamic_paths("/RootSphere/Xform__3_0", 4, 2, 15, ""),
            &names,
            attributes,
        );
        verify_names(
            &doc,
            &dynamic_paths("/RootSphere/Xform__3_1", 3, 2, 7, ""),
            &names,
            attributes,
        );
        // `geom` composes no dynamic arguments.
        assert_eq!(
            significant(
                &mut doc,
                "root.usda",
                "/RootSphere/geom",
                DEPTH,
                Value::Int(4),
                attributes,
                &["/RootSphere", "/RootSphere/geom"],
            ),
            none
        );
    }
}

fn arg_dict(payload_id: &str, num: i32) -> Value {
    Value::Dictionary(vec![(
        Arc::from(payload_id),
        Value::Dictionary(vec![(Arc::from(NUM), Value::Int(num))]),
    )])
}

/// `test_ChangesMultiPayload`.
#[test]
fn test_changes_multi_payload() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let mut doc = fixture_open(&dir, "root.usda", attributes);
        let payloads1 = dynamic_paths("/RootMulti", 3, 4, 21, "Pl1");
        let payloads2 = dynamic_paths("/RootMulti", 4, 2, 15, "Pl2");
        let root = names(attributes, &["Pl1", "Pl2"]);
        verify_names(&doc, &payloads1[..1], &root, attributes);
        verify_names(
            &doc,
            &payloads1[1..],
            &names(attributes, &["Pl1"]),
            attributes,
        );
        verify_names(
            &doc,
            &payloads2[1..],
            &names(attributes, &["Pl2"]),
            attributes,
        );
        let computed = ["/RootMulti"];
        let none: [&str; 0] = [];
        // An argument for a payload id no payload has changes nothing; one
        // for `Pl1` changes the prim's payloads.
        let (pl3, pl1) = if attributes {
            (
                (format!("Pl3:{NUM}"), Value::Int(1)),
                (format!("Pl1:{NUM}"), Value::Int(1)),
            )
        } else {
            (
                (ARG_DICT.into(), arg_dict("Pl3", 1)),
                (ARG_DICT.into(), arg_dict("Pl1", 1)),
            )
        };
        assert_eq!(
            significant(
                &mut doc,
                "root.usda",
                "/RootMulti",
                &pl3.0,
                pl3.1,
                attributes,
                &computed
            ),
            none
        );
        assert_eq!(
            significant(
                &mut doc,
                "root.usda",
                "/RootMulti",
                &pl1.0,
                pl1.1,
                attributes,
                &computed
            ),
            ["/RootMulti"]
        );
        let payloads1 = dynamic_paths("/RootMulti", 3, 1, 3, "Pl1");
        verify_names(&doc, &payloads1[..1], &root, attributes);
        verify_names(
            &doc,
            &payloads1[1..],
            &names(attributes, &["Pl1"]),
            attributes,
        );
        verify_names(
            &doc,
            &payloads2[1..],
            &names(attributes, &["Pl2"]),
            attributes,
        );
    }
}

/// `test_SubrootRefChange`.
#[test]
fn test_subroot_ref_change() {
    let Some(dir) = fixtures() else { return };
    for attributes in [false, true] {
        let mut doc = fixture_open(&dir, "subrootref.usda", attributes);
        let payloads = dynamic_paths("/Root", 2, 4, 5, "Pl1");
        let child = names(attributes, &["Pl1"]);
        let root = names(attributes, &["Pl1", "Pl2"]);
        let root = if attributes { root } else { child.clone() };
        verify_names(&doc, &payloads[..1], &root, attributes);
        verify_names(&doc, &payloads[1..], &child, attributes);
        assert_eq!(
            significant(
                &mut doc,
                "subrootref.usda",
                "/Root",
                HEIGHT,
                Value::Double(2.0),
                attributes,
                &["/Root"],
            ),
            ["/Root"]
        );
        verify_names(&doc, &payloads[..1], &root, attributes);
        verify_names(&doc, &payloads[1..], &child, attributes);
        // An argument of `/RootMulti` in the referenced layer reaches the
        // prim referencing its child.
        let (name, value) = if attributes {
            (format!("Pl1:{NUM}"), Value::Int(5))
        } else {
            (ARG_DICT.into(), arg_dict("Pl1", 5))
        };
        assert_eq!(
            significant(
                &mut doc,
                "/root.usda",
                "/RootMulti",
                &name,
                value,
                attributes,
                &["/Root"],
            ),
            ["/Root"]
        );
        let payloads = dynamic_paths("/Root", 2, 5, 6, "Pl1");
        verify_names(&doc, &payloads[..1], &root, attributes);
        verify_names(&doc, &payloads[1..], &child, attributes);
        // So does one reaching a prim that references a generated, not
        // dynamic, child.
        no_errors(&doc, "/SubrootGeomRef");
        let (name, value) = if attributes {
            (format!("Pl1:{NUM}"), Value::Int(3))
        } else {
            (ARG_DICT.into(), arg_dict("Pl1", 3))
        };
        assert_eq!(
            significant(
                &mut doc,
                "/root.usda",
                "/RootMulti",
                &name,
                value,
                attributes,
                &["/Root", "/SubrootGeomRef"],
            ),
            ["/Root", "/SubrootGeomRef"]
        );
    }
}
