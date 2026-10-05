// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use alloc::{format, vec};
use layerstack::{EditTarget, PrimSpec, PropertySpec, Transaction, Value};

#[derive(Debug, Default)]
struct Memory {
    files: BTreeMap<String, Vec<u8>>,
    writes: Vec<String>,
    fail_write: bool,
}
impl Memory {
    fn with(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(k, v)| ((*k).into(), v.as_bytes().to_vec()))
                .collect(),
            ..Default::default()
        }
    }
}
impl Storage for Memory {
    fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
        Ok(asset.into())
    }
    fn read(&mut self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.files
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, format!("missing {identifier}")))
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        if self.fail_write {
            return Err(IoError::new(IoErrorKind::Storage, "injected write failure"));
        }
        self.files.insert(identifier.into(), bytes.to_vec());
        self.writes.push(identifier.into());
        Ok(())
    }
}

fn numeric_usdc() -> Vec<u8> {
    let points = (0..2048)
        .map(|i| format!("({i}, 0, 0)"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("#usda 1.0\ndef Mesh \"Mesh\" {{ point3f[] points = [{points}] }}");
    let mut document = StageDocument::open(
        Memory::with(&[("mesh.usda", &source)]),
        "mesh.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = document.stage().stage().root_layer().unwrap();
    document.export_layer(root, "mesh.usdc").unwrap();
    document.storage_mut().files.remove("mesh.usdc").unwrap()
}

#[cfg(feature = "std")]
fn retained_options() -> LoadOptions {
    LoadOptions {
        usdc: UsdcReadOptions {
            arrays: UsdcArrayLoading::Retained,
            decode_budget: None,
        },
        ..Default::default()
    }
}

#[test]
fn explicit_decode_budgets_reject_structural_import_without_publishing() {
    let mut memory = Memory::default();
    memory.files.insert("mesh.usdc".into(), numeric_usdc());
    let error = StageDocument::open_with(
        memory,
        "mesh.usdc",
        StageOptions::default(),
        LoadOptions {
            usdc: UsdcReadOptions {
                decode_budget: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Rejected);
    assert!(error.message.contains("budget"), "{error}");
}

#[cfg(feature = "std")]
#[test]
fn retained_open_decodes_on_demand_and_reload_preserves_old_snapshots() {
    let mut memory = Memory::default();
    let bytes = numeric_usdc();
    memory.files.insert("mesh.usdc".into(), bytes.clone());
    let mut document = StageDocument::open_with(
        memory,
        "mesh.usdc",
        StageOptions::default(),
        retained_options(),
    )
    .unwrap();
    let root = document.stage().stage().root_layer().unwrap();
    let old = document.retained_values(root).unwrap().clone();
    assert_eq!(old.stats().decode_attempts, 0);
    assert_eq!(old.stats().input_bytes, bytes.len());
    let path = document.store_mut().property_path("/Mesh.points");
    let mut query = layerstack::AttributeQuery::new(path);
    let value = query
        .try_get(document.stage().stage(), layerstack::Time::Default)
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(value.array_ref().unwrap().len(), 2048);
    assert_eq!(old.stats().decode_attempts, 1);
    query
        .try_get(document.stage().stage(), layerstack::Time::Default)
        .unwrap();
    assert_eq!(old.stats().decode_attempts, 1);
    document.reload(ReloadPolicy::PreserveDirty).unwrap();
    assert_eq!(
        document
            .retained_values(root)
            .unwrap()
            .stats()
            .decode_attempts,
        0
    );
    assert_eq!(old.stats().decode_attempts, 1);
    assert_eq!(value.array_ref().unwrap().len(), 2048);
    document
        .storage_mut()
        .files
        .insert("mesh.usdc".into(), b"PXR-USDC corrupt".to_vec());
    assert!(document.reload(ReloadPolicy::PreserveDirty).is_err());
    assert_eq!(
        document
            .retained_values(root)
            .unwrap()
            .stats()
            .decode_attempts,
        0
    );
    assert!(
        layerstack::AttributeQuery::new(path)
            .try_get(document.stage().stage(), layerstack::Time::Default)
            .unwrap()
            .is_some()
    );
}

#[cfg(feature = "std")]
#[test]
fn retained_budget_errors_are_distinct_from_missing_geometry() {
    let bytes = numeric_usdc();
    let mut memory = Memory::default();
    memory.files.insert("mesh.usdc".into(), bytes.clone());
    let probe = StageDocument::open_with(
        memory,
        "mesh.usdc",
        StageOptions::default(),
        retained_options(),
    )
    .unwrap();
    let root = probe.stage().stage().root_layer().unwrap();
    let limit = layerstack_usdc::DecodeBudget::for_input(bytes.len()).remaining()
        - probe.retained_values(root).unwrap().stats().remaining_units;
    let mut memory = Memory::default();
    memory.files.insert("mesh.usdc".into(), bytes);
    let mut document = StageDocument::open_with(
        memory,
        "mesh.usdc",
        StageOptions::default(),
        LoadOptions {
            usdc: UsdcReadOptions {
                decode_budget: Some(limit),
                ..retained_options().usdc
            },
            ..Default::default()
        },
    )
    .unwrap();
    let path = document.store_mut().property_path("/Mesh.points");
    let expected = layerstack::ArrayReadError::BudgetExceeded { limit };
    assert_eq!(
        layerstack::AttributeQuery::new(path)
            .try_get(document.stage().stage(), layerstack::Time::Default),
        Err(expected.clone())
    );
    assert_eq!(
        layerstack::AttributeQuery::new(path)
            .try_get(document.stage().stage(), layerstack::Time::Default),
        Err(expected)
    );
    let stats = document.retained_values(root).unwrap().stats();
    assert_eq!(stats.failed_arrays, 1);
    assert_eq!(stats.decode_attempts, 1);
    let failure = document.export_layer(root, "failed.usdc").unwrap_err();
    assert_eq!(failure.kind, IoErrorKind::Decode);
    assert_eq!(
        failure.array_read_error,
        Some(layerstack::ArrayReadError::BudgetExceeded { limit })
    );
    assert!(!document.storage().files.contains_key("failed.usdc"));
    for member in ["root.usda", "root.usdc"] {
        let plan = layerstack_usdz::localize::LocalizationPlan {
            layers: vec![layerstack_usdz::localize::LocalizedLayer {
                source: root,
                member: member.into(),
                layer: document.store().layers[&root].clone(),
            }],
            assets: vec![],
            resolved_uses: 0,
        };
        let failure = document.export_usdz("failed.usdz", &plan).unwrap_err();
        assert_eq!(failure.kind, IoErrorKind::Decode);
        assert_eq!(
            failure.array_read_error,
            Some(layerstack::ArrayReadError::BudgetExceeded { limit })
        );
        assert!(!document.storage().files.contains_key("failed.usdz"));
    }
    assert!(document.storage().writes.is_empty());
}

#[cfg(feature = "std")]
#[test]
fn retained_policy_reaches_package_members_and_explicit_dependency_loads() {
    let bytes = numeric_usdc();
    let root = b"#usda 1.0\n( subLayers = [@./mesh.usdc@] )";
    let package = layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile {
            path: "root.usda",
            data: root,
        },
        layerstack_usdz::PackageFile {
            path: "mesh.usdc",
            data: &bytes,
        },
        layerstack_usdz::PackageFile {
            path: "unused.usdc",
            data: &bytes,
        },
    ])
    .unwrap();
    let mut memory = Memory::default();
    memory.files.insert("scene.usdz".into(), package);
    memory.files.insert("external.usdc".into(), bytes);
    let mut document = StageDocument::open_with(
        memory,
        "scene.usdz",
        StageOptions::default(),
        retained_options(),
    )
    .unwrap();
    let child = *document.catalog.ids.get("scene.usdz[mesh.usdc]").unwrap();
    assert_eq!(
        document
            .retained_values(child)
            .unwrap()
            .stats()
            .decode_attempts,
        0
    );
    assert!(!document.catalog.ids.contains_key("scene.usdz[unused.usdc]"));
    let path = document.store_mut().property_path("/Mesh.points");
    layerstack::AttributeQuery::new(path)
        .try_get(document.stage().stage(), layerstack::Time::Default)
        .unwrap();
    assert_eq!(
        document
            .retained_values(child)
            .unwrap()
            .stats()
            .decode_attempts,
        1
    );
    let (external, _) = document.load_asset("external.usdc", None).unwrap();
    assert_eq!(
        document
            .retained_values(external)
            .unwrap()
            .stats()
            .decode_attempts,
        0
    );
    document.reload(ReloadPolicy::PreserveDirty).unwrap();
    assert_eq!(
        document
            .retained_values(child)
            .unwrap()
            .stats()
            .decode_attempts,
        0
    );
}
#[cfg(feature = "std")]
#[test]
fn retained_binary_package_root_preserves_budget_and_reload_policy() {
    let bytes = numeric_usdc();
    let package = layerstack_usdz::write_usdz(&[layerstack_usdz::PackageFile {
        path: "root.usdc",
        data: &bytes,
    }])
    .unwrap();
    let mut memory = Memory::default();
    memory.files.insert("binary.usdz".into(), package);
    let mut document = StageDocument::open_with(
        memory,
        "binary.usdz",
        StageOptions::default(),
        retained_options(),
    )
    .unwrap();
    let root = document.stage().stage().root_layer().unwrap();
    let old = document.retained_values(root).unwrap().clone();
    assert_eq!(old.stats().decode_attempts, 0);
    let path = document.store_mut().property_path("/Mesh.points");
    assert_eq!(
        layerstack::AttributeQuery::new(path)
            .try_get(document.stage().stage(), layerstack::Time::Default)
            .unwrap()
            .unwrap()
            .value
            .array_ref()
            .unwrap()
            .len(),
        2048
    );
    assert_eq!(old.stats().decode_attempts, 1);
    document.reload(ReloadPolicy::PreserveDirty).unwrap();
    assert_eq!(
        document
            .retained_values(root)
            .unwrap()
            .stats()
            .decode_attempts,
        0
    );
    assert_eq!(old.stats().decode_attempts, 1);
    assert_eq!(document.load_options(), retained_options());
}

fn set_value(doc: &mut StageDocument<Memory>, layer: LayerId, value: i32) {
    let (store, stage) = doc.parts_mut();
    let property = store.property_path("/Root.value");
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(layer).property(property),
        Value::Int(value),
    );
    stage.apply(store, &edit).unwrap();
}
fn value(doc: &StageDocument<Memory>) -> Value {
    let prim = doc
        .store
        .paths
        .lookup(&layerstack::Path::root().join(&[doc.store.tokens.lookup("Root").unwrap()]))
        .unwrap();
    let property = layerstack::PropertyPath::new(prim, doc.store.tokens.lookup("value").unwrap());
    doc.stage
        .stage()
        .resolve_field_path(property)
        .unwrap()
        .value
}
#[test]
fn save_tracks_successful_generations_and_excludes_sessions() {
    let backend = Memory::with(&[(
        "root.usda",
        "#usda 1.0\ndef \"Root\" {\n int value = 1\n}\n",
    )]);
    let mut store = InMemoryStore::default();
    let session = LayerId(50);
    let mut layer = Layer::new(session);
    let prim = store.path("/Root");
    let property = store.tokens.intern("value");
    layer.insert_prim(
        prim,
        PrimSpec::over().with_property(
            property,
            PropertySpec::typed_attribute(layerstack::PropertyType::new(
                "int",
                false,
                Value::Int(0),
            ))
            .with_default(Value::Int(8)),
        ),
    );
    let pseudo = store.path("/");
    layer.insert_prim(
        pseudo,
        PrimSpec::default().with_children(vec![store.tokens.intern("Root")]),
    );
    store.insert_layer(layer);
    let mut doc = StageDocument::open_in(
        backend,
        store,
        "root.usda",
        StageOptions {
            session_layer: Some(session),
            ..Default::default()
        },
        ImportPolicy::Strict,
    )
    .unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    assert_ne!(root, session);
    assert_eq!(value(&doc), Value::Int(8));
    set_value(&mut doc, root, 2);
    assert!(doc.is_dirty(root));
    doc.storage_mut().fail_write = true;
    let failed = doc.save();
    assert_eq!(failed.failures.len(), 1);
    assert!(doc.is_dirty(root));
    doc.storage_mut().fail_write = false;
    assert_eq!(doc.save().saved, vec![root]);
    assert!(!doc.is_dirty(root));
    assert!(doc.is_dirty(session));
    assert_eq!(doc.save_session_layers().anonymous, vec![session]);
    doc.export_layer(session, "session.usda").unwrap();
    assert!(
        doc.is_dirty(session),
        "export does not change the binding or saved cursor"
    );
    assert!(doc.save().saved.is_empty());
}
#[test]
fn staged_reload_protects_edits_and_preserves_layer_identities() {
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\n(\n subLayers = [@base.usda@]\n)\ndef \"Root\" {}\n",
        ),
        (
            "base.usda",
            "#usda 1.0\ndef \"Root\" {\n int value = 1\n}\n",
        ),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    assert_eq!(value(&doc), Value::Int(1));
    let base = *doc.catalog.ids.get("base.usda").unwrap();
    set_value(&mut doc, base, 3);
    doc.storage_mut().files.insert(
        "base.usda".into(),
        b"#usda 1.0\ndef \"Root\" {\n int value = 9\n}\n".to_vec(),
    );
    assert_eq!(
        doc.reload(ReloadPolicy::PreserveDirty).unwrap_err().kind,
        IoErrorKind::DirtyReload
    );
    assert_eq!(value(&doc), Value::Int(3));
    let before = doc.store.layers.clone();
    doc.storage_mut()
        .files
        .insert("base.usda".into(), b"bad USD".to_vec());
    assert!(doc.reload(ReloadPolicy::DiscardDirty).is_err());
    assert_eq!(
        doc.store.layers, before,
        "failed staged reload publishes no layers"
    );
    assert!(doc.is_dirty(base));
    doc.storage_mut().files.insert(
        "base.usda".into(),
        b"#usda 1.0\ndef \"Root\" {\n int value = 9\n}\n".to_vec(),
    );
    let report = doc.reload(ReloadPolicy::DiscardDirty).unwrap();
    assert!(report.layers.contains(&base));
    assert_eq!(doc.catalog.ids["base.usda"], base);
    assert_eq!(value(&doc), Value::Int(9));
    assert!(!doc.is_dirty(base));
}
#[test]
fn binary_export_reopens_and_package_reload_keeps_member_ids() {
    let root = b"#usda 1.0\n(\n subLayers = [@base.usda@]\n)\ndef \"Root\" {}\n";
    let base = b"#usda 1.0\ndef \"Root\" {\n int value = 7\n}\n";
    let package = layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile::new("root.usda", root),
        layerstack_usdz::PackageFile::new("base.usda", base),
    ])
    .unwrap();
    let mut backend = Memory::default();
    backend.files.insert("scene.usdz".into(), package);
    let mut doc = StageDocument::open(backend, "scene.usdz", StageOptions::default()).unwrap();
    let member = doc.catalog.ids["scene.usdz[base.usda]"];
    assert_eq!(value(&doc), Value::Int(7));
    let report = doc.reload(ReloadPolicy::PreserveDirty).unwrap();
    assert!(report.layers.contains(&member));
    assert_eq!(doc.catalog.ids["scene.usdz[base.usda]"], member);
    assert_eq!(doc.store.layers.len(), 2);
    set_value(&mut doc, member, 11);
    let saved = doc.save();
    assert_eq!(saved.failures.len(), 1);
    assert_eq!(saved.failures[0].error.kind, IoErrorKind::ReadOnly);
    doc.export_layer(member, "base.usdc").unwrap();
    let backend = Memory {
        files: doc.storage.files.clone(),
        ..Default::default()
    };
    let binary = StageDocument::open(backend, "base.usdc", StageOptions::default()).unwrap();
    assert_eq!(value(&binary), Value::Int(11));
}

#[test]
fn resolution_does_not_dirty_clean_sources_or_clear_authored_dirtiness() {
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\ndef \"Root\" {\n int value = 1\n}\n",
        ),
        ("asset.usda", "#usda 1.0\ndef \"Asset\" {}\n"),
        ("other.usda", "#usda 1.0\ndef \"Other\" {}\n"),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    assert!(!doc.is_dirty(root));
    doc.load_asset("asset.usda", Some(root)).unwrap();
    assert!(!doc.is_dirty(root));
    set_value(&mut doc, root, 4);
    doc.load_asset("other.usda", Some(root)).unwrap();
    assert!(doc.is_dirty(root));
}
#[test]
fn expression_assets_open_with_the_primary_stack_and_retained_bindings() {
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\n(\n expressionVariables = {\n string ASSET = \"asset.usda\"\n }\n)\ndef \"Host\" (\n prepend references = @`${ASSET}`@</Asset>\n) {}\n",
        ),
        (
            "asset.usda",
            "#usda 1.0\ndef \"Asset\" {\n def \"Child\" {}\n}\n",
        ),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    let child = doc.store.path("/Host/Child");
    assert!(doc.stage.stage().has_prim(child));
    assert!(doc.stage.stage().composition_errors().is_empty());
    assert!(!doc.is_dirty(doc.stage.stage().root_layer().unwrap()));
}
#[test]
fn package_reload_reads_unreferenced_resident_members_and_allows_root_rename() {
    fn package(root_name: &str, root: &[u8], members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut files = vec![layerstack_usdz::PackageFile::new(root_name, root)];
        files.extend(
            members
                .iter()
                .map(|(name, data)| layerstack_usdz::PackageFile::new(name, data)),
        );
        layerstack_usdz::write_usdz(&files).unwrap()
    }
    let mut backend = Memory::default();
    backend.files.insert(
        "scene.usdz".into(),
        package(
            "root.usda",
            b"#usda 1.0\n(\n subLayers = [@member.usda@]\n)\ndef \"Root\" {}\n",
            &[("member.usda", b"#usda 1.0\ndef \"Old\" {}\n")],
        ),
    );
    let mut doc = StageDocument::open(backend, "scene.usdz", StageOptions::default()).unwrap();
    let member = doc.catalog.ids["scene.usdz[member.usda]"];
    doc.storage_mut().files.insert(
        "scene.usdz".into(),
        package(
            "root.usda",
            b"#usda 1.0\ndef \"Root\" {}\n",
            &[("member.usda", b"#usda 1.0\ndef \"New\" {}\n")],
        ),
    );
    assert!(
        doc.reload_layers(&[member], ReloadPolicy::DiscardDirty)
            .unwrap()
            .layers
            .contains(&member)
    );
    let new = doc.store.path("/New");
    let old = doc.store.path("/Old");
    assert!(doc.store.layers[&member].prims.contains_key(&new));
    assert!(!doc.store.layers[&member].prims.contains_key(&old));
    doc.storage_mut().files.insert(
        "scene.usdz".into(),
        package(
            "newroot.usda",
            b"#usda 1.0\n(\n subLayers = [@root.usda@]\n)\ndef \"Root\" {}\n",
            &[
                ("root.usda", b"#usda 1.0\ndef \"Another\" {}\n"),
                ("member.usda", b"#usda 1.0\ndef \"New\" {}\n"),
            ],
        ),
    );
    doc.reload(ReloadPolicy::DiscardDirty).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    assert_ne!(doc.catalog.ids["scene.usdz[root.usda]"], root);
    assert_eq!(doc.catalog.ids["scene.usdz[newroot.usda]"], root);
}
#[test]
fn top_level_transport_errors_keep_machine_readable_categories() {
    #[derive(Debug)]
    struct Failing;
    impl Storage for Failing {
        fn identify(&self, _: &str, _: Option<&str>) -> Result<String, IoError> {
            Ok("scene.usda".into())
        }
        fn read(&mut self, _: &str) -> Result<Vec<u8>, IoError> {
            Err(IoError::new(IoErrorKind::Storage, "transport unavailable"))
        }
        fn write(&mut self, _: &str, _: &[u8]) -> Result<(), IoError> {
            unreachable!()
        }
    }
    assert_eq!(
        StageDocument::open(Failing, "scene.usda", StageOptions::default())
            .unwrap_err()
            .kind,
        IoErrorKind::Storage
    );
}

#[test]
fn strict_nested_failures_cannot_disappear_inside_parser_recovery() {
    let mut backend = Memory::with(&[(
        "root.usda",
        "#usda 1.0\ndef \"Host\" (\n references = @broken.usda@</Asset>\n) {}\n",
    )]);
    backend.files.insert("broken.usda".into(), vec![255, 255]);
    assert_eq!(
        StageDocument::open(backend, "root.usda", StageOptions::default())
            .unwrap_err()
            .kind,
        IoErrorKind::Rejected
    );
}
#[test]
fn package_expressions_use_the_archive_before_outer_transport() {
    let root = b"#usda 1.0\n(\n expressionVariables = {\n string ASSET = \"member.usda\"\n }\n)\ndef \"Host\" (\n references = @`${ASSET}`@</Asset>\n) {}\n";
    let member = b"#usda 1.0\ndef \"Asset\" {\n def \"Child\" {}\n}\n";
    let mut backend = Memory::default();
    backend.files.insert(
        "scene.usdz".into(),
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new("root.usda", root),
            layerstack_usdz::PackageFile::new("member.usda", member),
        ])
        .unwrap(),
    );
    let mut doc = StageDocument::open(backend, "scene.usdz", StageOptions::default()).unwrap();
    let child = doc.store.path("/Host/Child");
    assert!(doc.stage.stage().has_prim(child));
    assert!(doc.stage.stage().composition_errors().is_empty());
}
#[test]
fn promoted_members_retain_distinct_identity_from_the_outer_package() {
    let root = b"#usda 1.0\n(\n subLayers = [@member.usda@]\n)\n";
    let member = b"#usda 1.0\ndef \"OldMember\" {}\n";
    let mut backend = Memory::default();
    backend.files.insert(
        "scene.usdz".into(),
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new("root.usda", root),
            layerstack_usdz::PackageFile::new("member.usda", member),
        ])
        .unwrap(),
    );
    let mut doc = StageDocument::open(backend, "scene.usdz", StageOptions::default()).unwrap();
    let member_id = doc.catalog.ids["scene.usdz[member.usda]"];
    let root_id = doc.stage.stage().root_layer().unwrap();
    doc.storage_mut().files.insert(
        "scene.usdz".into(),
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new("member.usda", b"#usda 1.0\ndef \"NewMember\" {}\n"),
            layerstack_usdz::PackageFile::new("root.usda", root),
        ])
        .unwrap(),
    );
    let report = doc
        .reload_layers(&[member_id], ReloadPolicy::DiscardDirty)
        .unwrap();
    assert!(report.layers.contains(&member_id));
    assert!(report.layers.contains(&root_id));
    assert_eq!(doc.catalog.ids["scene.usdz[member.usda]"], member_id);
    let new = doc.store.path("/NewMember");
    let old = doc.store.path("/OldMember");
    for id in [member_id, root_id] {
        assert!(doc.store.layers[&id].prims.contains_key(&new));
        assert!(!doc.store.layers[&id].prims.contains_key(&old));
    }
}
#[test]
fn newly_created_documents_save_and_reopen_without_seed_files() {
    let mut doc =
        StageDocument::create(Memory::default(), "new.usda", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let (store, stage) = doc.parts_mut();
    let prim = store.path("/Root");
    let mut edit = Transaction::new();
    edit.create_prim(
        EditTarget::for_layer(root).prim(prim),
        layerstack::Specifier::Def,
        None,
    );
    stage.apply(store, &edit).unwrap();
    assert!(doc.is_dirty(root));
    assert_eq!(doc.save().saved, vec![root]);
    let backend = Memory {
        files: doc.storage.files.clone(),
        ..Default::default()
    };
    let mut reopened = StageDocument::open(backend, "new.usda", StageOptions::default()).unwrap();
    let prim = reopened.store.path("/Root");
    assert!(reopened.stage.stage().has_prim(prim));
}

#[test]
fn nested_promoted_members_refresh_with_member_relative_anchors() {
    let mut backend = Memory::default();
    backend.files.insert(
        "scene.usdz".into(),
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new(
                "root.usda",
                b"#usda 1.0\n(subLayers=[@nested/member.usda@])\n",
            ),
            layerstack_usdz::PackageFile::new(
                "nested/member.usda",
                b"#usda 1.0\ndef \"OldMember\" {}\n",
            ),
        ])
        .unwrap(),
    );
    let mut doc = StageDocument::open(backend, "scene.usdz", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let member = doc.catalog.ids["scene.usdz[nested/member.usda]"];
    doc.storage_mut().files.insert(
        "scene.usdz".into(),
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new(
                "nested/member.usda",
                b"#usda 1.0\ndef \"NewMember\" {}\n",
            ),
            layerstack_usdz::PackageFile::new("root.usda", b"#usda 1.0\ndef \"PreviousRoot\" {}\n"),
        ])
        .unwrap(),
    );
    let report = doc
        .reload_layers(&[root], ReloadPolicy::DiscardDirty)
        .unwrap();
    assert!(report.layers.contains(&member));
    let new = doc.store.path("/NewMember");
    assert!(doc.store.layers[&member].prims.contains_key(&new));
}
#[test]
fn promoted_self_references_follow_the_explicit_member_identity() {
    let mut backend = Memory::default();
    backend.files.insert(
        "scene.usdz".into(),
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new(
                "root.usda",
                b"#usda 1.0\n(subLayers=[@member.usda@])\n",
            ),
            layerstack_usdz::PackageFile::new("member.usda", b"#usda 1.0\ndef \"OldMember\" {}\n"),
        ])
        .unwrap(),
    );
    let mut doc = StageDocument::open(backend, "scene.usdz", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let member = doc.catalog.ids["scene.usdz[member.usda]"];
    doc.storage_mut().files.insert("scene.usdz".into(), layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile::new("member.usda", b"#usda 1.0\ndef \"Asset\" {\n int x=1\n}\ndef \"Host\" (references=@member.usda@</Asset>) {}\n"),
        layerstack_usdz::PackageFile::new("root.usda", b"#usda 1.0\ndef \"PreviousRoot\" {}\n"),
    ]).unwrap());
    doc.reload_layers(&[root], ReloadPolicy::DiscardDirty)
        .unwrap();
    let asset = doc.store.property_path("/Asset.x");
    let host = doc.store.property_path("/Host.x");
    doc.store
        .layers
        .get_mut(&member)
        .unwrap()
        .set_property(asset, PropertySpec::attribute().with_default(Value::Int(9)));
    doc.synchronize();
    assert_eq!(
        layerstack::AttributeQuery::new(host)
            .get(doc.stage.stage(), layerstack::Time::Default)
            .unwrap()
            .value,
        Value::Int(9)
    );
    assert_eq!(
        layerstack::AttributeQuery::new(asset)
            .get(doc.stage.stage(), layerstack::Time::Default)
            .unwrap()
            .value,
        Value::Int(1)
    );
}

#[test]
fn package_relative_missing_dependencies_keep_recovery_evidence() {
    fn backend() -> Memory {
        let mut backend = Memory::default();
        backend.files.insert(
            "scene.usdz".into(),
            layerstack_usdz::write_usdz(&[layerstack_usdz::PackageFile::new(
                "root.usda",
                b"#usda 1.0\ndef \"Host\" (references=@./missing.usda@</A>) {}\n",
            )])
            .unwrap(),
        );
        backend
    }
    assert_eq!(
        StageDocument::open(backend(), "scene.usdz", StageOptions::default())
            .unwrap_err()
            .kind,
        IoErrorKind::Rejected
    );
    let doc = StageDocument::open_in(
        backend(),
        InMemoryStore::default(),
        "scene.usdz",
        StageOptions::default(),
        ImportPolicy::AllowRecovery,
    )
    .unwrap();
    assert!(doc.load_report().has_errors());
    assert!(doc.load_report().diagnostics.iter().any(|d| matches!(&d.diagnostic, ImportDiagnostic::AssetResolve {asset, error: AssetResolveError::NotFound} if &**asset == "./missing.usda")));
}

#[test]
fn retained_clip_queries_refresh_when_only_the_clip_source_is_reloaded() {
    use layerstack::{AttributeQuery, InterpolationType, Time};
    let backend = Memory::with(&[
        (
            "root.usda",
            r#"#usda 1.0
def "P" (clips = { dictionary default = {
 asset[] assetPaths = [@clip.usda@]
 string primPath = "/C"
 double2[] active = [(0,0)]
 asset manifestAssetPath = @manifest.usda@
 }}) { double x }
"#,
        ),
        (
            "clip.usda",
            "#usda 1.0\ndef \"C\" {\n double x.timeSamples={0:1,10:1}\n}\n",
        ),
        ("manifest.usda", "#usda 1.0\ndef \"C\" {\n double x\n}\n"),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    doc.load_asset("manifest.usda", Some(root)).unwrap();
    let (clip, _) = doc.load_asset("clip.usda", Some(root)).unwrap();
    let property = doc.store.property_path("/P.x");
    let mut query = AttributeQuery::new(property);
    let time = Time::At {
        code: 5.0,
        interpolation: InterpolationType::Linear,
    };
    assert_eq!(
        query.get(doc.stage.stage(), time).unwrap().value,
        Value::Double(1.0)
    );
    query.get(doc.stage.stage(), time);
    assert_eq!(query.work().cache_hits, 1);
    doc.storage_mut().files.insert(
        "clip.usda".into(),
        b"#usda 1.0\ndef \"C\" {\n double x.timeSamples={0:9,10:9}\n}\n".to_vec(),
    );
    doc.reload_layers(&[clip], ReloadPolicy::DiscardDirty)
        .unwrap();
    assert_eq!(
        query.get(doc.stage.stage(), time).unwrap().value,
        Value::Double(9.0)
    );
    assert_eq!(query.work().evaluations, 2);
}
