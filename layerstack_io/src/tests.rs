// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;

mod deferred_arcs;
#[cfg(feature = "std")]
mod dynamic_payloads;
mod encoded_ownership;
mod file_formats;
mod package_identity;
mod registered_metadata;
mod threaded_open;

#[test]
fn sparse_clip_preparation_covers_advanced_weaker_queries_and_set_reordering() {
    use layerstack::InterpolationType;
    for reorder in [false, true] {
        let root = format!(
            r#"#usda 1.0
def "P" (
{}
clips = {{
 dictionary a = {{
  asset[] assetPaths=[@a.usda@]
  string primPath="/C"
  double2[] active=[(0,0)]
  asset manifestAssetPath=@manifest.usda@
 }}
 dictionary b = {{
  asset[] assetPaths=[@b0.usda@,@b1.usda@]
  string primPath="/C"
  double2[] active=[(0,0),(10,1)]
  asset manifestAssetPath=@manifest.usda@
 }}
}}) {{ float[] x }}
"#,
            if reorder {
                "reorder clipSets=[\"b\",\"a\"]"
            } else {
                ""
            }
        );
        let memory = Memory::with(&[
            ("root.usda", &root),
            ("manifest.usda", "#usda 1.0\ndef \"C\" {\n float[] x\n}"),
            (
                "a.usda",
                "#usda 1.0\ndef \"C\" {\n float[] x.timeSamples={0:[100,100],10:edit[write 200 to [0]]}\n}",
            ),
            (
                "b0.usda",
                "#usda 1.0\ndef \"C\" {\n float[] x.timeSamples={0:[0,0],3:[3,3]}\n}",
            ),
            (
                "b1.usda",
                "#usda 1.0\ndef \"C\" {\n float[] x.timeSamples={10:[10,10],20:[20,20]}\n}",
            ),
        ]);
        let mut document =
            StageDocument::open(memory, "root.usda", StageOptions::default()).unwrap();
        let property = document.store_mut().property_path("/P.x");
        let prepared = document.prepare_clip_query(property, 2., InterpolationType::Linear, 8, 16);
        assert!(prepared.plan.is_complete(), "{prepared:?}");
        let resolved = document
            .stage()
            .stage()
            .resolve_property_path_at_time(property, 2., InterpolationType::Linear)
            .unwrap()
            .value;
        let expected = if reorder {
            vec![2., 2.]
        } else {
            vec![120., 82.]
        };
        let array = resolved.array_ref().unwrap();
        assert_eq!(
            array.iter().map(|v| v.into_owned()).collect::<Vec<_>>(),
            expected.into_iter().map(Value::Float).collect::<Vec<_>>()
        );
        assert_eq!(
            document.storage.reads.iter().any(|s| s == "a.usda"),
            !reorder
        );
        assert_eq!(
            document.storage.reads.iter().any(|s| s == "b1.usda"),
            !reorder,
            "only the sparse stronger set advances to the next clip"
        );
    }
}

#[test]
fn dense_clip_preparation_does_not_load_masked_sets_or_dormant_assets() {
    use layerstack::InterpolationType;
    for (source, manifest, expected) in [
        (
            Some("#usda 1.0\ndef \"C\" {\n double x.timeSamples={0:42,3:42}\n}"),
            "double x",
            Some(Value::Double(42.)),
        ),
        (None, "double x", None),
        (None, "double x=9", Some(Value::Double(9.))),
    ] {
        let manifest = format!("#usda 1.0\ndef \"C\" {{\n {manifest}\n}}");
        let mut memory = Memory::with(&[
            (
                "root.usda",
                r#"#usda 1.0
def "P" (clips = {
 dictionary a = {
  asset[] assetPaths=[@a.usda@,@dormant.usdc@]
  string primPath="/C"
  double2[] active=[(0,0),(100,1)]
  asset manifestAssetPath=@a-manifest.usda@
 }
 dictionary b = {
  asset[] assetPaths=[@masked.usdc@]
  string primPath="/C"
  double2[] active=[(0,0)]
  asset manifestAssetPath=@b-manifest.usda@
 }
}) { double x }
"#,
            ),
            ("a-manifest.usda", &manifest),
            ("b-manifest.usda", "#usda 1.0\ndef \"C\" {\n double x\n}"),
            ("masked.usdc", "malformed masked source"),
            ("dormant.usdc", "malformed dormant source"),
        ]);
        if let Some(source) = source {
            memory
                .files
                .insert("a.usda".into(), source.as_bytes().to_vec());
        }
        if manifest.contains("=9") {
            let root = String::from_utf8(memory.files["root.usda"].clone()).unwrap();
            memory.files.insert(
                "root.usda".into(),
                root.replace(
                    "double2[] active=[(0,0),(100,1)]",
                    "double2[] active=[(0,0)]",
                )
                .into_bytes(),
            );
        }
        let mut document =
            StageDocument::open(memory, "root.usda", StageOptions::default()).unwrap();
        let property = document.store_mut().property_path("/P.x");
        let prepared = document.prepare_clip_query(property, 2., InterpolationType::Linear, 8, 16);
        assert!(prepared.plan.is_complete(), "{prepared:?}");
        assert!(prepared.failures.is_empty(), "{prepared:?}");
        assert_eq!(prepared.load_attempts, 2);
        assert!(!document.storage.reads.iter().any(|name| {
            matches!(
                name.as_str(),
                "masked.usdc" | "dormant.usdc" | "b-manifest.usda"
            )
        }));
        assert_eq!(
            document
                .stage()
                .stage()
                .resolve_property_path_at_time(property, 2., InterpolationType::Linear)
                .map(|resolved| resolved.value),
            expected
        );
    }
}

#[cfg(feature = "std")]
#[test]
fn clip_query_planning_does_not_decode_retained_interpolation_endpoints() {
    use layerstack::{InterpolationType, TypedArray};
    let values = |value: &str| vec![value; 64].join(",");
    let source = format!(
        "#usda 1.0\ndef \"C\" {{\n float[] x.timeSamples={{0:[{}],10:[{}]}}\n}}",
        values("1"),
        values("3"),
    );
    let source = StageDocument::open(
        Memory::with(&[("clip.usda", &source)]),
        "clip.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = source.stage().stage().root_layer().unwrap();
    let bytes = layerstack_usdc::writer::save_layer(
        &source.store.layers[&root],
        &source.store.tokens,
        &source.store.paths,
    )
    .unwrap();
    let mut memory = Memory::with(&[
        (
            "root.usda",
            r#"#usda 1.0
def "P" (clips = { dictionary default = {
 asset[] assetPaths = [@clip.usdc@]
 string primPath = "/C"
 double2[] active = [(0,0)]
 asset manifestAssetPath = @manifest.usda@
 }}) { float[] x }
"#,
        ),
        ("manifest.usda", "#usda 1.0\ndef \"C\" {\n float[] x\n}"),
    ]);
    memory.files.insert("clip.usdc".into(), bytes);
    let mut document = StageDocument::open_with(
        memory,
        "root.usda",
        StageOptions::default(),
        retained_options(),
    )
    .unwrap();
    let property = document.store_mut().property_path("/P.x");
    let prepared = document.prepare_clip_query(property, 5., InterpolationType::Linear, 2, 16);
    assert!(prepared.plan.is_complete(), "{prepared:?}");
    assert_eq!(prepared.load_attempts, 2);
    let clip = document.catalog.ids["clip.usdc"];
    assert_eq!(
        document
            .retained_values(clip)
            .unwrap()
            .stats()
            .decode_attempts,
        0,
        "residency planning must not evaluate the numeric interpolation"
    );
    let prepared = document.prepare_clip_query(property, 5., InterpolationType::Linear, 0, 16);
    assert!(prepared.plan.is_complete(), "{prepared:?}");
    assert_eq!(prepared.load_attempts, 0);
    let resolved = document
        .stage()
        .stage()
        .try_resolve_property_path_at_time(property, 5., InterpolationType::Linear)
        .unwrap()
        .unwrap();
    let Value::TypedArray(TypedArray::Float(values)) = resolved.value else {
        panic!("float array interpolation");
    };
    assert_eq!(values.as_slice(), &[2.; 64]);
    assert_eq!(
        document
            .retained_values(clip)
            .unwrap()
            .stats()
            .decode_attempts,
        2
    );
}

#[test]
fn demanded_clip_failures_distinguish_missing_from_malformed_and_allow_retry() {
    use layerstack::InterpolationType;
    let memory = Memory::with(&[
        (
            "root.usda",
            r#"#usda 1.0
def "P" (clips = { dictionary default = {
 asset[] assetPaths = [@needed.usdc@]
 string primPath = "/C"
 double2[] active = [(0,0)]
 asset manifestAssetPath = @manifest.usda@
 }}) { double x }
"#,
        ),
        ("manifest.usda", "#usda 1.0\ndef \"C\" {\n double x\n}"),
        ("needed.usdc", "malformed required source"),
    ]);
    let mut doc =
        StageDocument::open(memory.clone(), "root.usda", StageOptions::default()).unwrap();
    let property = doc.store.property_path("/P.x");
    let failed = doc.prepare_clip_query(property, 2., InterpolationType::Linear, 4, 16);
    assert!(!failed.plan.is_complete());
    assert_eq!(failed.failures.len(), 1);
    assert_ne!(failed.failures[0].error.kind, IoErrorKind::NotFound);
    let source = StageDocument::open(
        Memory::with(&[(
            "needed.usda",
            "#usda 1.0\ndef \"C\" {\n double x.timeSamples={0:7,10:7}\n}",
        )]),
        "needed.usda",
        StageOptions::default(),
    )
    .unwrap();
    let id = source.stage.stage().root_layer().unwrap();
    let bytes = layerstack_usdc::writer::save_layer(
        &source.store.layers[&id],
        &source.store.tokens,
        &source.store.paths,
    )
    .unwrap();
    doc.storage.files.insert("needed.usdc".into(), bytes);
    let retried = doc.prepare_clip_query(property, 2., InterpolationType::Linear, 4, 16);
    assert!(retried.plan.is_complete(), "{retried:?}");
    assert_eq!(retried.load_attempts, 1);
    assert!(retried.failures.is_empty());

    let mut memory = memory;
    memory.files.remove("needed.usdc");
    let mut missing = StageDocument::open(memory, "root.usda", StageOptions::default()).unwrap();
    let property = missing.store.property_path("/P.x");
    let gap = missing.prepare_clip_query(property, 2., InterpolationType::Linear, 4, 16);
    assert!(gap.plan.is_complete(), "{gap:?}");
    assert!(gap.failures.is_empty());
    assert!(gap.plan.demands.iter().any(|d| matches!(
        d.request.status,
        layerstack::value_clips::ClipAssetStatus::Unavailable(
            layerstack::value_clips::ClipAssetUnavailable::Missing
        )
    )));
}

#[test]
fn unsupported_package_resources_are_reported_before_export() {
    let memory = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\ndef \"P\" {\n asset blob=@data.bin@\n}",
        ),
        ("data.bin", "opaque unsupported resource"),
    ]);
    let mut document = StageDocument::open(memory, "root.usda", StageOptions::default()).unwrap();
    let root = document.stage.stage().root_layer().unwrap();
    let prepared = document
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(!prepared.report().is_complete());
    assert!(prepared.plan().is_none());
    assert!(prepared.write_usdz().is_err());
}

#[cfg(feature = "std")]
#[test]
fn external_package_resources_keep_their_original_filesystem_anchor() {
    use std::fs;
    #[cfg(target_os = "wasi")]
    let directory = std::env::current_dir()
        .unwrap()
        .join("layerstack-package-external-wasi");
    #[cfg(not(target_os = "wasi"))]
    let directory = std::env::temp_dir().join(format!(
        "layerstack-package-external-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("root.usda"),
        b"#usda 1.0\n(subLayers=[@child.usda@])",
    )
    .unwrap();
    fs::write(
        directory.join("child.usda"),
        b"#usda 1.0\ndef \"P\" {\n asset image=@./image.png@\n}",
    )
    .unwrap();
    fs::write(directory.join("image.png"), b"pixels").unwrap();
    let mut document = StageDocument::open(
        Filesystem::new(&directory, []).unwrap(),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = document.stage.stage().root_layer().unwrap();
    let package = document
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |dependency| {
                if dependency.identifier.ends_with("image.png") {
                    PackageInclusion::KeepExternal
                } else {
                    PackageInclusion::Include
                }
            },
        )
        .unwrap();
    assert!(package.report().is_complete());
    let plan = package.plan().unwrap();
    assert!(plan.assets.is_empty());
    let image = plan
        .layers
        .iter()
        .flat_map(|layer| {
            layerstack::asset_dependencies::extract_layer_dependencies(
                &layer.layer,
                &document.store.paths,
                &document.store.tokens,
            )
        })
        .find(|dependency| dependency.identifier.ends_with("image.png"))
        .unwrap();
    assert_eq!(
        image.identifier.as_ref(),
        fs::canonicalize(directory.join("image.png"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn query_preparation_resumes_and_leaves_dormant_malformed_clips_unread() {
    use layerstack::{InterpolationType, PropertyPath};
    let memory = Memory::with(&[
        (
            "root.usda",
            r#"#usda 1.0
def "P" (clips = { dictionary default = {
 asset[] assetPaths = [@first.usda@, @next.usda@, @dormant.usdc@]
 string primPath = "/C"
 double2[] active = [(0,0),(10,1),(100,2)]
 asset manifestAssetPath = @manifest.usda@
 }}) { double x }
"#,
        ),
        (
            "first.usda",
            "#usda 1.0\ndef \"C\" {\n double x.timeSamples={0:0,3:3}\n}",
        ),
        (
            "next.usda",
            "#usda 1.0\ndef \"C\" {\n double x.timeSamples={10:10,100:100}\n}",
        ),
        ("manifest.usda", "#usda 1.0\ndef \"C\" {\n double x\n}"),
        ("dormant.usdc", "malformed dormant source"),
    ]);
    let mut doc = StageDocument::open(memory, "root.usda", StageOptions::default()).unwrap();
    let property = doc.store.property_path("/P.x");
    let first = doc.prepare_clip_query(property, 2., InterpolationType::Linear, 2, 16);
    assert!(first.plan.is_complete(), "{first:?}");
    assert_eq!(first.load_attempts, 2);
    assert!(
        !doc.storage
            .reads
            .iter()
            .any(|s| s == "next.usda" || s == "dormant.usdc")
    );
    let held = doc.prepare_clip_query(property, 5., InterpolationType::Held, 0, 16);
    assert!(held.plan.is_complete(), "{held:?}");
    let pending = doc.prepare_clip_query(property, 5., InterpolationType::Linear, 0, 16);
    assert!(pending.budget_exhausted);
    assert_eq!(
        pending
            .plan
            .pending()
            .next()
            .unwrap()
            .request
            .identifier
            .as_ref(),
        "next.usda"
    );
    let next = doc.prepare_clip_query(property, 5., InterpolationType::Linear, 1, 16);
    assert!(next.plan.is_complete(), "{next:?}");
    assert_eq!(next.load_attempts, 1);
    assert_eq!(
        doc.stage
            .stage()
            .resolve_property_path_at_time(property, 5., InterpolationType::Linear)
            .unwrap()
            .value,
        Value::Double(5.)
    );
    assert!(!doc.storage.reads.iter().any(|s| s == "dormant.usdc"));
    assert_eq!(
        doc.prepare_clip_query(property, 5., InterpolationType::Linear, 0, 16)
            .load_attempts,
        0
    );
    let unknown = PropertyPath::new(property.prim_path(), doc.store.tokens.intern("absent"));
    assert!(
        doc.stage
            .stage()
            .plan_clip_query(unknown, 5., InterpolationType::Linear, 16)
            .is_complete()
    );
    assert!(
        !doc.stage
            .stage()
            .plan_clip_query(property, f64::NAN, InterpolationType::Linear, 16)
            .is_complete()
    );
    assert!(
        doc.stage
            .stage()
            .plan_clip_query(property, 5., InterpolationType::Linear, 0)
            .budget_exhausted
    );
}

#[test]
fn reference_custom_data_resources_are_localized_and_missing_assets_prevent_export() {
    // Scope this resolver to the fixture: existing Memory tests intentionally
    // use opaque identifiers, while these resources must retain the layer anchor.
    struct AnchoredMemory(Memory);
    impl Storage for AnchoredMemory {
        fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
            Ok(anchor
                .and_then(|anchor| layerstack::asset::anchor_asset_path(asset, anchor))
                .unwrap_or_else(|| asset.into()))
        }
        fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
            self.0.read(identifier)
        }
        fn read_bounded(&self, identifier: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
            self.0.read_bounded(identifier, max_bytes)
        }
        fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
            self.0.write(identifier, bytes)
        }
    }
    let source = r#"#usda 1.0
def "Library" {
 int answer = 42
}
def "Root" (
 references = </Library> (
  customData = {
   dictionary images = {
    asset preview = @./preview.png@
    asset[] details = [@./detail.png@, @./preview.png@]
    string label = "literal.png"
   }
  }
 )
) {}
"#;
    let memory = AnchoredMemory(Memory::with(&[
        ("source/root.usda", source),
        ("source/preview.png", "preview pixels"),
        ("source/detail.png", "detail pixels"),
    ]));
    let mut doc = StageDocument::open(memory, "source/root.usda", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let original = doc.store.layers[&root].clone();
    let package = doc
        .prepare_package(
            root,
            "scenes/scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(package.report().is_complete(), "{:?}", package.report());
    assert_eq!(package.report().dependencies.len(), 2);
    let plan = package.plan().unwrap();
    assert_eq!(plan.layers.len(), 1, "the reference is internal");
    assert_eq!(plan.assets.len(), 2, "duplicate uses share resource bytes");
    let dependencies = layerstack::asset_dependencies::extract_layer_dependencies(
        &plan.layers[0].layer,
        &doc.store.paths,
        &doc.store.tokens,
    );
    assert_eq!(dependencies.len(), 2);
    assert!(
        dependencies
            .iter()
            .all(|dependency| dependency.identifier.starts_with("../assets/"))
    );
    assert_eq!(
        doc.store.layers[&root], original,
        "preparation leaves source data intact"
    );
    assert_eq!(doc.store.layers[&root].generation(), original.generation());
    assert!(doc.storage.0.writes.is_empty());
    doc.export_package("out.usdz", &package).unwrap();

    let mut output = Memory::default();
    output
        .files
        .insert("out.usdz".into(), doc.storage.0.files["out.usdz"].clone());
    let mut reopened = StageDocument::open(output, "out.usdz", StageOptions::default()).unwrap();
    let reopened_root = reopened.stage.stage().root_layer().unwrap();
    let dependencies = layerstack::asset_dependencies::extract_layer_dependencies(
        &reopened.store.layers[&reopened_root],
        &reopened.store.paths,
        &reopened.store.tokens,
    );
    assert_eq!(
        dependencies.len(),
        2,
        "custom data survives serialization and import"
    );
    let mut contents = Vec::new();
    for dependency in dependencies {
        assert!(dependency.identifier.starts_with("../assets/"));
        let resource = reopened
            .read_asset_bytes(&dependency.identifier, Some(reopened_root))
            .unwrap();
        contents.push(resource.bytes.as_ref().to_vec());
    }
    contents.sort();
    assert_eq!(
        contents,
        vec![b"detail pixels".to_vec(), b"preview pixels".to_vec()]
    );
    let answer = reopened.store.property_path("/Root.answer");
    assert_eq!(
        reopened
            .stage
            .stage()
            .resolve_field_path(answer)
            .unwrap()
            .value,
        Value::Int(42)
    );

    doc.storage.0.files.remove("source/detail.png");
    let missing = doc
        .prepare_package(
            root,
            "scenes/scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(!missing.report().is_complete(), "{:?}", missing.report());
    assert!(missing.plan().is_none());
    assert!(missing.report().dependencies.iter().any(|dependency| {
        dependency.source.identifier.as_ref() == "./detail.png"
            && dependency.targets.iter().any(|target| {
                matches!(
                    target,
                    layerstack::asset_dependencies::DependencyTarget::Unresolved(_)
                )
            })
    }));
    assert!(doc.export_package("missing.usdz", &missing).is_err());
    assert!(!doc.storage.0.files.contains_key("missing.usdz"));
    assert_eq!(doc.storage.0.writes, vec!["out.usdz"]);
    assert_eq!(doc.store.layers[&root], original);
    assert_eq!(doc.store.layers[&root].generation(), original.generation());
}

#[test]
fn package_preparation_includes_unsaved_layers_and_reports_missing_resources_without_publication() {
    let memory = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\ndef \"Root\" (references=@child.usda@</Child>) {}",
        ),
        (
            "child.usda",
            "#usda 1.0\ndef \"Child\" {\n int answer=42\n asset image=@image.png@\n}",
        ),
        ("image.png", "pixels"),
    ]);
    let mut doc = StageDocument::open(memory, "root.usda", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let child = *doc.catalog.ids.get("child.usda").unwrap();
    let source = doc.store.path("/Child");
    let answer = doc.store.tokens.intern("answer");
    let layer = doc.store.layers.get_mut(&child).unwrap();
    let property = layer
        .prim_mut(source)
        .unwrap()
        .properties
        .iter_mut()
        .find(|p| p.name == answer)
        .unwrap();
    Arc::make_mut(&mut property.spec).default = Some(Value::Int(99));
    let generation = layer.generation();
    let package = doc
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(package.report().is_complete(), "{:?}", package.report());
    assert_eq!(package.plan().unwrap().layers.len(), 2);
    assert_eq!(package.plan().unwrap().assets.len(), 1);
    assert_eq!(doc.store.layers[&child].generation(), generation);
    assert!(doc.is_dirty(child));
    assert!(doc.storage.writes.is_empty());
    doc.export_package("out.usdz", &package).unwrap();
    let bytes = doc.storage.files["out.usdz"].clone();
    let mut output = Memory::default();
    output.files.insert("out.usdz".into(), bytes);
    let mut reopened = StageDocument::open(output, "out.usdz", StageOptions::default()).unwrap();
    let property = reopened.store.property_path("/Root.answer");
    assert_eq!(
        reopened
            .stage
            .stage()
            .resolve_field_path(property)
            .unwrap()
            .value,
        Value::Int(99)
    );
    doc.storage.files.remove("image.png");
    let missing = doc
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(!missing.report().is_complete());
    assert!(missing.plan().is_none());
    assert!(missing.write_usdz().is_err());
    let external = doc
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |dependency| {
                if dependency.identifier.as_ref() == "image.png" {
                    PackageInclusion::KeepExternal
                } else {
                    PackageInclusion::Include
                }
            },
        )
        .unwrap();
    assert!(external.report().is_complete());
    assert!(external.plan().unwrap().assets.is_empty());
}

#[test]
fn nested_packages_resolve_defaults_explicit_layers_relative_dependencies_and_resources() {
    let inner = layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile::new("dir/root.usda", b"#usda 1.0\n(defaultPrim=\"Inside\")\ndef \"Inside\" (references=@./child.usda@</Child>) {}"),
        layerstack_usdz::PackageFile::new("dir/child.usda", b"#usda 1.0\ndef \"Child\" {\n int answer=42\n}"),
        layerstack_usdz::PackageFile::new("dir/data.png", b"inner pixels"),
    ]).unwrap();
    for reference in ["inner.usdz", "inner.usdz[dir/root.usda]"] {
        let source =
            alloc::format!("#usda 1.0\ndef \"Root\" (references=@{reference}@</Inside>) {{}}");
        let outer = layerstack_usdz::write_usdz(&[
            layerstack_usdz::PackageFile::new("root.usda", source.as_bytes()),
            layerstack_usdz::PackageFile::new("inner.usdz", &inner),
        ])
        .unwrap();
        let mut memory = Memory::default();
        memory.files.insert("outer.usdz".into(), outer);
        let mut document =
            StageDocument::open(memory, "outer.usdz", StageOptions::default()).unwrap();
        let root = document.store.path("/Root");
        let answer = document.store.tokens.intern("answer");
        assert_eq!(
            document
                .stage
                .stage()
                .resolve_field_path(layerstack::PropertyPath::new(root, answer))
                .unwrap_or_else(|| panic!(
                    "{:?} {:?} {:?}",
                    document.stage.stage().composition_errors(),
                    document.load_report,
                    document.store.layers
                ))
                .value,
            Value::Int(42)
        );
        let inside = document
            .load_report
            .layers
            .iter()
            .copied()
            .find(|id| {
                document
                    .identifier(*id)
                    .unwrap()
                    .ends_with("inner.usdz[dir/root.usda]]")
            })
            .unwrap();
        let first = document
            .read_asset_bytes("./data.png", Some(inside))
            .unwrap();
        let second = document
            .read_asset_bytes("outer.usdz[inner.usdz[dir/data.png]]", None)
            .unwrap();
        assert_eq!(first.bytes.as_ref(), b"inner pixels");
        assert!(first.bytes.shares_backing(&second.bytes));
        assert_eq!(document.package_stats(inside).unwrap().index_parses, 2);
        assert_eq!(document.storage.reads.len(), 1);
        let (explicit, _) = document
            .load_asset("outer.usdz[inner.usdz[dir/root.usda]]", None)
            .unwrap();
        assert_eq!(explicit, inside);
        let storage = document.storage.clone();
        let mut explicit = StageDocument::open(
            storage,
            "outer.usdz[inner.usdz[dir/root.usda]]",
            StageOptions::default(),
        )
        .unwrap();
        assert_eq!(
            explicit
                .read_asset_bytes(
                    "./data.png",
                    Some(explicit.stage.stage().root_layer().unwrap())
                )
                .unwrap()
                .bytes
                .as_ref(),
            b"inner pixels"
        );
        let mut candidate = document
            .prepare_reload_root(ReloadPolicy::PreserveDirty)
            .unwrap();
        let fresh = candidate
            .read_asset_bytes("./data.png", Some(inside))
            .unwrap();
        assert!(!first.bytes.shares_backing(&fresh.bytes));
        drop(candidate);
        assert!(
            first.bytes.shares_backing(
                &document
                    .read_asset_bytes("./data.png", Some(inside))
                    .unwrap()
                    .bytes
            )
        );
        assert_eq!(first.bytes.as_ref(), b"inner pixels");
    }
}
use alloc::{format, vec};
use layerstack::{EditTarget, PrimSpec, PropertySpec, Transaction, Value};

/// The sources a storage read, in order; storages read through `&self`.
#[derive(Debug, Default)]
struct Reads(std::sync::Mutex<Vec<String>>);
impl Reads {
    fn list(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.0.lock().unwrap()
    }
    fn push(&self, identifier: String) {
        self.list().push(identifier);
    }
    fn iter(&self) -> vec::IntoIter<String> {
        self.list().clone().into_iter()
    }
    fn len(&self) -> usize {
        self.list().len()
    }
    fn is_empty(&self) -> bool {
        self.list().is_empty()
    }
    fn clear(&self) {
        self.list().clear();
    }
}
impl Clone for Reads {
    fn clone(&self) -> Self {
        Self(std::sync::Mutex::new(self.list().clone()))
    }
}
impl PartialEq<Vec<String>> for Reads {
    fn eq(&self, other: &Vec<String>) -> bool {
        *self.list() == *other
    }
}

/// How many times a storage identified an asset.
#[derive(Debug, Default)]
struct Count(core::sync::atomic::AtomicUsize);
impl Count {
    fn get(&self) -> usize {
        self.0.load(core::sync::atomic::Ordering::Relaxed)
    }
    fn add(&self) {
        self.0.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}
impl Clone for Count {
    fn clone(&self) -> Self {
        Self(core::sync::atomic::AtomicUsize::new(self.get()))
    }
}

#[derive(Clone, Debug, Default)]
struct Memory {
    files: BTreeMap<String, Vec<u8>>,
    writes: Vec<String>,
    reads: Reads,
    identifies: Count,
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
        self.identifies.add();
        Ok(asset.into())
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.reads.push(identifier.into());
        self.files
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, format!("missing {identifier}")))
    }
    fn read_bounded(&self, identifier: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
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
        let prepared = document
            .prepare_package(root, member, PackagePreparationLimits::default(), |_| {
                PackageInclusion::Include
            })
            .unwrap();
        assert!(prepared.report().is_complete());
        let failure = document
            .export_package("failed-prepared.usdz", &prepared)
            .unwrap_err();
        assert_eq!(failure.kind, IoErrorKind::Decode);
        assert_eq!(
            failure.array_read_error,
            Some(layerstack::ArrayReadError::BudgetExceeded { limit })
        );
        assert!(
            !document
                .storage()
                .files
                .contains_key("failed-prepared.usdz")
        );
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
            PropertySpec::typed_attribute(layerstack::PropertyType::parse("int").unwrap())
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
fn repeated_references_identify_each_asset_once_per_operation() {
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\ndef \"A\" (references = @asset.usda@</Asset>) {}\ndef \"B\" (references = @asset.usda@</Asset>) {}\ndef \"C\" (payload = @asset.usda@</Asset>) {}\n",
        ),
        ("asset.usda", "#usda 1.0\ndef \"Asset\" {}\n"),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    assert!(doc.stage.stage().has_prim(doc.store.path("/C")));
    // The root and one shared asset; the three arcs share one identity.
    assert_eq!(doc.storage.identifies.get(), 2);
    // A later operation asks the transport again rather than trusting a
    // cache that could hide files created or removed since.
    let root = doc.stage.stage().root_layer().unwrap();
    doc.load_asset("asset.usda", Some(root)).unwrap();
    assert_eq!(doc.storage.identifies.get(), 3);
}
#[test]
fn value_edits_in_one_variant_branch_refresh_without_recomposing_sibling_branches() {
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\ndef \"Red\" (references = @asset.usda@</Asset>\n variants = { string look = \"red\" }) {}\ndef \"Blue\" (references = @asset.usda@</Asset>\n variants = { string look = \"blue\" }) {}\n",
        ),
        (
            "asset.usda",
            "#usda 1.0\ndef \"Asset\" (variantSets = [\"look\"]) {\n variantSet \"look\" = {\n \"red\" { def \"Geom\" { double size = 1 } }\n \"blue\" { def \"Geom\" { double size = 2 } }\n }\n}\n",
        ),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    let asset = *doc
        .store
        .layers
        .keys()
        .find(|&&id| doc.identifier(id) == Some("asset.usda"))
        .unwrap();
    let red = doc.store.property_path("/Red/Geom.size");
    let blue = doc.store.property_path("/Blue/Geom.size");
    let spec = layerstack::SpecPath::parse(
        "/Asset{look=red}Geom.size",
        &doc.store.tokens,
        &doc.store.paths,
    )
    .unwrap();
    let mut edit = Transaction::new();
    edit.set_default(layerstack::Address::spec(asset, spec), Value::Double(5.0));
    let (store, stage) = doc.parts_mut();
    let applied = stage.apply(store, &edit).unwrap();
    // `/Blue/Geom` composes the sibling branch's spec of the same prim in
    // the same layer; it keeps its slots rather than forcing composition.
    assert_eq!(applied.recomposed, [red.prim_path()]);
    assert_eq!(applied.changes.changed_info_only, [red.prim_path()]);
    let resolved = |path| doc.stage.stage().resolve_field_path(path).unwrap().value;
    assert_eq!(resolved(red), Value::Double(5.0));
    assert_eq!(resolved(blue), Value::Double(2.0));
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
    assert!(doc.store.layers[&member].prims().contains_key(&new));
    assert!(!doc.store.layers[&member].prims().contains_key(&old));
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
        fn read(&self, _: &str) -> Result<Vec<u8>, IoError> {
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
        assert!(doc.store.layers[&id].prims().contains_key(&new));
        assert!(!doc.store.layers[&id].prims().contains_key(&old));
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
    assert!(doc.store.layers[&member].prims().contains_key(&new));
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
        IoErrorKind::Rejected
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
        IoErrorKind::NotFound
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
        fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
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
