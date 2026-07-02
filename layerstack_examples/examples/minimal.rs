// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal end-to-end composition example.

use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Reference, Stage, StageOptions, Value};

fn main() {
    let mut store = InMemoryStore::default();

    let field_title = store.tokens.intern("title");

    let p = store.path("/Doc");
    let q = store.path("/Template");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.insert_prim(
        p,
        PrimSpec::default()
            .with_reference(Reference::with_asset(LayerId(2), q, "template"))
            .with_field(field_title, Value::string("Hello from local")),
    );
    store.insert_layer(root_layer);

    let mut template_layer = Layer::new(LayerId(2));
    template_layer.insert_prim(
        q,
        PrimSpec::default().with_field(field_title, Value::string("Hello from reference")),
    );
    store.insert_layer(template_layer);

    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );

    let resolved = stage.resolve_field(p, field_title).expect("title exists");
    let Value::String(title) = resolved.value else {
        panic!("unexpected resolved type");
    };

    println!("Resolved title: {title}");

    let prov = resolved.provenance.expect("provenance enabled");
    println!("Winning layer id: {}", prov.layer.0);
}
