// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Flattening: a grove of referenced trees written out as one file.
//!
//! A tree asset is referenced into a grove three times, once retimed and
//! once with a local override, and a sublayer adds a season's opinions.
//! [`Stage::flatten`] turns the composed grove into a single layer with no
//! composition arcs, which is saved as USDA (`usdcat --flatten` in
//! OpenUSD), and reports how: what it wrote exactly, what it transformed
//! (the retimed samples, list ops made explicit) and what it lost (nothing,
//! or the default requirements would have refused). Reading the file back
//! on its own composes the same grove, which [`Stage::verify_flattened`]
//! checks.
//!
//! ```text
//! grove.usda                 tree.usda
//!   subLayers: season.usda     /Tree
//!   /Grove                       .height = 3
//!     /Oak  -> tree.usda         .sway (samples at 0 and 10)
//!     /Elm  -> tree.usda           /Canopy
//!              (offset = 5)          rel light -> /Tree/Canopy
//!     /Ash  -> tree.usda
//!       .height = 6
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use layerstack::stage::flatten::{FindingKind, FlattenRequirements, Instancing, Transformation};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, Layer, LayerId, ListOp, PathInterner,
    ResolvedAsset, Stage, StageOptions, StageView, Time, TokenInterner, Value,
};

const GROVE: &str = r#"#usda 1.0
(
    defaultPrim = "Grove"
    upAxis = "Y"
    metersPerUnit = 1
    startTimeCode = 0
    endTimeCode = 20
    subLayers = [@./season.usda@]
)

def Xform "Grove"
{
    def "Oak" (
        references = @./tree.usda@
    )
    {
    }

    def "Elm" (
        references = @./tree.usda@ (offset = 5)
    )
    {
    }

    def "Ash" (
        references = @./tree.usda@
    )
    {
        double height = 6
    }
}
"#;

const SEASON: &str = r#"#usda 1.0

over "Grove"
{
    over "Oak"
    {
        token leafColor = "gold"
    }
}
"#;

const TREE: &str = r#"#usda 1.0
(
    defaultPrim = "Tree"
)

def Xform "Tree" (
    kind = "component"
)
{
    double height = 3
    token leafColor = "green"
    float sway.timeSamples = {
        0: 0,
        10: 1,
    }

    def Scope "Canopy"
    {
        rel light = </Tree/Canopy>
    }
}
"#;

/// Serves the three files above by name.
struct Files {
    files: HashMap<&'static str, &'static str>,
    loaded: HashMap<String, LayerId>,
    next: u64,
}

impl AssetResolver for Files {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let resolved_path: Arc<str> = Arc::from(asset_path);
        if let Some(&layer_id) = self.loaded.get(asset_path) {
            return Ok(ResolvedAsset {
                layer_id,
                resolved_path,
                layer: None,
            });
        }
        let text = *self
            .files
            .get(asset_path)
            .ok_or(AssetResolveError::NotFound)?;
        let layer_id = LayerId(self.next);
        self.next += 1;
        self.loaded.insert(asset_path.into(), layer_id);
        // These files name no further assets.
        let (layer, _) = parse(text, layer_id, tokens, paths, self);
        Ok(ResolvedAsset {
            layer_id,
            resolved_path,
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

/// Reads USDA `text` as layer `id`, with the layers its arcs resolve to.
fn parse(
    text: &str,
    id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
) -> (Layer, Vec<Layer>) {
    let parsed = layerstack_usda::parser::parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let emitted = layerstack_usda::emit::emit(&parsed.layer, id, tokens, paths, resolver);
    assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
    (emitted.layer, emitted.resolved_layers)
}

/// Reads USDA `text` into `store` as layer `id`, with the layers it names.
fn load(text: &str, id: LayerId, store: &mut InMemoryStore, files: &mut Files) {
    let (layer, resolved) = parse(text, id, store.tokens(), store.paths(), files);
    store.insert_layer(layer);
    for layer in resolved {
        store.insert_layer(layer);
    }
}

fn main() {
    // 1. Load the grove and everything it names.
    let mut store = InMemoryStore::default();
    let mut files = Files {
        files: HashMap::from([("./tree.usda", TREE), ("./season.usda", SEASON)]),
        loaded: HashMap::new(),
        next: 2,
    };
    let root = LayerId(1);
    load(GROVE, root, &mut store, &mut files);

    // 2. Compose and flatten, requiring exact instancing and animation (the
    // defaults, spelled out) and refusing any loss.
    let stage = Stage::compose(&mut store, root, StageOptions::default());
    let flat_id = LayerId(100);
    let requirements = FlattenRequirements {
        instancing: Instancing::Preserve,
        exact_animation: true,
        ..FlattenRequirements::default()
    };
    let flattened = match stage.flatten(&mut store, root, flat_id, &requirements) {
        Ok(flattened) => flattened,
        Err(e) => panic!("the grove flattens: {e}"),
    };
    let report = &flattened.report;
    println!("exact: {:?}", report.preserved);
    for finding in &report.findings {
        println!("{finding}");
    }
    assert!(
        report.is_lossless(),
        "the default requirements refuse any loss"
    );
    let retimed: Vec<String> = report
        .transformed()
        .filter(|f| {
            matches!(
                f.kind,
                FindingKind::Transformed(Transformation::SamplesRetimed { .. })
            )
        })
        .map(|f| f.path.to_string())
        .collect();
    assert_eq!(retimed, ["/Grove/Elm.sway"], "only the Elm is retimed");
    let flat = &flattened.layer;
    assert!(flat.sublayers().is_empty(), "no sublayers remain");
    assert!(
        flat.prims()
            .values()
            .all(|spec| spec.references == ListOp::default()),
        "no references remain"
    );

    // 3. Save it as one USDA file.
    let text = layerstack_usda::save::save_usda(flat, store.tokens(), store.paths())
        .expect("the flattened grove saves");
    println!("{text}");
    assert!(!text.contains("references"), "the file names no asset");
    assert!(text.contains("upAxis = \"Y\""), "layer metadata is kept");

    // 4. The file composes the same grove on its own.
    let reread = LayerId(101);
    load(&text, reread, &mut store, &mut files);
    let reopened = Stage::compose(&mut store, reread, StageOptions::default());
    let verification = stage.verify_flattened(&reopened, &store, report, &[0.0, 5.0, 10.0, 15.0]);
    assert!(
        verification.is_equivalent(),
        "{:?}",
        verification.mismatches
    );
    println!("verified: {:?}", verification.scope);
    let flattened = reopened;
    let (original, flattened) = (stage.view(&store), flattened.view(&store));
    let value = |view: StageView<'_>, path: &str, time: Time| {
        view.attribute(path)
            .and_then(|attribute| attribute.get(time))
            .map(|resolved| resolved.value)
    };
    for tree in ["Oak", "Elm", "Ash"] {
        for name in ["height", "leafColor"] {
            let path = format!("/Grove/{tree}.{name}");
            assert_eq!(
                value(flattened, &path, Time::Default),
                value(original, &path, Time::Default),
                "{tree}.{name}"
            );
        }
        for time in [0.0, 5.0, 10.0, 15.0] {
            let path = format!("/Grove/{tree}.sway");
            assert_eq!(
                value(flattened, &path, Time::at(time)),
                value(original, &path, Time::at(time)),
                "{tree}.sway at {time}"
            );
        }
        let light = flattened
            .relationship(format!("/Grove/{tree}/Canopy.light").as_str())
            .expect("the canopy's light");
        assert_eq!(
            light.targets().len(),
            1,
            "{tree}'s canopy light is mapped into the grove"
        );
    }
    assert_eq!(
        value(flattened, "/Grove/Elm.sway", Time::at(15.0)),
        Some(Value::Float(1.0)),
        "the retimed Elm reaches full sway at stage time 15"
    );
    println!("The flattened grove composes the same values as the original.");
}
