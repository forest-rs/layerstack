// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Timeline composition for a video-editor-like scenario.
//!
//! This example builds a simple multi-track timeline — the kind you'd see in a
//! video editor — on top of layerstack's composition primitives. It shows how
//! several core features combine to support time-varying media:
//!
//! - **Layers as tracks**: each layer is a separate editorial decision (base
//!   animation, color-grade layer). Stronger layers win over weaker ones, just
//!   like higher tracks in a nonlinear editor.
//! - **`TimeSamples`**: per-frame keyframes for properties like position.
//! - **Splines**: smooth Bézier animation curves (e.g., an opacity ease-in).
//! - **Layer offsets**: retiming a sublayer so its local frame range lands at a
//!   different point on the global timeline (like slipping a clip).
//! - **Interpolation modes**: Held (step/hard-cut) vs. Linear (smooth blend).
//! - **Value resolution at time**: querying a composed property at any frame.
//!
//! ## The scenario
//!
//! A character walks across screen while fading in:
//!
//! - **Base layer** (weakest): position keyframes at frames 0–24 and an opacity
//!   spline that eases from 0→1 over frames 0–12. Included with a +10 frame
//!   offset, so it plays starting at global frame 10.
//! - **Override layer** (stronger): scalar defaults for position (-10) and
//!   opacity (0.8). These never win because the base's timeSamples and spline
//!   outrank scalars — but they'd be the fallback if those were removed.
//! - **Root layer** (strongest): empty — just wires the stack together.
//!
//! ### Resolution rules demonstrated
//!
//! For **`position_x`**: the override has a scalar default (-10.0) and the base
//! has timeSamples. `TimeSamples` always beat scalars regardless of layer strength
//! (§12.3), so the base's keyframes win at every frame. The offset shifts them
//! so local frame 0 starts at global frame 10.
//!
//! For **opacity**: the override has a scalar default (0.8) and the base has a
//! spline. Splines beat scalar defaults (§12.3), so the base layer's Bézier
//! fade-in wins at every frame. The override's 0.8 would only appear if the
//! spline were removed.

use layerstack::{
    FieldValue, InMemoryStore, InterpolationType, Layer, LayerId, LayerOffset, PrimSpec,
    SplineData, Stage, StageOptions, SublayerEntry, Value,
    spline::{CurveType, Extrapolation, Knot, KnotInterp, SplineDataType},
};

fn main() {
    let mut store = InMemoryStore::default();

    // -----------------------------------------------------------------------
    // Intern tokens and paths
    //
    // In OpenUSD (and layerstack), all field names and prim paths are interned
    // — stored once and referred to by small IDs. This keeps comparison fast
    // and memory compact.
    // -----------------------------------------------------------------------

    let position_x = store.tokens.intern("position_x");
    let opacity = store.tokens.intern("opacity");
    let char_path = store.path("/Character");

    // -----------------------------------------------------------------------
    // Layer 1 — Base animation (weakest track)
    //
    // The "source clip": raw animation data before editorial decisions.
    //
    // Position is stored as TimeSamples — sorted (time, value) pairs that
    // the engine can interpolate between. Think of them as keyframes on a
    // timeline.
    //
    // Opacity is stored as a Spline — a smooth Bézier curve. Splines are
    // ideal for eases and organic transitions where you want smooth
    // interpolation rather than linearly connecting dots.
    // -----------------------------------------------------------------------

    let mut base = Layer::new(LayerId(1));

    // Walk keyframes: x=0 at frame 0, x=50 at frame 12, x=100 at frame 24.
    // These are in the base layer's *local* time. The layer offset (applied
    // below) will shift them +10 frames on the global timeline.
    let walk_samples: Vec<(f64, Value)> = vec![
        (0.0, Value::Float(0.0)),    // start of walk
        (12.0, Value::Float(50.0)),  // midpoint
        (24.0, Value::Float(100.0)), // destination
    ];

    // Opacity fade-in: a 2-knot Bézier spline from 0.0 to 1.0 over 12 frames.
    // The Bézier tangent handles create a smooth ease (not a linear ramp).
    let fade_in = SplineData {
        data_type: SplineDataType::Float,
        default_curve_type: CurveType::Bezier,
        // "Held" extrapolation: before the first knot, hold its value (0.0);
        // after the last knot, hold its value (1.0). The character stays
        // transparent before the fade and stays opaque after.
        pre_extrapolation: Extrapolation::Held,
        post_extrapolation: Extrapolation::Held,
        loop_params: None,
        knots: vec![
            Knot {
                time: 0.0,
                value: 0.0, // fully transparent at the start
                pre_value: None,
                next_interp: KnotInterp::Curve, // Bézier interpolation to next knot
                curve_type: CurveType::Bezier,
                pre_tan_maya_form: false,
                post_tan_maya_form: false,
                pre_tan_width: 0.0,
                post_tan_width: 4.0, // tangent handle extends 4 frames right
                pre_tan_slope: 0.0,
                post_tan_slope: 0.05, // gentle start (ease-out of zero)
            },
            Knot {
                time: 12.0,
                value: 1.0, // fully opaque by frame 12
                pre_value: None,
                next_interp: KnotInterp::Held, // stay at 1.0 after this point
                curve_type: CurveType::Bezier,
                pre_tan_maya_form: false,
                post_tan_maya_form: false,
                pre_tan_width: 4.0, // tangent handle extends 4 frames left
                post_tan_width: 0.0,
                pre_tan_slope: 0.05, // gentle arrival (ease-in to 1.0)
                post_tan_slope: 0.0,
            },
        ],
    };

    base.insert_prim(
        char_path,
        PrimSpec::def()
            // TimeSamples are a FieldValue variant — they store per-frame data.
            .with_field(position_x, FieldValue::TimeSamples(walk_samples))
            // Splines are another FieldValue variant — smooth curves.
            .with_field(opacity, FieldValue::Spline(fade_in.clone())),
    );
    store.insert_layer(base);

    // -----------------------------------------------------------------------
    // Layer 2 — Override layer (middle track)
    //
    // The "adjustments" track. Provides scalar defaults:
    //   - position_x = -10.0 (character starts offscreen)
    //   - opacity = 0.8
    //
    // Key insight about resolution priority (§12.3):
    //   TimeSamples > Spline > Default (scalar)
    //
    // This ordering applies *across* layers. Even though this override layer
    // is stronger than the base, the base layer's TimeSamples (for position)
    // and Spline (for opacity) both outrank this layer's scalar defaults.
    //
    // The override's scalars act as fallback values that would appear if
    // the base layer's timeSamples/spline were removed entirely.
    // -----------------------------------------------------------------------

    let mut overrides = Layer::new(LayerId(2));
    overrides.insert_prim(
        char_path,
        PrimSpec::over() // "over" = provides opinions without defining the prim
            .with_field(position_x, Value::Float(-10.0)) // scalar default
            .with_field(opacity, Value::Float(0.8)), // scalar default
    );
    store.insert_layer(overrides);

    // -----------------------------------------------------------------------
    // Layer 3 — Root layer (strongest, wires the stack together)
    //
    // The root layer has no opinions of its own. It lists sublayers in
    // strength order: first listed = strongest.
    //
    // The layer offset on the base sublayer means:
    //   mapped_time = query_time * scale + offset
    //   With offset=-10, scale=1: querying global frame 10 reads base frame 0.
    //   The base layer's content is shifted 10 frames later on the timeline.
    //
    // In video editor terms, this is like dragging the base clip 10 frames
    // to the right on the timeline.
    // -----------------------------------------------------------------------

    let mut root = Layer::new(LayerId(3));
    root.sublayers = vec![
        // Override layer — no time offset (plays at global time).
        SublayerEntry::new(LayerId(2)),
        // Base layer — shifted 10 frames later on the global timeline.
        // offset=-10 means: mapped_time = query_time - 10.
        // So global frame 10 reads base-local frame 0.
        SublayerEntry {
            layer: LayerId(1),
            offset: LayerOffset {
                offset: -10.0,
                scale: 1.0,
            },
        },
    ];
    store.insert_layer(root);

    // -----------------------------------------------------------------------
    // Compose the stage
    //
    // Composition flattens all layers into a single queryable stage. It walks
    // the sublayer tree, builds a strength-ordered opinion stack for each
    // prim and field, and prepares for time-varying queries.
    // -----------------------------------------------------------------------

    let stage = Stage::compose(
        &mut store,
        LayerId(3),
        StageOptions {
            with_provenance: true, // track which layer each value came from
            ..StageOptions::default()
        },
    );

    // -----------------------------------------------------------------------
    // Part 1: Query the timeline at various frames
    //
    // resolve_value_at_time() does the full resolution dance:
    //   1. Walk opinions strongest → weakest
    //   2. For each opinion with TimeSamples, remap time through layer offset
    //   3. First TimeSamples opinion wins (interpolating between samples)
    //   4. If no TimeSamples, first Spline opinion wins (evaluating the curve)
    //   5. If no Spline, first scalar Default wins
    //
    // Expected behavior:
    //   - Position: base layer's timeSamples win (offset by +10). At frame 22
    //     the engine reads base-local frame 12 → x=50.
    //   - Opacity: base layer's spline wins (offset by +10). The Bézier
    //     ease-in plays from global frame 10 to 22.
    // -----------------------------------------------------------------------

    println!("=== Part 1: Timeline Queries (Linear interpolation) ===");
    println!();
    println!("  Layer stack (strongest → weakest):");
    println!("    Root (3)     — empty shell, wires sublayers together");
    println!("    Override (2) — scalar defaults: position=-10, opacity=0.8");
    println!("    Base (1)     — timeSamples for position, spline for opacity");
    println!("                   (shifted +10 frames on the global timeline)");
    println!();
    println!("  Resolution priority: TimeSamples > Spline > Default");
    println!("  So the base's timeSamples/spline beat the override's scalars.");
    println!();

    for frame in [0.0, 5.0, 10.0, 15.0, 22.0, 30.0, 34.0] {
        let pos =
            stage.resolve_value_at_time(char_path, position_x, frame, InterpolationType::Linear);
        let opa = stage.resolve_value_at_time(char_path, opacity, frame, InterpolationType::Linear);

        let pos_str = pos.as_ref().map_or("none".into(), |r| fmt_val(&r.value));
        let opa_str = opa.as_ref().map_or("none".into(), |r| fmt_val(&r.value));
        let pos_src = source_name(pos.as_ref().and_then(|r| r.provenance.as_ref()));
        let opa_src = source_name(opa.as_ref().and_then(|r| r.provenance.as_ref()));

        println!(
            "  frame {frame:5.1}  position = {pos_str:>7} ({pos_src:<9})  \
             opacity = {opa_str:>5} ({opa_src})"
        );
    }

    // -----------------------------------------------------------------------
    // Part 2: Interpolation modes — Linear vs. Held
    //
    // The same timeSamples produce different results depending on the
    // interpolation mode:
    //   - Linear: smoothly blends between neighboring samples (like a dissolve)
    //   - Held: snaps to the previous sample's value (like a hard cut)
    //
    // We query at global frame 16.0, which maps to base-local frame 6.0
    // (between base samples at 0.0 and 12.0).
    // -----------------------------------------------------------------------

    println!();
    println!("=== Part 2: Interpolation Modes (position_x at frame 16.0) ===");
    println!();
    println!("  Global frame 16 maps to base-local frame 6 (due to +10 offset).");
    println!("  Base samples: frame 0 → 0.0, frame 12 → 50.0");
    println!();

    let linear = stage
        .resolve_value_at_time(char_path, position_x, 16.0, InterpolationType::Linear)
        .map(|r| fmt_val(&r.value))
        .unwrap_or_else(|| "none".into());

    let held = stage
        .resolve_value_at_time(char_path, position_x, 16.0, InterpolationType::Held)
        .map(|r| fmt_val(&r.value))
        .unwrap_or_else(|| "none".into());

    println!("  Linear: {linear}  (blends between samples → smooth motion)");
    println!("  Held:   {held}  (holds frame-0 value → snaps like a hard cut)");

    // -----------------------------------------------------------------------
    // Part 3: Spline curve visualization
    //
    // Show the Bézier ease-in shape by evaluating the spline directly.
    // This is the base layer's local view — in the composed stage it plays
    // at global frames 10–22 (shifted by the +10 layer offset).
    // -----------------------------------------------------------------------

    println!();
    println!("=== Part 3: Opacity Spline Curve (Bézier ease-in) ===");
    println!();
    println!("  Direct evaluation of the base layer's fade-in spline.");
    println!("  In the composed stage this plays at global frames 10–22");
    println!("  (due to the +10 layer offset).");
    println!();

    for f in (0..=14).map(|i| i as f64) {
        let v = fade_in.evaluate(f).unwrap_or(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "intentional f64→usize for bar chart width"
        )]
        let bar_len = (v * 40.0) as usize;
        let bar: String = "#".repeat(bar_len);
        println!("  local frame {f:5.1}  opacity = {v:.3}  {bar}");
    }

    // -----------------------------------------------------------------------
    // Part 4: Layer offset in action
    //
    // The base layer has a +10 frame offset. Querying global frame 22 maps
    // to base-local frame 12, which is the walk midpoint (50.0). Without
    // the offset, frame 22 would read base-local frame 22 and get ~91.7.
    //
    // This is the USD equivalent of "slipping" a clip on the timeline.
    // -----------------------------------------------------------------------

    println!();
    println!("=== Part 4: Layer Offset Remapping ===");
    println!();
    println!("  Base layer is shifted +10 frames.");
    println!("  Global frame 22 → base-local frame 12 (the midpoint).");
    println!();

    if let Some(r) =
        stage.resolve_value_at_time(char_path, position_x, 22.0, InterpolationType::Linear)
    {
        println!("  position at global frame 22.0 = {}", fmt_val(&r.value));
        if let Some(p) = &r.provenance {
            println!("  source: {} layer", source_name(Some(p)));
        }
    }

    println!();
    println!("  Compare: without offset, frame 22 in a 0..24 range would be");
    println!("  near the end of the walk (~91.7). The +10 offset shifts it");
    println!("  to the midpoint.");
}

/// Format a [`Value`] for display, showing floats with 2 decimal places.
fn fmt_val(v: &Value) -> String {
    match v {
        Value::Float(f) => format!("{f:.2}"),
        Value::Double(f) => format!("{f:.2}"),
        _ => format!("{v}"),
    }
}

/// Map a provenance layer ID to a human-readable name.
fn source_name(prov: Option<&layerstack::stage::Provenance>) -> &'static str {
    match prov.map(|p| p.layer) {
        Some(LayerId(1)) => "base",
        Some(LayerId(2)) => "override",
        Some(LayerId(3)) => "root",
        _ => "?",
    }
}
