// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

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
    AssetResolveError, AssetResolver, LayerOffset, ResolvedAsset, Stage, StageOptions,
    SublayerEntry, Time, Value,
};
use layerstack_usda::read_usda;

/// Stub asset resolver for inline USDA examples with no external assets.
struct StubResolver {
    next_id: u64,
}

impl AssetResolver for StubResolver {
    fn resolve(
        &mut self,
        _asset_path: &str,
        _anchor: Option<LayerId>,
        _tokens: &TokenInterner,
        _paths: &PathInterner,
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
    let mut resolver = StubResolver { next_id: 10_000 };
    let read = read_usda(
        source,
        layer_id,
        store.tokens(),
        store.paths(),
        &mut resolver,
    );
    let diagnostics: Vec<_> = read.diagnostics().collect();
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
    let emitted = read.emitted;

    store.insert_layer(emitted.layer);
    for layer in emitted.resolved_layers {
        store.insert_layer(layer);
    }
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
    int[] points = edit [
        write 9 to [0]
        append 4
        erase [-2]
    ]
}
"#;

    let time_sampled_sparse_override = r#"#usda 1.0
over "Mesh" {
    int[] animated.timeSamples = {
        0: edit [],
        2: edit [write 8 to [1]],
        3: edit []
    }
}
"#;

    insert_inline_usda(&mut store, LayerId(1), base);
    insert_inline_usda(&mut store, LayerId(2), sparse_override);
    insert_inline_usda(&mut store, LayerId(3), time_sampled_sparse_override);

    let mut root = Layer::new(LayerId(4));
    *root.sublayers_mut() = vec![
        SublayerEntry::new(LayerId(3)),
        SublayerEntry::new(LayerId(2)),
        SublayerEntry {
            layer: LayerId(1),
            offset: LayerOffset::IDENTITY,
            asset: None,
        },
    ];
    store.insert_layer(root);

    let stage = Stage::compose(&mut store, LayerId(4), StageOptions::default());

    // `points` and `animated` are attributes: read them through their
    // handles, at the default time or at a time code.
    let view = stage.view(&store);
    let points = view.attribute("/Mesh.points").expect("points attribute");
    let animated = view
        .attribute("/Mesh.animated")
        .expect("animated attribute");

    let resolved_points = points.get(Time::Default).expect("points value");
    println!("Sparse override over dense array:");
    println!("  resolved points = {}", resolved_points.value);
    assert_eq!(
        resolved_points.value,
        Value::from(vec![9_i32, 2, 4]),
        "sparse default edit should patch the dense base array in strength order"
    );

    let animated_mid = animated
        .get(Time::held(2.5))
        .expect("held sparse sample at t=2.5");
    let animated_reset = animated
        .get(Time::held(3.5))
        .expect("held sparse sample at t=3.5");

    println!("Held time-sampled sparse edits over the same dense base:");
    println!("  t=2.5 -> {}", animated_mid.value);
    println!("  t=3.5 -> {}", animated_reset.value);

    assert_eq!(
        animated_mid.value,
        Value::from(vec![1_i32, 8, 3]),
        "held sample at t=2.5 should use the edit authored at sample time 2"
    );
    assert_eq!(
        animated_reset.value,
        Value::from(vec![1_i32, 2, 3]),
        "held sample at t=3.5 should reset to the dense base via the identity edit"
    );
}
