//! Typed schema layer: domain-specific types over `Value::Opaque`.
//!
//! The layerstack kernel is domain-neutral — it stores `Value` enums and
//! resolves them by strength. Domain-specific types (transforms, colors,
//! bounding boxes) are encoded as `Value::Opaque` with a type discriminator
//! and decoded by a schema layer above the kernel.
//!
//! This example builds a tiny "robot scene schema" with:
//! - `Transform` (position + rotation as f32 arrays)
//! - `Color` (RGBA as f32 array)
//! - `BoundingBox` (min/max corners)
//!
//! It shows:
//! 1. Encoding domain types into `Value::Opaque`
//! 2. Decoding them back with a typed API
//! 3. Composition still works (strongest opinion wins, `ListOps` chain)
//! 4. Provenance tells you which layer provided the winning value

use std::sync::Arc;

use layerstack::{
    InMemoryStore, Layer, LayerId, PathId, PrimSpec, Stage, StageOptions, TokenId, Value,
};

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// A 3D transform: position and Euler rotation.
#[derive(Debug, Clone, PartialEq)]
struct Transform {
    position: [f32; 3],
    rotation: [f32; 3],
}

/// An RGBA color.
#[derive(Debug, Clone, PartialEq)]
struct Color {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

/// An axis-aligned bounding box.
#[derive(Debug, Clone, PartialEq)]
struct BoundingBox {
    min: [f32; 3],
    max: [f32; 3],
}

// ---------------------------------------------------------------------------
// Codec: domain types ↔ Value::Opaque
// ---------------------------------------------------------------------------

/// Registry of type names used as discriminators in `Value::Opaque`.
struct SchemaTokens {
    transform: TokenId,
    color: TokenId,
    bbox: TokenId,
    // Field names.
    field_xform: TokenId,
    field_color: TokenId,
    field_bounds: TokenId,
    field_name: TokenId,
}

impl SchemaTokens {
    fn intern(store: &mut InMemoryStore) -> Self {
        Self {
            transform: store.tokens.intern("schema:Transform"),
            color: store.tokens.intern("schema:Color"),
            bbox: store.tokens.intern("schema:BoundingBox"),
            field_xform: store.tokens.intern("xformOp:transform"),
            field_color: store.tokens.intern("primvars:displayColor"),
            field_bounds: store.tokens.intern("extent"),
            field_name: store.tokens.intern("name"),
        }
    }
}

/// Encodes a `Transform` as `Value::Opaque`.
fn encode_transform(tokens: &SchemaTokens, xform: &Transform) -> Value {
    // Simple encoding: 6 × f32, little-endian.
    let mut bytes = Vec::with_capacity(24);
    for &v in &xform.position {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    for &v in &xform.rotation {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    Value::Opaque {
        type_name: tokens.transform,
        bytes: Arc::from(bytes.as_slice()),
    }
}

/// Decodes a `Transform` from `Value::Opaque`.
fn decode_transform(tokens: &SchemaTokens, value: &Value) -> Option<Transform> {
    match value {
        Value::Opaque { type_name, bytes }
            if *type_name == tokens.transform && bytes.len() == 24 =>
        {
            let f = |i: usize| f32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
            Some(Transform {
                position: [f(0), f(4), f(8)],
                rotation: [f(12), f(16), f(20)],
            })
        }
        _ => None,
    }
}

/// Encodes a `Color` as `Value::Opaque`.
fn encode_color(tokens: &SchemaTokens, color: &Color) -> Value {
    let mut bytes = Vec::with_capacity(16);
    for &v in &[color.r, color.g, color.b, color.a] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    Value::Opaque {
        type_name: tokens.color,
        bytes: Arc::from(bytes.as_slice()),
    }
}

/// Decodes a `Color` from `Value::Opaque`.
fn decode_color(tokens: &SchemaTokens, value: &Value) -> Option<Color> {
    match value {
        Value::Opaque { type_name, bytes } if *type_name == tokens.color && bytes.len() == 16 => {
            let f = |i: usize| f32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
            Some(Color {
                r: f(0),
                g: f(4),
                b: f(8),
                a: f(12),
            })
        }
        _ => None,
    }
}

/// Encodes a `BoundingBox` as `Value::Opaque`.
fn encode_bbox(tokens: &SchemaTokens, bbox: &BoundingBox) -> Value {
    let mut bytes = Vec::with_capacity(24);
    for &v in &bbox.min {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    for &v in &bbox.max {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    Value::Opaque {
        type_name: tokens.bbox,
        bytes: Arc::from(bytes.as_slice()),
    }
}

/// Decodes a `BoundingBox` from `Value::Opaque`.
fn decode_bbox(tokens: &SchemaTokens, value: &Value) -> Option<BoundingBox> {
    match value {
        Value::Opaque { type_name, bytes } if *type_name == tokens.bbox && bytes.len() == 24 => {
            let f = |i: usize| f32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
            Some(BoundingBox {
                min: [f(0), f(4), f(8)],
                max: [f(12), f(16), f(20)],
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Typed query helpers
// ---------------------------------------------------------------------------

/// Typed wrapper over `Stage` for the robot schema.
struct SceneQuery<'a> {
    stage: &'a Stage,
    tokens: &'a SchemaTokens,
}

impl<'a> SceneQuery<'a> {
    fn transform(&self, prim: PathId) -> Option<Transform> {
        let resolved = self.stage.resolve_field(prim, self.tokens.field_xform)?;
        decode_transform(self.tokens, &resolved.value)
    }

    fn color(&self, prim: PathId) -> Option<Color> {
        let resolved = self.stage.resolve_field(prim, self.tokens.field_color)?;
        decode_color(self.tokens, &resolved.value)
    }

    fn bounds(&self, prim: PathId) -> Option<BoundingBox> {
        let resolved = self.stage.resolve_field(prim, self.tokens.field_bounds)?;
        decode_bbox(self.tokens, &resolved.value)
    }

    fn name(&self, prim: PathId) -> Option<String> {
        let resolved = self.stage.resolve_field(prim, self.tokens.field_name)?;
        match &resolved.value {
            Value::String(s) => Some(s.to_string()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let mut store = InMemoryStore::default();
    let schema = SchemaTokens::intern(&mut store);

    let robot = store.path("/Robot");
    let arm = store.path("/Robot/Arm");

    // --- Base layer: defines the robot with default values. ---
    let mut base = Layer::new(LayerId(1));
    base.sublayers = vec![LayerId(2)];

    let arm_token = store.tokens.intern("Arm");

    let robot_spec = PrimSpec::def()
        .with_children(vec![arm_token])
        .with_field(schema.field_name, "Atlas")
        .with_field(
            schema.field_xform,
            encode_transform(
                &schema,
                &Transform {
                    position: [0.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0],
                },
            ),
        )
        .with_field(
            schema.field_bounds,
            encode_bbox(
                &schema,
                &BoundingBox {
                    min: [-1.0, 0.0, -1.0],
                    max: [1.0, 2.0, 1.0],
                },
            ),
        );
    base.insert_prim(robot, robot_spec);

    let arm_spec = PrimSpec::def()
        .with_field(
            schema.field_color,
            encode_color(
                &schema,
                &Color {
                    r: 0.5,
                    g: 0.5,
                    b: 0.5,
                    a: 1.0,
                },
            ),
        )
        .with_field(
            schema.field_xform,
            encode_transform(
                &schema,
                &Transform {
                    position: [0.5, 1.0, 0.0],
                    rotation: [0.0, 0.0, 0.0],
                },
            ),
        );
    base.insert_prim(arm, arm_spec);
    store.insert_layer(base);

    // --- Override layer: a "shot" layer that repositions the robot and
    //     recolors the arm. Stronger than the base. ---
    let mut shot = Layer::new(LayerId(2));

    // Override only the transform on the robot (position it in the scene).
    let robot_override = PrimSpec::default().with_field(
        schema.field_xform,
        encode_transform(
            &schema,
            &Transform {
                position: [10.0, 0.0, 5.0],
                rotation: [0.0, 45.0, 0.0],
            },
        ),
    );
    shot.insert_prim(robot, robot_override);

    // Override the arm color to red.
    let arm_override = PrimSpec::default().with_field(
        schema.field_color,
        encode_color(
            &schema,
            &Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        ),
    );
    shot.insert_prim(arm, arm_override);
    store.insert_layer(shot);

    // --- Compose and query with the typed API. ---
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );

    let query = SceneQuery {
        stage: &stage,
        tokens: &schema,
    };

    println!("/Robot");
    println!("  name      = {:?}", query.name(robot).unwrap());
    // Base layer provides the name (no override in shot layer).
    println!("  transform = {:?}", query.transform(robot).unwrap());
    // Shot layer wins — robot is at (10, 0, 5) rotated 45°.
    println!("  bounds    = {:?}", query.bounds(robot).unwrap());
    // Only base layer has bounds, so base wins.

    println!("/Robot/Arm");
    println!("  color     = {:?}", query.color(arm).unwrap());
    // Shot layer wins — arm is red.
    println!("  transform = {:?}", query.transform(arm).unwrap());
    // Only base layer has arm transform, so base wins.

    // Verify composition: shot layer (LayerId(2)) is a sublayer of base
    // (LayerId(1)), making base STRONGER. But we inserted the shot as
    // sublayer, meaning base opinions are checked first.
    //
    // Wait — sublayers are WEAKER than the parent. LayerId(1) lists
    // LayerId(2) as a sublayer, so LayerId(1) is stronger. Let's verify:
    let robot_xform_prov = stage
        .resolve_field(robot, schema.field_xform)
        .unwrap()
        .provenance
        .unwrap();
    println!(
        "\nRobot transform provided by layer {} (1=base, 2=shot)",
        robot_xform_prov.layer.0
    );

    // The base layer is stronger, so it provides the transform.
    // To make the shot layer stronger, it should be the ROOT with
    // base as its sublayer. Let's recompose with that ordering:
    println!("\n--- Recomposing with shot as strongest layer ---\n");

    // Move the shot to be the root, with base as sublayer.
    {
        let shot = store.layers.get_mut(&LayerId(2)).unwrap();
        shot.sublayers = vec![LayerId(1)];
        let base = store.layers.get_mut(&LayerId(1)).unwrap();
        base.sublayers = vec![];
    }

    let stage2 = Stage::compose(
        &mut store,
        LayerId(2), // Shot is now root (strongest).
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );

    let query2 = SceneQuery {
        stage: &stage2,
        tokens: &schema,
    };

    println!("/Robot");
    println!("  transform = {:?}", query2.transform(robot).unwrap());
    println!("  name      = {:?}", query2.name(robot).unwrap());
    // Now shot's transform wins; name still comes from base (only source).

    println!("/Robot/Arm");
    println!("  color     = {:?}", query2.color(arm).unwrap());
    // Shot's red color wins over base's gray.

    let prov = stage2
        .resolve_field(robot, schema.field_xform)
        .unwrap()
        .provenance
        .unwrap();
    println!(
        "\nRobot transform now provided by layer {} (shot wins!)",
        prov.layer.0
    );
}
