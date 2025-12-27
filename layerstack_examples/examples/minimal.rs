//! Minimal end-to-end composition example.

use layerstack::{
    FieldValue, HashMap, InMemoryStore, Layer, LayerId, Path, PrimSpec, Reference, Stage,
    StageOptions, Value,
};

fn main() {
    let mut store = InMemoryStore::default();

    let field_title = store.tokens.intern("title");

    let p = store
        .paths
        .intern(Path::parse_absolute("/Doc", &mut store.tokens).expect("valid path"));

    let q = store
        .paths
        .intern(Path::parse_absolute("/Template", &mut store.tokens).expect("valid path"));

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![],
        prims: HashMap::new(),
    };

    let mut doc_spec = PrimSpec::default();
    doc_spec.references.append.push(Reference {
        layer: LayerId(2),
        prim_path: q,
        asset: Some("template".to_string()),
    });
    doc_spec.fields.insert(
        field_title,
        FieldValue::Value(Value::String("Hello from local".into())),
    );
    root_layer.prims.insert(p, doc_spec);
    store.insert_layer(root_layer);

    let mut template_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut template_spec = PrimSpec::default();
    template_spec.fields.insert(
        field_title,
        FieldValue::Value(Value::String("Hello from reference".into())),
    );
    template_layer.prims.insert(q, template_spec);
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
