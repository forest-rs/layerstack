//! Sparse array edit composition from authored USDA.
//!
//! ```sh
//! cargo run -p layerstack_examples --example sparse_array_edits
//! ```

use std::sync::Arc;

use layerstack::doc::{InMemoryStore, Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{
    AssetResolveError, AssetResolver, InterpolationType, LayerOffset, ResolvedAsset, Stage,
    StageOptions, SublayerEntry, Value,
};
use layerstack_usda::emit;
use layerstack_usda::lower;
use layerstack_usda::parser::parse_cst;

/// Stub asset resolver for inline USDA examples with no external assets.
struct StubResolver {
    next_id: u64,
}

impl AssetResolver for StubResolver {
    fn resolve(
        &mut self,
        _asset_path: &str,
        _anchor: Option<LayerId>,
        _tokens: &mut TokenInterner,
        _paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        Ok(ResolvedAsset {
            layer_id: id,
            resolved_path: Arc::from("stub"),
            layer: Some(Layer::new(id)),
        })
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

fn insert_inline_usda(store: &mut InMemoryStore, layer_id: LayerId, source: &str) {
    let cst = parse_cst(source);
    assert!(
        cst.diagnostics.is_empty(),
        "unexpected CST diagnostics: {:?}",
        cst.diagnostics
    );

    let ast = lower::lower(&cst.tree, source);
    assert!(
        ast.diagnostics.is_empty(),
        "unexpected lowering diagnostics: {:?}",
        ast.diagnostics
    );

    let mut resolver = StubResolver { next_id: 10_000 };
    let emitted = emit::emit(
        &ast.layer,
        layer_id,
        &mut store.tokens,
        &mut store.paths,
        &mut resolver,
    );
    assert!(
        emitted.diagnostics.is_empty(),
        "unexpected emit diagnostics: {:?}",
        emitted.diagnostics
    );

    store.insert_layer(emitted.layer);
    for layer in emitted.resolved_layers {
        store.insert_layer(layer);
    }
}

fn expect_array(value: Value) -> Vec<Value> {
    let Value::Array(items) = value else {
        panic!("expected array value");
    };
    items
}

fn main() {
    let mut store = InMemoryStore::default();

    let base = r#"#usda 1.0
def "Mesh" {
    int[] points = [1, 2, 3]
    int[] animated = [1, 2, 3]
}
"#;

    let sparse_override = r#"#usda 1.0
over "Mesh" {
    int[] points = edit (
        write 9 to [0],
        append 4,
        erase [-2]
    )
}
"#;

    let time_sampled_sparse_override = r#"#usda 1.0
over "Mesh" {
    int[] animated.timeSamples = {
        0: edit (),
        2: edit (write 8 to [1]),
        3: edit ()
    }
}
"#;

    insert_inline_usda(&mut store, LayerId(1), base);
    insert_inline_usda(&mut store, LayerId(2), sparse_override);
    insert_inline_usda(&mut store, LayerId(3), time_sampled_sparse_override);

    let mut root = Layer::new(LayerId(4));
    root.sublayers = vec![
        SublayerEntry::new(LayerId(3)),
        SublayerEntry::new(LayerId(2)),
        SublayerEntry {
            layer: LayerId(1),
            offset: LayerOffset::IDENTITY,
        },
    ];
    store.insert_layer(root);

    let stage = Stage::compose(&mut store, LayerId(4), StageOptions::default());

    let mesh = store.path("/Mesh");
    let points = store.tokens.intern("points");
    let animated = store.tokens.intern("animated");

    let resolved_points = stage.resolve_field(mesh, points).expect("points field");
    println!("Sparse override over dense array:");
    println!("  resolved points = {}", resolved_points.value);
    assert_eq!(
        expect_array(resolved_points.value),
        vec![Value::Int(9), Value::Int(2), Value::Int(4)],
        "sparse default edit should patch the dense base array in strength order"
    );

    let animated_mid = stage
        .resolve_value_at_time(mesh, animated, 2.5, InterpolationType::Held)
        .expect("held sparse sample at t=2.5");
    let animated_reset = stage
        .resolve_value_at_time(mesh, animated, 3.5, InterpolationType::Held)
        .expect("held sparse sample at t=3.5");

    println!("Held time-sampled sparse edits over the same dense base:");
    println!("  t=2.5 -> {}", animated_mid.value);
    println!("  t=3.5 -> {}", animated_reset.value);

    assert_eq!(
        expect_array(animated_mid.value),
        vec![Value::Int(1), Value::Int(8), Value::Int(3)],
        "held sample at t=2.5 should use the edit authored at sample time 2"
    );
    assert_eq!(
        expect_array(animated_reset.value),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)],
        "held sample at t=3.5 should reset to the dense base via the identity edit"
    );
}
