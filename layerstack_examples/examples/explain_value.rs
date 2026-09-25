// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explaining why attributes and metadata have their values.
//!
//! A tree asset is referenced into a grove. The grove overrides the tree's
//! height, and a sublayer of edits moves the top of its trunk with a sparse
//! array edit. `Stage::explain_*` names every opinion each value consulted:
//! its layer, spec, arc and layer offset, and whether it contributed, was
//! shadowed, or was cut off.
//!
//! ```sh
//! cargo run -p layerstack_examples --example explain_value
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::doc::{InMemoryStore, Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{
    AssetResolveError, AssetResolver, Contribution, IgnoreCause, InterpolationType, OpinionRole,
    ResolvedAsset, ResolvedValue, Stage, StageOptions, Value, ValueExplanation, ValueSource,
};
use layerstack_usda::{emit, lower, parser::parse_cst};

const GROVE: &str = r#"#usda 1.0
(
    subLayers = [@grove_edits.usda@]
)

def Xform "Grove"
{
    def Xform "OldOak" (
        references = @tree.usda@ (offset = 10)
        customData = {
            dictionary growth = {
                double max = 9
            }
        }
    )
    {
        double height = 7
    }
}
"#;

const GROVE_EDITS: &str = r#"#usda 1.0

over "Grove"
{
    over "OldOak"
    {
        float3[] points = edit [
            write (0, 2.5, 0.5) to [-1]
            append (0, 3, 1)
        ]
    }
}
"#;

const TREE: &str = r#"#usda 1.0
(
    defaultPrim = "Tree"
)

class "_class_Tree"
{
    string bark = "rough"
}

def Xform "Tree" (
    inherits = </_class_Tree>
    customData = {
        string species = "oak"
        dictionary growth = {
            double rate = 0.5
            double max = 4
        }
    }
)
{
    double height = 3
    float3[] points = [(0, 0, 0), (0, 1, 0), (0, 2, 0)]
    double sway.timeSamples = {
        0: 0,
        24: 1,
    }
}
"#;

/// Resolves asset paths to the inline layers and remembers their names.
struct InlineResolver {
    sources: BTreeMap<&'static str, &'static str>,
    ids: BTreeMap<String, LayerId>,
    names: BTreeMap<LayerId, String>,
    pending: Vec<Layer>,
}

impl InlineResolver {
    /// Parses and emits the layer `name` as `id`, queueing the layers its
    /// arcs resolve.
    fn emit(
        &mut self,
        name: &str,
        id: LayerId,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Option<Layer> {
        let source = *self.sources.get(name)?;
        self.ids.insert(name.to_string(), id);
        self.names.insert(id, name.to_string());
        let cst = parse_cst(source);
        assert!(cst.diagnostics.is_empty(), "{:?}", cst.diagnostics);
        let ast = lower::lower(&cst.tree, source);
        assert!(ast.diagnostics.is_empty(), "{:?}", ast.diagnostics);
        let emitted = emit::emit(&ast.layer, id, tokens, paths, self);
        assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
        self.pending.extend(emitted.resolved_layers);
        Some(emitted.layer)
    }
}

impl AssetResolver for InlineResolver {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        if let Some(&layer_id) = self.ids.get(asset_path) {
            return Ok(ResolvedAsset {
                layer_id,
                resolved_path: Arc::from(asset_path),
                layer: None,
            });
        }
        let layer_id = LayerId(self.ids.len() as u64 + 1);
        let layer = self
            .emit(asset_path, layer_id, tokens, paths)
            .ok_or(AssetResolveError::NotFound)?;
        Ok(ResolvedAsset {
            layer_id,
            resolved_path: Arc::from(asset_path),
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.names.get(&id).map(String::as_str)
    }
}

/// Prints one explanation, naming layers through the resolver.
fn print<T: std::fmt::Debug>(
    title: &str,
    explained: &ValueExplanation<'_, T>,
    store: &InMemoryStore,
    resolver: &InlineResolver,
) {
    println!("{title}");
    println!("  value  = {:?}", explained.value);
    println!("  source = {:?}", explained.source);
    for opinion in &explained.opinions {
        let layer = resolver.resolved_path(opinion.layer()).unwrap_or("?");
        let arc = opinion
            .node
            .map(|node| {
                format!(
                    "{:?} {}",
                    node.arc_kind(),
                    node.site().display(&store.tokens)
                )
            })
            .unwrap_or_default();
        let offset = opinion.layer_offset();
        let role = match &opinion.role {
            OpinionRole::Contributed(Contribution::Dictionary(merge)) => {
                let keys = |paths: &[Vec<Arc<str>>]| {
                    paths
                        .iter()
                        .map(|path| path.join(":"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!(
                    "dictionary: supplied [{}], merged [{}], overridden [{}]",
                    keys(&merge.supplied),
                    keys(&merge.merged),
                    keys(&merge.overridden)
                )
            }
            role => format!("{role:?}"),
        };
        println!(
            "  - {layer} {} via {arc} (offset {}, scale {}): {role}",
            opinion.spec_path().display(&store.tokens),
            offset.offset,
            offset.scale,
        );
        for sample in &opinion.samples {
            println!(
                "      sample at stage time {}: {:?}",
                sample.time, sample.role
            );
        }
    }
    println!();
}

fn main() {
    let mut store = InMemoryStore::default();
    let mut resolver = InlineResolver {
        sources: BTreeMap::from([
            ("grove.usda", GROVE),
            ("grove_edits.usda", GROVE_EDITS),
            ("tree.usda", TREE),
        ]),
        ids: BTreeMap::new(),
        names: BTreeMap::new(),
        pending: Vec::new(),
    };
    let root = LayerId(1);
    let layer = resolver
        .emit("grove.usda", root, &mut store.tokens, &mut store.paths)
        .expect("root layer");
    store.insert_layer(layer);
    for layer in resolver.pending.drain(..) {
        store.insert_layer(layer);
    }
    let stage = Stage::compose(&mut store, root, StageOptions::default());

    // The grove's local height shadows the referenced tree's.
    let height = store.property_path("/Grove/OldOak.height");
    let explained = stage.explain_property_value(height).expect("height");
    print("height", &explained, &store, &resolver);
    assert_eq!(
        explained.value,
        Some(ResolvedValue::Scalar(Value::Double(7.0))),
        "the grove's local height wins"
    );
    assert_eq!(
        explained.opinions[1].role,
        OpinionRole::Ignored(IgnoreCause::Shadowed),
        "the referenced height is shadowed"
    );

    // The sparse edit from the edits sublayer composes over the asset's
    // dense points, reached through the reference.
    let points = store.property_path("/Grove/OldOak.points");
    let explained = stage.explain_property_value(points).expect("points");
    print("points", &explained, &store, &resolver);
    let roles: Vec<_> = explained.opinions.iter().map(|o| o.role.clone()).collect();
    assert_eq!(
        roles,
        vec![
            OpinionRole::Contributed(Contribution::ArrayEdit),
            OpinionRole::Contributed(Contribution::Value),
        ],
        "the sparse edit composes over the referenced dense points"
    );
    assert_eq!(
        explained.value,
        Some(ResolvedValue::Scalar(Value::Array(vec![
            Value::Vec3f([0.0, 0.0, 0.0]),
            Value::Vec3f([0.0, 1.0, 0.0]),
            Value::Vec3f([0.0, 2.5, 0.5]),
            Value::Vec3f([0.0, 3.0, 1.0]),
        ]))),
        "the edit moves the last point and appends one"
    );

    // The reference offsets the tree's samples by 10 frames.
    let sway = store.property_path("/Grove/OldOak.sway");
    let explained = stage
        .explain_property_value_at_time(sway, 22.0, InterpolationType::Linear)
        .expect("sway");
    print("sway at 22", &explained, &store, &resolver);
    assert_eq!(
        explained.value,
        Some(Value::Double(0.5)),
        "the reference offsets the tree's samples by 10 frames"
    );
    assert_eq!(
        explained.source,
        ValueSource::TimeSamples {
            lower: 10.0,
            upper: 34.0,
            interpolation: InterpolationType::Linear,
            lower_seeded_by_fallback: false,
            upper_seeded_by_fallback: false,
        },
        "the bracketing samples sit at stage times 10 and 34"
    );

    // `customData` combines key by key across the grove and the asset.
    let oak = store.path("/Grove/OldOak");
    let custom_data = store.tokens.intern("customData");
    let explained = stage.explain_value(oak, custom_data).expect("customData");
    print("customData", &explained, &store, &resolver);
    assert_eq!(
        explained.contributors().count(),
        2,
        "the grove and the asset both contribute entries"
    );

    // The bark comes from the class the tree inherits.
    let bark = store.property_path("/Grove/OldOak.bark");
    let explained = stage.explain_property_value(bark).expect("bark");
    print("bark", &explained, &store, &resolver);
    assert_eq!(
        explained.opinions[0].arc_kind(),
        Some(layerstack::ArcKind::Inherits),
        "the bark comes from the inherited class"
    );
}
