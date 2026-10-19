// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Registered file formats build layers from a source and its format
//! arguments, which name distinct layers.

use super::*;
use layerstack::{
    LayerOffset, PrimSpec, PropertySpec, PropertyType, Reference, ReferenceTarget, SublayerEntry,
    Time,
};

/// `/Grid` with one `RowN` child per row (`rows`, default 2), each with a
/// `height` of `N`. The source names the grid's `label`. The layer
/// sublayers `base.usda` and references `part.usda` from `/Grid/Row0`,
/// both by asset path.
#[derive(Debug)]
struct Grid;
impl FileFormat for Grid {
    fn name(&self) -> &str {
        "grid"
    }
    fn extensions(&self) -> &[&str] {
        &["grid"]
    }
    fn default_args(&self) -> FormatArgs {
        FormatArgs::new().with("rows", "2")
    }
    fn read(&self, source: &FormatSource<'_>) -> Result<Layer, IoError> {
        let label = core::str::from_utf8(source.bytes.expect("grid reads its source"))
            .map_err(|_| source.rejected("the label is not text"))?
            .trim();
        if label.is_empty() {
            return Err(source.rejected("the grid has no label"));
        }
        let rows: usize = source
            .args
            .get("rows")
            .unwrap_or("2")
            .parse()
            .map_err(|_| source.rejected("`rows` must be a count"))?;
        let mut layer = Layer::new(source.layer);
        layer.sublayers_mut().push(SublayerEntry::unresolved(
            "base.usda",
            LayerOffset::IDENTITY,
        ));
        let grid = source.path("/Grid")?;
        layer.define_prim(
            grid,
            PrimSpec::def().with_property(
                source.token("label"),
                PropertySpec::typed_attribute(PropertyType::parse("string").unwrap())
                    .with_default(Value::String(label.into())),
            ),
            source.paths,
        );
        for row in 0..rows {
            let mut spec = PrimSpec::def().with_property(
                source.token("height"),
                PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
                    .with_default(Value::Double(row as f64)),
            );
            if row == 0 {
                spec = spec.with_reference(Reference::deferred(
                    source.layer,
                    "part.usda",
                    ReferenceTarget::DefaultPrim,
                    LayerOffset::IDENTITY,
                ));
            }
            layer.define_prim(source.path(&format!("/Grid/Row{row}"))?, spec, source.paths);
        }
        Ok(layer)
    }
}

/// `/Noise` with a `seed` copied from its argument; it reads no source.
#[derive(Debug)]
struct Noise;
impl FileFormat for Noise {
    fn name(&self) -> &str {
        "noise"
    }
    fn extensions(&self) -> &[&str] {
        &["noise"]
    }
    fn reads_source(&self) -> bool {
        false
    }
    fn read(&self, source: &FormatSource<'_>) -> Result<Layer, IoError> {
        assert!(source.bytes.is_none());
        let seed: i32 = source
            .args
            .get("seed")
            .ok_or_else(|| source.rejected("noise needs a `seed`"))?
            .parse()
            .map_err(|_| source.rejected("`seed` must be an integer"))?;
        let mut layer = Layer::new(source.layer);
        layer.define_prim(
            source.path("/Noise")?,
            PrimSpec::def().with_property(
                source.token("seed"),
                PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
                    .with_default(Value::Int(seed)),
            ),
            source.paths,
        );
        Ok(layer)
    }
}

fn formats() -> FileFormats {
    FileFormats::default()
        .with(Arc::new(Grid))
        .unwrap()
        .with(Arc::new(Noise))
        .unwrap()
}

fn scene(root: &str) -> Memory {
    Memory::with(&[
        ("root.usda", root),
        ("field.grid", "north\n"),
        (
            "base.usda",
            "#usda 1.0\nover \"Grid\" { custom string note = \"base\" }\n",
        ),
        (
            "part.usda",
            "#usda 1.0\n(\n    defaultPrim = \"Part\"\n)\ndef \"Part\" { custom int bolts = 4 }\n",
        ),
    ])
}

const ROOT: &str = r#"#usda 1.0
def "Three" (prepend references = @field.grid:SDF_FORMAT_ARGS:rows=3@</Grid>) {}
def "Plain" (prepend references = @field.grid@</Grid>) {}
def "Two" (prepend references = @field.grid:SDF_FORMAT_ARGS:target=usd&rows=2@</Grid>) {}
def "Seeded" (prepend references = @gen.noise:SDF_FORMAT_ARGS:seed=7@</Noise>) {}
"#;

fn open(threads: usize) -> StageDocument<Memory> {
    StageDocument::open_with(
        scene(ROOT),
        "root.usda",
        StageOptions {
            threads,
            ..Default::default()
        },
        LoadOptions {
            formats: formats(),
            ..Default::default()
        },
    )
    .unwrap()
}

/// What the composed stage reads under each referencing prim.
fn summary(doc: &StageDocument<Memory>) -> Vec<String> {
    let view = doc.view();
    let mut out = Vec::new();
    for root in ["/Three", "/Plain", "/Two"] {
        let prim = view.prim(root).unwrap();
        let rows: Vec<String> = prim
            .children()
            .map(|row| {
                let height = row.attribute("height").unwrap().value::<f64>(Time::Default);
                format!(
                    "{}={height:?}",
                    doc.store().paths.display(row.path(), &doc.store().tokens)
                )
            })
            .collect();
        let label = prim
            .attribute("label")
            .unwrap()
            .value::<String>(Time::Default);
        let note = prim
            .attribute("note")
            .unwrap()
            .value::<String>(Time::Default);
        out.push(format!("{root} {label:?} {note:?} {rows:?}"));
    }
    let bolts = view
        .attribute("/Three/Row0.bolts")
        .and_then(|a| a.value::<i32>(Time::Default));
    let seed = view
        .attribute("/Seeded.seed")
        .and_then(|a| a.value::<i32>(Time::Default));
    out.push(format!("bolts {bolts:?} seed {seed:?}"));
    out
}

#[test]
fn format_arguments_name_distinct_generated_layers() {
    let doc = open(1);
    assert_eq!(
        summary(&doc),
        [
            r#"/Three Some("north") Some("base") ["/Three/Row0=Some(0.0)", "/Three/Row1=Some(1.0)", "/Three/Row2=Some(2.0)"]"#,
            r#"/Plain Some("north") Some("base") ["/Plain/Row0=Some(0.0)", "/Plain/Row1=Some(1.0)"]"#,
            r#"/Two Some("north") Some("base") ["/Two/Row0=Some(0.0)", "/Two/Row1=Some(1.0)"]"#,
            "bolts Some(4) seed Some(7)",
        ]
    );
    // `rows=2` is the default and `target` names no layer, so `/Plain` and
    // `/Two` share one: two grid layers, one noise layer.
    let mut identifiers: Vec<&str> = doc
        .load_report()
        .layers
        .iter()
        .filter_map(|&id| doc.identifier(id))
        .collect();
    identifiers.sort_unstable();
    assert_eq!(
        identifiers,
        [
            "base.usda",
            "field.grid",
            "field.grid:SDF_FORMAT_ARGS:rows=3",
            "gen.noise:SDF_FORMAT_ARGS:seed=7",
            "part.usda",
            "root.usda",
        ]
    );
    // The grid's source is read once per layer; the noise has no source.
    let reads: Vec<String> = doc.storage().reads.iter().collect();
    assert_eq!(reads.iter().filter(|r| *r == "field.grid").count(), 2);
    assert!(!reads.iter().any(|r| r.starts_with("gen.noise")));
}

#[test]
fn threads_read_generated_layers_as_one_does() {
    let (one, four) = (open(1), open(4));
    assert_eq!(summary(&one), summary(&four));
    let identifiers = |doc: &StageDocument<Memory>| -> Vec<(LayerId, String)> {
        doc.load_report()
            .layers
            .iter()
            .map(|&id| (id, doc.identifier(id).unwrap().into()))
            .collect()
    };
    assert_eq!(identifiers(&one), identifiers(&four));
}

#[test]
fn generated_layers_save_only_as_copies() {
    let mut doc = open(1);
    let grid = doc
        .load_report()
        .layers
        .iter()
        .copied()
        .find(|&id| doc.identifier(id) == Some("field.grid:SDF_FORMAT_ARGS:rows=3"))
        .unwrap();
    let plain = doc
        .load_report()
        .layers
        .iter()
        .copied()
        .find(|&id| doc.identifier(id) == Some("field.grid"))
        .unwrap();
    let row = doc.store_mut().path("/Grid/Row1");
    for layer in [grid, plain] {
        let layer = doc.store_mut().layer_mut(layer).unwrap();
        let spec = layer.prim(row).unwrap().clone();
        layer.insert_prim(row, spec.with_active(false));
    }
    doc.synchronize();
    let report = doc.save_layers(&[grid, plain]);
    let mut failures: Vec<(LayerId, IoErrorKind)> = report
        .failures
        .iter()
        .map(|f| (f.layer, f.error.kind))
        .collect();
    failures.sort_by_key(|(layer, _)| *layer);
    // With arguments, a layer is built from them; without, the grid
    // format writes nothing.
    let mut expected = vec![
        (plain, IoErrorKind::Unsupported),
        (grid, IoErrorKind::ReadOnly),
    ];
    expected.sort_by_key(|(layer, _)| *layer);
    assert_eq!(failures, expected);
    assert!(doc.storage().writes.is_empty());
    doc.export_layer(grid, "copy.usda").unwrap();
    let text = String::from_utf8(doc.storage().files["copy.usda"].clone()).unwrap();
    assert!(text.contains("def \"Row2\""), "{text}");
    assert_eq!(
        doc.export_layer(grid, "copy.grid").unwrap_err().kind,
        IoErrorKind::Unsupported
    );
}

#[test]
fn format_failures_reject_the_layer_with_its_identifier() {
    let open = |root: &str| {
        StageDocument::open_with(
            scene(root),
            "root.usda",
            StageOptions::default(),
            LoadOptions {
                formats: formats(),
                ..Default::default()
            },
        )
    };
    let error = open(
        "#usda 1.0\ndef \"A\" (prepend references = @field.grid:SDF_FORMAT_ARGS:rows=many@</Grid>) {}\n",
    )
    .unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Rejected);
    assert_eq!(
        error.to_string(),
        "field.grid:SDF_FORMAT_ARGS:rows=many: `rows` must be a count"
    );
    let error =
        open("#usda 1.0\ndef \"A\" (prepend references = @gen.noise@</Noise>) {}\n").unwrap_err();
    assert_eq!(error.to_string(), "gen.noise: noise needs a `seed`");
    let error = open(
        "#usda 1.0\ndef \"A\" (prepend references = @gen.noise:SDF_FORMAT_ARGS:seed@</Noise>) {}\n",
    )
    .unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Rejected);
    // Without the format registered, the extension is read as USDA, which
    // the grid's label is not.
    let error = StageDocument::open(
        scene("#usda 1.0\ndef \"A\" (prepend references = @field.grid@</Grid>) {}\n"),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Rejected);
}

#[test]
fn reloading_reads_generated_layers_again() {
    let mut doc = open(1);
    doc.storage_mut()
        .files
        .insert("field.grid".into(), b"south\n".to_vec());
    doc.reload(ReloadPolicy::PreserveDirty).unwrap();
    let label = |root: &str| {
        doc.view()
            .attribute(&format!("{root}.label"))
            .and_then(|a| a.value::<String>(Time::Default))
    };
    assert_eq!(label("/Three").as_deref(), Some("south"));
    assert_eq!(label("/Plain").as_deref(), Some("south"));
}
