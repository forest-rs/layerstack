// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use alloc::{format, vec};
use layerstack::{EditTarget, PrimSpec, PropertySpec, Transaction, Value};

#[derive(Debug, Default)]
struct Memory {
    files: BTreeMap<String, Vec<u8>>,
    writes: Vec<String>,
    reads: Vec<String>,
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
        self.reads.push(identifier.into());
        self.files
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, format!("missing {identifier}")))
    }
    fn read_bounded(&mut self, identifier: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
        let bytes = self
            .files
            .get(identifier)
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, identifier))?;
        if bytes.len() as u64 > max_bytes {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                "resource exceeds encoded byte budget",
            ));
        }
        self.read(identifier)
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

#[test]
fn rejecting_a_prepared_reload_preserves_sources_bindings_and_freshness() {
    let mut doc = StageDocument::open(
        Memory::with(&[
            ("root.usda", "#usda 1.0\ndef \"Root\" { int value = 1 }"),
            ("new.usda", "#usda 1.0\ndef \"Asset\" { int value = 9 }"),
        ]),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let generation = doc.store.layers[&root].generation();
    let ids = doc.catalog.ids.clone();
    let saved = doc.catalog.saved.clone();
    let bindings = doc.store.asset_layers.clone();
    let identity = doc.store.identity();
    let mut cursor = doc.stage.change_cursor();
    doc.storage.files.insert(
        "root.usda".into(),
        b"#usda 1.0\ndef \"Root\" (references=@new.usda@</Asset>) {}".to_vec(),
    );
    {
        let candidate = doc.prepare_reload(ReloadPolicy::PreserveDirty).unwrap();
        let new = candidate
            .load_report()
            .layers
            .iter()
            .copied()
            .find(|id| candidate.identifier(*id) == Some("new.usda"))
            .unwrap();
        assert!(candidate.store().layers.contains_key(&new));
        let property = layerstack::PropertyPath::new(
            candidate
                .store()
                .paths
                .lookup(
                    &layerstack::Path::root().join(&[candidate
                        .store()
                        .tokens
                        .lookup("Root")
                        .unwrap()]),
                )
                .unwrap(),
            candidate.store().tokens.lookup("value").unwrap(),
        );
        assert_eq!(
            candidate
                .stage()
                .stage()
                .resolve_field_path(property)
                .unwrap()
                .value,
            Value::Int(9)
        );
    }
    assert_eq!(doc.store.layers[&root].generation(), generation);
    assert_eq!(doc.catalog.ids, ids);
    assert_eq!(doc.catalog.saved, saved);
    assert_eq!(doc.store.asset_layers, bindings);
    assert_eq!(doc.store.identity(), identity);
    assert_eq!(value(&doc), Value::Int(1));
    doc.synchronize();
    assert_eq!(doc.stage.changes_since(&mut cursor).unwrap().count(), 0);
    assert!(!doc.is_dirty(root));
    doc.prepare_reload(ReloadPolicy::PreserveDirty)
        .unwrap()
        .commit();
    assert_eq!(value(&doc), Value::Int(9));
}

#[test]
fn prepared_commit_reuses_sources_and_preserves_stage_observers_and_dependencies() {
    use core::sync::atomic::{AtomicUsize, Ordering};
    let mut doc = StageDocument::open(
        Memory::with(&[
            (
                "root.usda",
                "#usda 1.0\n(subLayers=[@base.usda@])\ndef \"Removed\" {}",
            ),
            ("base.usda", "#usda 1.0\ndef \"Root\" { int value = 1 }"),
        ]),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let base = doc.catalog.ids["base.usda"];
    let property = doc.store.property_path("/Root.value");
    let mut cached = layerstack::AttributeQuery::new(property);
    assert_eq!(
        cached
            .try_get(doc.stage.stage(), layerstack::Time::Default)
            .unwrap()
            .unwrap()
            .value,
        Value::Int(1)
    );
    assert!(cached.is_current(doc.stage.stage(), layerstack::Time::Default));
    let mut other = LiveStage::compose(
        &mut doc.store,
        doc.stage.stage().root_layer().unwrap(),
        StageOptions::default(),
    );
    let mut cursor = doc.stage.change_cursor();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let subscription = doc.stage.subscribe_changes(move |notice| {
        if counter.fetch_add(1, Ordering::Relaxed) == 0 {
            assert!(
                !notice.changes.resynced.is_empty(),
                "publication resynchronizes composed observers"
            );
        }
    });
    let budget = doc.stage.change_history_budget();
    doc.storage.files.insert(
        "root.usda".into(),
        b"#usda 1.0\n(subLayers=[@base.usda@])\ndef \"Created\" {}".to_vec(),
    );
    doc.storage.files.insert(
        "base.usda".into(),
        b"#usda 1.0\ndef \"Root\" { int value = 7 }".to_vec(),
    );
    let report = doc
        .prepare_reload(ReloadPolicy::PreserveDirty)
        .unwrap()
        .commit();
    assert!(report.layers.contains(&base));
    assert!(!cached.is_current(doc.stage.stage(), layerstack::Time::Default));
    assert_eq!(
        cached
            .try_get(doc.stage.stage(), layerstack::Time::Default)
            .unwrap()
            .unwrap()
            .value,
        Value::Int(7)
    );
    assert_eq!(value(&doc), Value::Int(7));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let notices: Vec<_> = doc.stage.changes_since(&mut cursor).unwrap().collect();
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].created,
        [doc.store
            .paths
            .lookup(&layerstack::Path::root().join(&[doc.store.tokens.lookup("Created").unwrap()]))
            .unwrap()]
    );
    assert_eq!(
        notices[0].removed,
        [doc.store
            .paths
            .lookup(&layerstack::Path::root().join(&[doc.store.tokens.lookup("Removed").unwrap()]))
            .unwrap()]
    );
    assert_eq!(doc.stage.change_history_budget(), budget);
    assert_eq!(
        doc.storage.reads.len(),
        4,
        "commit never rereads source bytes"
    );
    other.synchronize(&mut doc.store);
    assert!(
        other.stage().has_prim(
            doc.store
                .paths
                .lookup(
                    &layerstack::Path::root().join(&[doc.store.tokens.lookup("Created").unwrap()])
                )
                .unwrap()
        )
    );
    set_value(&mut doc, base, 11);
    assert_eq!(value(&doc), Value::Int(11));
    assert!(doc.stage.unsubscribe_changes(&subscription));
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

#[test]
fn asset_bytes_use_package_member_provenance_without_importing_assets() {
    let mut backend = Memory::default();
    backend.files.insert(
        "scene.usdz".into(),
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new(
                "root.usda",
                b"#usda 1.0\n(subLayers=[@layers/member.usda@])\n",
            ),
            layerstack_usdz::PackageFile::new(
                "layers/member.usda",
                b"#usda 1.0\ndef \"Mesh\" {}\n",
            ),
            layerstack_usdz::PackageFile::new("layers/textures/color.png", &[0, 1, 2, 255]),
            layerstack_usdz::PackageFile::new("textures/color.png", b"root texture"),
            layerstack_usdz::PackageFile::new("root_only.exr", b"environment"),
        ])
        .unwrap(),
    );
    backend
        .files
        .insert("missing.png".into(), b"search fallback".to_vec());
    backend
        .files
        .insert("./missing.png".into(), b"must not fall through".to_vec());
    let mut doc = StageDocument::open(backend, "scene.usdz", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let member = doc.catalog.ids["scene.usdz[layers/member.usda]"];
    let generation = doc.store.layers[&member].generation();
    let layers = doc.store.layers.len();
    let reads = doc.storage.reads.len();
    let bytes = doc
        .read_asset_bytes("textures/color.png", Some(member))
        .unwrap();
    assert_eq!(bytes.bytes.as_ref(), &[0, 1, 2, 255]);
    assert_eq!(bytes.identifier, "scene.usdz[layers/textures/color.png]");
    assert_eq!(bytes.anchor, Some(member));
    assert_eq!(
        bytes.source,
        AssetByteSource::PackageMember {
            package: "scene.usdz".into(),
            member: "layers/textures/color.png".into()
        }
    );
    assert_eq!(
        doc.read_asset_bytes("textures\\color.png", Some(member))
            .unwrap()
            .identifier,
        bytes.identifier
    );
    assert_eq!(
        doc.read_asset_bytes("../textures/color.png", Some(member))
            .unwrap()
            .bytes
            .as_ref(),
        b"root texture"
    );
    assert_eq!(
        doc.read_asset_bytes("root_only.exr", Some(member))
            .unwrap()
            .bytes
            .as_ref(),
        b"environment"
    );
    assert_eq!(
        doc.read_asset_bytes("textures/color.png", Some(root))
            .unwrap()
            .bytes
            .as_ref(),
        b"root texture"
    );
    assert_eq!(
        doc.read_asset_bytes(&bytes.identifier, None)
            .unwrap()
            .bytes
            .as_ref(),
        bytes.bytes.as_ref()
    );
    assert_eq!(
        doc.storage.reads.len(),
        reads,
        "resident packages provide their snapshot without transport reads"
    );
    assert_eq!(
        doc.read_asset_bytes("./missing.png", Some(member))
            .unwrap_err()
            .kind,
        IoErrorKind::NotFound
    );
    assert_eq!(
        doc.read_asset_bytes("../../textures/color.png", Some(member))
            .unwrap_err()
            .kind,
        IoErrorKind::NotFound
    );
    let external = doc.read_asset_bytes("missing.png", Some(member)).unwrap();
    assert_eq!(external.bytes.as_ref(), b"search fallback");
    assert_eq!(external.source, AssetByteSource::Storage);
    assert_eq!(
        doc.read_asset_bytes("textures/color.png", Some(LayerId(9999)))
            .unwrap_err()
            .kind,
        IoErrorKind::MissingLayer
    );
    assert_eq!(
        doc.read_asset_bytes("scene.usdz[../escape.png]", None)
            .unwrap_err()
            .kind,
        IoErrorKind::Rejected
    );
    assert_eq!(
        doc.read_asset_bytes("scene.usdz[x.usdz[y.png]]", None)
            .unwrap_err()
            .kind,
        IoErrorKind::Unsupported
    );
    assert_eq!(doc.store.layers.len(), layers);
    assert_eq!(doc.store.layers[&member].generation(), generation);
    assert!(!doc.is_dirty(member));
}

#[test]
fn candidate_package_bytes_publish_only_on_commit_and_keep_validated_handles() {
    fn package(value: u8) -> Vec<u8> {
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new("root.usda", b"#usda 1.0\ndef \"Root\" {}"),
            layerstack_usdz::PackageFile::new("textures/color.png", &[value]),
        ])
        .unwrap()
    }
    let mut backend = Memory::default();
    backend.files.insert("scene.usdz".into(), package(1));
    let mut doc = StageDocument::open(backend, "scene.usdz", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let old = doc
        .read_asset_bytes("textures/color.png", Some(root))
        .unwrap();
    doc.storage.files.insert("scene.usdz".into(), package(2));
    let validated = {
        let mut candidate = doc.prepare_reload(ReloadPolicy::PreserveDirty).unwrap();
        candidate
            .read_asset_bytes("textures/color.png", Some(root))
            .unwrap()
    };
    assert_eq!(validated.bytes.as_ref(), &[2]);
    assert_eq!(
        doc.read_asset_bytes("textures/color.png", Some(root))
            .unwrap()
            .bytes
            .as_ref(),
        &[1]
    );
    let mut candidate = doc.prepare_reload(ReloadPolicy::PreserveDirty).unwrap();
    let committed = candidate
        .read_asset_bytes("textures/color.png", Some(root))
        .unwrap();
    candidate.commit();
    assert_eq!(
        doc.read_asset_bytes("textures/color.png", Some(root))
            .unwrap()
            .bytes
            .as_ref(),
        committed.bytes.as_ref()
    );
    assert_eq!(old.bytes.as_ref(), &[1]);
    assert_eq!(
        doc.storage.reads.len(),
        3,
        "one archive read per preparation and none at commit"
    );
}

#[test]
fn explicit_package_bytes_work_without_a_loaded_package_and_keep_decode_errors() {
    let mut backend = Memory::with(&[("root.usda", "#usda 1.0\ndef \"Root\" {}")]);
    backend.files.insert(
        "images.usdz".into(),
        layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new("root.usda", b"#usda 1.0"),
            layerstack_usdz::PackageFile::new("image.exr", &[0, 255, 0, 128]),
        ])
        .unwrap(),
    );
    backend
        .files
        .insert("broken.usdz".into(), b"not a zip archive".to_vec());
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    let count = doc.store.layers.len();
    assert_eq!(
        doc.read_asset_bytes("images.usdz[./image.exr]", None)
            .unwrap()
            .bytes
            .as_ref(),
        &[0, 255, 0, 128]
    );
    assert_eq!(
        doc.read_asset_bytes("images.usdz[absent]", None)
            .unwrap_err()
            .kind,
        IoErrorKind::NotFound
    );
    assert_eq!(
        doc.read_asset_bytes("broken.usdz[image.exr]", None)
            .unwrap_err()
            .kind,
        IoErrorKind::Rejected
    );
    assert_eq!(doc.store.layers.len(), count);
}

#[test]
fn forgetting_a_candidate_leaves_the_published_document_usable() {
    let mut doc = StageDocument::open(
        Memory::with(&[("root.usda", "#usda 1.0\ndef \"Root\" { int value = 1 }")]),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let identity = doc.store.identity();
    let path = doc.store.path("/Root");
    doc.storage.files.insert(
        "root.usda".into(),
        b"#usda 1.0\ndef \"CandidateOnly\" {}".to_vec(),
    );
    let candidate = doc.prepare_reload(ReloadPolicy::PreserveDirty).unwrap();
    assert_ne!(
        candidate.store().identity(),
        identity,
        "candidate names cannot alias a published domain"
    );
    core::mem::forget(candidate);
    assert_eq!(doc.store.identity(), identity);
    assert_eq!(doc.store.paths.display(path, &doc.store.tokens), "/Root");
    assert_eq!(value(&doc), Value::Int(1));
    assert!(doc.store.tokens.lookup("CandidateOnly").is_none());
    set_value(&mut doc, root, 2);
    assert_eq!(value(&doc), Value::Int(2));
    assert_eq!(doc.save().saved, [root]);
}

#[test]
fn bounded_resources_reject_before_storage_or_member_payload_copies() {
    let package = layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile::new("root.usda", b"#usda 1.0"),
        layerstack_usdz::PackageFile::new("image.png", &[1; 32]),
    ])
    .unwrap();
    let mut memory = Memory::default();
    memory.files.insert("scene.usdz".into(), package);
    memory.files.insert("loose.png".into(), vec![2; 32]);
    let mut doc = StageDocument::open(memory, "scene.usdz", StageOptions::default()).unwrap();
    doc.storage.reads.clear();
    let limits = AssetReadLimits {
        bytes: 4,
        package_bytes: 4096,
    };
    let root = doc.stage.stage().root_layer().unwrap();
    assert!(
        doc.read_asset_bytes_bounded("./image.png", Some(root), limits)
            .is_err()
    );
    assert!(
        doc.read_asset_bytes_bounded("loose.png", None, limits)
            .is_err()
    );
    assert!(
        doc.storage.reads.is_empty(),
        "oversized resource payloads are not read/copied"
    );
    let mut candidate = doc
        .prepare_reload_root(ReloadPolicy::PreserveDirty)
        .unwrap();
    assert!(
        candidate
            .read_asset_bytes_bounded("./image.png", Some(root), limits)
            .is_err()
    );
    drop(candidate);
    let limits = AssetReadLimits {
        bytes: 32,
        ..limits
    };
    assert_eq!(
        doc.read_asset_bytes_bounded("./image.png", Some(root), limits)
            .unwrap()
            .bytes
            .as_ref(),
        &[1; 32]
    );
}

#[test]
fn bounded_reads_never_fall_back_to_a_transport_without_limit_support() {
    struct UnboundedOnly;
    impl Storage for UnboundedOnly {
        fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
            Ok(asset.into())
        }
        fn read(&mut self, identifier: &str) -> Result<Vec<u8>, IoError> {
            assert_eq!(
                identifier, "root.usda",
                "a resource must never reach unbounded transport"
            );
            Ok(b"#usda 1.0".to_vec())
        }
        fn write(&mut self, _: &str, _: &[u8]) -> Result<(), IoError> {
            Ok(())
        }
    }
    let mut doc = StageDocument::open(UnboundedOnly, "root.usda", StageOptions::default()).unwrap();
    let limits = AssetReadLimits {
        bytes: 4,
        package_bytes: 4,
    };
    assert_eq!(
        doc.read_asset_bytes_bounded("huge.png", None, limits)
            .unwrap_err()
            .kind,
        IoErrorKind::Unsupported
    );
}

#[test]
fn root_reload_skips_removed_sources_even_when_they_introduce_missing_children() {
    let mut doc = StageDocument::open(
        Memory::with(&[
            ("root.usda", "#usda 1.0\n(subLayers=[@a.usda@])"),
            ("a.usda", "#usda 1.0\ndef \"Old\" {}"),
        ]),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let a = doc.catalog.ids["a.usda"];
    doc.storage
        .files
        .insert("root.usda".into(), b"#usda 1.0\ndef \"New\" {}".to_vec());
    doc.storage.files.insert(
        "a.usda".into(),
        b"#usda 1.0\n(subLayers=[@missing.usda@])".to_vec(),
    );
    doc.storage.reads.clear();
    let candidate = doc
        .prepare_reload_root(ReloadPolicy::PreserveDirty)
        .unwrap();
    assert_eq!(candidate.load_report().layers, vec![root]);
    candidate.commit();
    assert_eq!(doc.storage.reads, vec![String::from("root.usda")]);
    assert_eq!(
        doc.catalog.ids["a.usda"], a,
        "unused source identity remains reusable"
    );
    assert!(!doc.stage.stage().used_layers(true).contains(&a));
}

#[test]
fn root_reload_refreshes_resident_dependencies_and_protects_required_dirty_layers() {
    let mut doc = StageDocument::open(
        Memory::with(&[
            ("root.usda", "#usda 1.0\n(subLayers=[@a.usda@])"),
            ("a.usda", "#usda 1.0\ndef \"Asset\" { int value=1 }"),
        ]),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let a = doc.catalog.ids["a.usda"];
    let property = doc.store.property_path("/Asset.value");
    doc.storage.files.insert(
        "a.usda".into(),
        b"#usda 1.0\ndef \"Asset\" { int value=2 }".to_vec(),
    );
    doc.storage.reads.clear();
    let candidate = doc
        .prepare_reload_root(ReloadPolicy::PreserveDirty)
        .unwrap();
    assert_eq!(
        candidate
            .stage()
            .stage()
            .read_property(property, layerstack::Time::Default, |v| Some(v.clone()))
            .unwrap()
            .value,
        Value::Int(2)
    );
    candidate.commit();
    assert_eq!(
        doc.storage.reads,
        vec![String::from("root.usda"), String::from("a.usda")]
    );
    let mut layer = doc.store.layers[&a].clone();
    layer.set_property(
        property,
        PropertySpec::attribute().with_default(Value::Int(9)),
    );
    doc.store.insert_layer(layer);
    assert_eq!(
        doc.prepare_reload_root(ReloadPolicy::PreserveDirty)
            .unwrap_err()
            .kind,
        IoErrorKind::DirtyReload
    );
    doc.prepare_reload_root(ReloadPolicy::DiscardDirty)
        .unwrap()
        .commit();
    assert_eq!(doc.catalog.ids["a.usda"], a);
}

#[test]
fn root_reload_does_not_force_removed_package_members() {
    let before = layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile::new("root.usda", b"#usda 1.0\n(subLayers=[@old.usda@])"),
        layerstack_usdz::PackageFile::new("old.usda", b"#usda 1.0\ndef \"Old\" {}"),
    ])
    .unwrap();
    let after = layerstack_usdz::write_usdz(&[layerstack_usdz::PackageFile::new(
        "root.usda",
        b"#usda 1.0\ndef \"New\" {}",
    )])
    .unwrap();
    let mut memory = Memory::default();
    memory.files.insert("scene.usdz".into(), before);
    let mut doc = StageDocument::open(memory, "scene.usdz", StageOptions::default()).unwrap();
    let old = doc.catalog.ids["scene.usdz[old.usda]"];
    doc.storage.files.insert("scene.usdz".into(), after);
    doc.prepare_reload_root(ReloadPolicy::PreserveDirty)
        .unwrap()
        .commit();
    assert!(!doc.stage.stage().used_layers(true).contains(&old));
    assert_eq!(doc.catalog.ids["scene.usdz[old.usda]"], old);
}

#[test]
fn root_reload_renames_a_dependent_package_root_without_alias_collision() {
    let before = layerstack_usdz::write_usdz(&[layerstack_usdz::PackageFile::new(
        "oldroot.usda",
        b"#usda 1.0\ndef \"Before\" {}",
    )])
    .unwrap();
    let after = layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile::new(
            "newroot.usda",
            b"#usda 1.0\n(subLayers=[@oldroot.usda@])\ndef \"NewRoot\" {}",
        ),
        layerstack_usdz::PackageFile::new("oldroot.usda", b"#usda 1.0\ndef \"OldMember\" {}"),
    ])
    .unwrap();
    let mut memory = Memory::default();
    memory.files.insert(
        "root.usda".into(),
        b"#usda 1.0\n(subLayers=[@scene.usdz@])".to_vec(),
    );
    memory.files.insert("scene.usdz".into(), before);
    let mut doc = StageDocument::open(memory, "root.usda", StageOptions::default()).unwrap();
    let package = doc.catalog.ids["scene.usdz"];
    assert_eq!(doc.catalog.ids["scene.usdz[oldroot.usda]"], package);
    doc.storage.files.insert("scene.usdz".into(), after);
    doc.prepare_reload_root(ReloadPolicy::PreserveDirty)
        .unwrap()
        .commit();
    assert_eq!(doc.catalog.ids["scene.usdz"], package);
    assert_eq!(doc.catalog.ids["scene.usdz[newroot.usda]"], package);
    assert_ne!(doc.catalog.ids["scene.usdz[oldroot.usda]"], package);
    assert!(doc.stage.stage().composition_errors().is_empty());
    let old_member = doc.store.path("/OldMember");
    let new_root = doc.store.path("/NewRoot");
    assert!(doc.stage.stage().is_defined(old_member, &doc.store));
    assert!(doc.stage.stage().is_defined(new_root, &doc.store));
}

#[test]
fn root_reload_refreshes_expression_dependencies_and_rejects_dirty_sources() {
    let mut doc = StageDocument::open(
        Memory::with(&[
            ("root.usda", "#usda 1.0\n(expressionVariables = { string ASSET = \"asset.usda\" })\ndef \"Host\" (references = @`${ASSET}`@</Asset>) {}"),
            ("asset.usda", "#usda 1.0\ndef \"Asset\" { int value=1 }"),
        ]),
        "root.usda", StageOptions::default(),
    ).unwrap();
    let asset = doc.catalog.ids["asset.usda"];
    let property = doc.store.property_path("/Host.value");
    let original_bindings = doc.store.asset_layers.clone();
    doc.storage.files.insert(
        "asset.usda".into(),
        b"#usda 1.0\ndef \"Asset\" { int value=2 }".to_vec(),
    );
    doc.storage.reads.clear();
    {
        let candidate = doc
            .prepare_reload_root(ReloadPolicy::PreserveDirty)
            .unwrap();
        assert_eq!(
            candidate
                .stage()
                .stage()
                .read_property(property, layerstack::Time::Default, |v| Some(v.clone()))
                .unwrap()
                .value,
            Value::Int(2)
        );
        assert!(candidate.load_report().layers.contains(&asset));
    }
    assert_eq!(doc.store.asset_layers, original_bindings);
    assert_eq!(
        doc.stage
            .stage()
            .read_property(property, layerstack::Time::Default, |v| Some(v.clone()))
            .unwrap()
            .value,
        Value::Int(1)
    );
    assert_eq!(
        doc.storage.reads,
        vec![String::from("root.usda"), String::from("asset.usda")]
    );
    let authored = doc.store.property_path("/Asset.value");
    let mut layer = doc.store.layers[&asset].clone();
    layer.set_property(
        authored,
        PropertySpec::attribute().with_default(Value::Int(9)),
    );
    doc.store.insert_layer(layer);
    assert!(doc.is_dirty(asset));
    assert_eq!(
        doc.prepare_reload_root(ReloadPolicy::PreserveDirty)
            .unwrap_err()
            .kind,
        IoErrorKind::DirtyReload
    );
    assert!(doc.is_dirty(asset));
    doc.prepare_reload_root(ReloadPolicy::DiscardDirty)
        .unwrap()
        .commit();
    assert_eq!(doc.catalog.ids["asset.usda"], asset);
    assert_eq!(
        doc.stage
            .stage()
            .read_property(property, layerstack::Time::Default, |v| Some(v.clone()))
            .unwrap()
            .value,
        Value::Int(2)
    );
    assert!(!doc.is_dirty(asset));
}

#[test]
fn root_reload_refreshes_nested_expression_assets_and_recovers_missing_sources() {
    let mut doc = StageDocument::open(
        Memory::with(&[
            ("root.usda", "#usda 1.0\n(expressionVariables = { string ASSET = \"asset.usda\"\n string CHILD = \"child.usda\" })\ndef \"Host\" (references = @`${ASSET}`@</Asset>) {}"),
            ("asset.usda", "#usda 1.0\n(subLayers=[@`${CHILD}`@])\ndef \"Asset\" {}"),
            ("child.usda", "#usda 1.0\ndef \"Asset\" { int value=1 }"),
        ]), "root.usda", StageOptions::default(),
    ).unwrap();
    let property = doc.store.property_path("/Host.value");
    let child = doc.catalog.ids["child.usda"];
    doc.storage.files.insert(
        "child.usda".into(),
        b"#usda 1.0\ndef \"Asset\" { int value=2 }".to_vec(),
    );
    doc.storage.reads.clear();
    doc.prepare_reload_root(ReloadPolicy::PreserveDirty)
        .unwrap()
        .commit();
    assert_eq!(
        doc.stage
            .stage()
            .read_property(property, layerstack::Time::Default, |v| Some(v.clone()))
            .unwrap()
            .value,
        Value::Int(2)
    );
    assert_eq!(
        doc.storage.reads,
        vec![
            String::from("root.usda"),
            String::from("asset.usda"),
            String::from("child.usda")
        ]
    );
    assert_eq!(doc.catalog.ids["child.usda"], child);
    let bindings = doc.store.asset_layers.clone();
    doc.storage.files.remove("child.usda");
    assert_eq!(
        doc.prepare_reload_root(ReloadPolicy::PreserveDirty)
            .unwrap_err()
            .kind,
        IoErrorKind::NotFound
    );
    assert_eq!(doc.store.asset_layers, bindings);
    assert_eq!(
        doc.stage
            .stage()
            .read_property(property, layerstack::Time::Default, |v| Some(v.clone()))
            .unwrap()
            .value,
        Value::Int(2)
    );
    doc.storage.files.insert(
        "child.usda".into(),
        b"#usda 1.0\ndef \"Asset\" { int value=3 }".to_vec(),
    );
    doc.prepare_reload_root(ReloadPolicy::PreserveDirty)
        .unwrap()
        .commit();
    assert_eq!(
        doc.stage
            .stage()
            .read_property(property, layerstack::Time::Default, |v| Some(v.clone()))
            .unwrap()
            .value,
        Value::Int(3)
    );
    doc.storage.files.remove("child.usda");
    doc.storage.files.insert(
        "root.usda".into(),
        b"#usda 1.0\ndef \"Repaired\" {}".to_vec(),
    );
    doc.storage.reads.clear();
    doc.prepare_reload_root(ReloadPolicy::PreserveDirty)
        .unwrap()
        .commit();
    assert_eq!(doc.storage.reads, vec![String::from("root.usda")]);
    assert!(!doc.stage.stage().used_layers(true).contains(&child));
}
