//! Asset resolution: loading referenced layers on demand.
//!
//! In a real pipeline, layers live in files, databases, or on the network.
//! References carry an `asset` URI that must be resolved to a `LayerId`
//! before composition. This example shows the pattern: a catalog-backed
//! [`AssetResolver`] that loads layers lazily and deduplicates by URI.
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
use std::sync::Arc;

use layerstack::{
    AssetResolveError, AssetResolver, FieldValue, HashMap, InMemoryStore, Layer, LayerId, ListOp,
    Path, PathId, PrimSpec, Reference, ResolvedAsset, Specifier, Stage, StageOptions,
    TokenInterner, Value, path::PathInterner,
};

// ---------------------------------------------------------------------------
// Catalog-backed AssetResolver.
// ---------------------------------------------------------------------------

/// An [`AssetResolver`] backed by a catalog of builder functions.
///
/// In production this would be a file loader, database client, or HTTP
/// fetcher. Here we use closures that build layers programmatically.
struct CatalogResolver {
    /// URI to builder function.
    #[allow(
        clippy::type_complexity,
        reason = "example code, clarity over abstraction"
    )]
    builders: StdHashMap<String, Box<dyn Fn(&mut TokenInterner, &mut PathInterner) -> Layer>>,
    /// Maps asset URI to resolved info for deduplication.
    resolved: StdHashMap<String, (LayerId, Arc<str>)>,
    next_id: u64,
}

impl CatalogResolver {
    fn new() -> Self {
        Self {
            builders: StdHashMap::new(),
            resolved: StdHashMap::new(),
            next_id: 100, // Reserve low IDs for hand-authored layers.
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

impl AssetResolver for CatalogResolver {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        // Deduplication: return cached result if already resolved.
        if let Some((layer_id, resolved_path)) = self.resolved.get(asset_path) {
            return Ok(ResolvedAsset {
                layer_id: *layer_id,
                resolved_path: resolved_path.clone(),
                layer: None,
            });
        }

        let id = LayerId(self.next_id);
        self.next_id += 1;

        let builder = self
            .builders
            .get(asset_path)
            .ok_or(AssetResolveError::NotFound)?;

        let mut layer = builder(tokens, paths);
        layer.id = id;

        let resolved_path: Arc<str> = Arc::from(asset_path);
        self.resolved
            .insert(asset_path.to_string(), (id, resolved_path.clone()));

        Ok(ResolvedAsset {
            layer_id: id,
            resolved_path,
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.resolved
            .values()
            .find(|(lid, _)| *lid == id)
            .map(|(_, path)| &**path)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn path(store: &mut InMemoryStore, s: &str) -> PathId {
    let p = Path::parse_absolute(s, &mut store.tokens).expect("valid path");
    store.paths.intern(p)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    // 1. Set up a catalog-backed resolver with two asset layers.
    let mut resolver = CatalogResolver::new();

    resolver.register("props/robot.layer", |tokens, paths| {
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
            .insert(field_joints, FieldValue::Value(Value::Int64(6)));

        let mut prims = HashMap::new();
        prims.insert(arm_path, arm_spec);

        Layer {
            id: LayerId(0), // Overwritten by the resolver.
            sublayers: vec![],
            prims,
        }
    });

    resolver.register("sets/env.layer", |tokens, paths| {
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

    // 2. Build the root scene layer, resolving asset URIs via the trait.
    let mut store = InMemoryStore::default();

    let robot_path = path(&mut store, "/Robot");
    let env_path = path(&mut store, "/Environment");

    // Resolve assets through the AssetResolver trait.
    let robot_asset = resolver
        .resolve(
            "props/robot.layer",
            None,
            &mut store.tokens,
            &mut store.paths,
        )
        .expect("robot asset should resolve");
    let robot_layer_id = robot_asset.layer_id;
    store.insert_layer(robot_asset.layer.expect("first resolve returns layer"));

    let env_asset = resolver
        .resolve("sets/env.layer", None, &mut store.tokens, &mut store.paths)
        .expect("env asset should resolve");
    let env_layer_id = env_asset.layer_id;
    store.insert_layer(env_asset.layer.expect("first resolve returns layer"));

    // Deduplication: resolving the same URI again returns the same LayerId,
    // with layer = None (no need to re-insert).
    let robot_again = resolver
        .resolve(
            "props/robot.layer",
            None,
            &mut store.tokens,
            &mut store.paths,
        )
        .expect("should deduplicate");
    assert_eq!(
        robot_layer_id, robot_again.layer_id,
        "dedup should return same LayerId"
    );
    assert!(
        robot_again.layer.is_none(),
        "dedup hit should not return layer"
    );
    println!(
        "Deduplication works: both resolve to layer {}",
        robot_layer_id.0
    );

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

    // Show resolved paths via the resolver.
    println!(
        "\nResolved path for robot layer: {:?}",
        resolver.resolved_path(robot_layer_id)
    );
    println!(
        "Resolved path for env layer: {:?}",
        resolver.resolved_path(env_layer_id)
    );

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
