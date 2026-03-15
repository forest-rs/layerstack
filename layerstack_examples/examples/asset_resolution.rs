//! Asset resolution: loading referenced layers on demand.
//!
//! In a real pipeline, layers live in files, databases, or on the network.
//! References carry an `asset` URI that must be resolved to a `LayerId`
//! before composition. This example shows the pattern: a custom
//! `LayerStore` that resolves asset URIs lazily and loads layers into an
//! in-memory cache on first access.
//!
//! Scene structure:
//!
//! ```text
//! /Stage
//!   /Robot        (references asset "props/robot.layer")
//!     /Arm        (defined in the referenced layer)
//!   /Environment  (references asset "sets/env.layer")
//!     /Ground     (defined in the referenced layer)
//! ```

use std::collections::HashMap as StdHashMap;

use layerstack::{
    FieldValue, HashMap, Layer, LayerId, ListOp, Path, PathId, PrimSpec, Reference, Specifier,
    Stage, StageOptions, TokenInterner, Value, path::PathInterner,
};

use layerstack::doc::LayerStore;

// ---------------------------------------------------------------------------
// Asset resolver: maps URI strings to layer data.
// ---------------------------------------------------------------------------

/// A simple asset catalog mapping URI strings to layer-building functions.
///
/// In production this would be a file loader, database client, or HTTP
/// fetcher. Here we use closures that build layers programmatically.
struct AssetCatalog {
    /// URI to builder function.
    #[allow(clippy::type_complexity, reason = "example code, clarity over abstraction")]
    builders: StdHashMap<String, Box<dyn Fn(&mut TokenInterner, &mut PathInterner) -> Layer>>,
}

impl AssetCatalog {
    fn new() -> Self {
        Self {
            builders: StdHashMap::new(),
        }
    }

    fn register(
        &mut self,
        uri: &str,
        builder: impl Fn(&mut TokenInterner, &mut PathInterner) -> Layer + 'static,
    ) {
        self.builders.insert(uri.to_string(), Box::new(builder));
    }
}

// ---------------------------------------------------------------------------
// Resolving LayerStore: resolves asset URIs and caches loaded layers.
// ---------------------------------------------------------------------------

/// A `LayerStore` that lazily loads layers when they're first accessed.
///
/// This demonstrates the pattern for integrating external asset sources
/// with layerstack's composition. The key insight: all asset resolution
/// happens *before* or *during* store population — `Stage::compose` only
/// sees `LayerId`s and `PathId`s.
struct ResolvingStore {
    tokens: TokenInterner,
    paths: PathInterner,
    layers: HashMap<LayerId, Layer>,
    /// Maps asset URI to assigned `LayerId` for deduplication.
    resolved: StdHashMap<String, LayerId>,
    next_id: u64,
}

impl ResolvingStore {
    fn new() -> Self {
        Self {
            tokens: TokenInterner::default(),
            paths: PathInterner::default(),
            layers: HashMap::new(),
            resolved: StdHashMap::new(),
            next_id: 100, // Reserve low IDs for hand-authored layers.
        }
    }

    /// Resolves an asset URI to a `LayerId`, loading it from the catalog
    /// if not already cached.
    fn resolve(&mut self, uri: &str, catalog: &AssetCatalog) -> LayerId {
        if let Some(&id) = self.resolved.get(uri) {
            return id;
        }

        let id = LayerId(self.next_id);
        self.next_id += 1;

        if let Some(builder) = catalog.builders.get(uri) {
            let mut layer = builder(&mut self.tokens, &mut self.paths);
            layer.id = id;
            self.layers.insert(id, layer);
        } else {
            eprintln!("warning: unknown asset URI '{uri}', inserting empty layer");
            self.layers.insert(
                id,
                Layer {
                    id,
                    sublayers: vec![],
                    prims: HashMap::new(),
                },
            );
        }

        self.resolved.insert(uri.to_string(), id);
        id
    }

    fn insert_layer(&mut self, layer: Layer) {
        self.layers.insert(layer.id, layer);
    }
}

impl LayerStore for ResolvingStore {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.get(&id)
    }
    fn tokens(&self) -> &TokenInterner {
        &self.tokens
    }
    fn tokens_mut(&mut self) -> &mut TokenInterner {
        &mut self.tokens
    }
    fn paths(&self) -> &PathInterner {
        &self.paths
    }
    fn paths_mut(&mut self) -> &mut PathInterner {
        &mut self.paths
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn path(store: &mut ResolvingStore, s: &str) -> PathId {
    let p = Path::parse_absolute(s, &mut store.tokens).expect("valid path");
    store.paths.intern(p)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    // 1. Set up an asset catalog with two referenced layers.
    let mut catalog = AssetCatalog::new();

    catalog.register("props/robot.layer", |tokens, paths| {
        let field_material = tokens.intern("material");
        let field_joints = tokens.intern("joints");
        let arm_path = paths.intern(Path::parse_absolute("/Arm", tokens).expect("valid path"));

        let mut arm_spec = PrimSpec {
            specifier: Some(Specifier::Def),
            ..PrimSpec::default()
        };
        arm_spec.fields.insert(
            field_material,
            FieldValue::Value(Value::String("titanium".into())),
        );
        arm_spec
            .fields
            .insert(field_joints, FieldValue::Value(Value::Int(6)));

        let mut prims = HashMap::new();
        prims.insert(arm_path, arm_spec);

        Layer {
            id: LayerId(0), // Will be overwritten by resolve().
            sublayers: vec![],
            prims,
        }
    });

    catalog.register("sets/env.layer", |tokens, paths| {
        let field_color = tokens.intern("color");
        let ground_path =
            paths.intern(Path::parse_absolute("/Ground", tokens).expect("valid path"));

        let mut ground_spec = PrimSpec {
            specifier: Some(Specifier::Def),
            ..PrimSpec::default()
        };
        ground_spec.fields.insert(
            field_color,
            FieldValue::Value(Value::String("brown".into())),
        );

        let mut prims = HashMap::new();
        prims.insert(ground_path, ground_spec);

        Layer {
            id: LayerId(0),
            sublayers: vec![],
            prims,
        }
    });

    // 2. Build the root scene layer, resolving asset URIs as we go.
    let mut store = ResolvingStore::new();

    let robot_path = path(&mut store, "/Robot");
    let env_path = path(&mut store, "/Environment");

    // Resolve assets to get concrete LayerIds.
    let robot_layer_id = store.resolve("props/robot.layer", &catalog);
    let env_layer_id = store.resolve("sets/env.layer", &catalog);

    // The referenced prim paths (what the reference points *at* inside the
    // asset layer).
    let ref_arm = path(&mut store, "/Arm");
    let ref_ground = path(&mut store, "/Ground");

    let mut root = Layer {
        id: LayerId(1),
        sublayers: vec![],
        prims: HashMap::new(),
    };

    // /Robot references /Arm from the robot asset.
    let mut robot_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        ..PrimSpec::default()
    };
    robot_spec.references = ListOp {
        append: vec![Reference {
            layer: robot_layer_id,
            prim_path: ref_arm,
            asset: Some("props/robot.layer".to_string()),
        }],
        ..ListOp::default()
    };
    root.prims.insert(robot_path, robot_spec);

    // /Environment references /Ground from the environment asset.
    let mut env_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        ..PrimSpec::default()
    };
    env_spec.references = ListOp {
        append: vec![Reference {
            layer: env_layer_id,
            prim_path: ref_ground,
            asset: Some("sets/env.layer".to_string()),
        }],
        ..ListOp::default()
    };
    root.prims.insert(env_path, env_spec);

    store.insert_layer(root);

    // 3. Compose and query.
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_provenance: true,
            with_dependencies: true,
            ..StageOptions::default()
        },
    );

    let field_material = store.tokens.intern("material");
    let field_joints = store.tokens.intern("joints");
    let field_color = store.tokens.intern("color");

    // Robot gets its fields from the referenced asset layer.
    let material = stage.resolve_field(robot_path, field_material).unwrap();
    let joints = stage.resolve_field(robot_path, field_joints).unwrap();
    println!("/Robot");
    println!("  material = {:?}", material.value);
    println!("  joints   = {:?}", joints.value);
    if let Some(prov) = material.provenance {
        println!("  (material provided by layer {})", prov.layer.0);
    }

    // Environment gets its fields from the env asset.
    let color = stage.resolve_field(env_path, field_color).unwrap();
    println!("/Environment");
    println!("  color = {:?}", color.value);

    // Show dependency tracking: which layers affect /Robot?
    let affecting = stage.layers_affecting_prim(robot_path);
    println!(
        "\nLayers affecting /Robot: {:?}",
        affecting.iter().map(|l| l.0).collect::<Vec<_>>()
    );

    // Show arc dependencies.
    let arcs = stage.arcs_targeting(robot_path);
    for arc in &arcs {
        println!("  arc: {:?} from layer {}", arc.arc_kind, arc.layer.0);
    }
}
