// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse edits to a typed array of light rig slots, without USD.
//!
//! A rig's slots are authored as a dense `Vec<Light>`. Two layers edit them
//! sparsely: a weaker layer inserts a light and duplicates the last slot, and
//! a stronger layer dims one slot and grows the rig, filling new slots with an
//! unlit light. Only `opinionated` is used; the element type is the host's
//! own, and it has no `Default`.
//!
//! ```sh
//! cargo run -p layerstack_examples --example light_rig_slots
//! ```

use opinionated::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, FillWith};

/// One slot of a light rig.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Light {
    intensity: f32,
    color: [f32; 3],
}

const fn light(intensity: f32, color: [f32; 3]) -> Light {
    Light { intensity, color }
}

const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
const WARM: [f32; 3] = [1.0, 0.8, 0.6];
const COOL: [f32; 3] = [0.6, 0.8, 1.0];

/// The slot a resize adds when it grows the rig: an unlit light.
///
/// This is the rig's choice, not a property of `Light`, which is why the edit
/// asks the host for it instead of using a language default.
const UNLIT: Light = light(0.0, WHITE);

fn main() {
    let base = vec![light(10.0, WHITE), light(4.0, WARM), light(2.0, COOL)];

    // The weaker layer inserts a light as the second slot and duplicates the
    // (now) last slot at the end.
    let weaker = ArrayEdit {
        ops: vec![
            ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(light(6.0, WARM)),
                index: ArrayIndex::Position(1),
            },
            ArrayEditOp::Insert {
                src: ArrayEditOperand::CopyFrom(ArrayIndex::Position(-1)),
                index: ArrayIndex::End,
            },
        ],
    };

    // The stronger layer dims the duplicated slot and grows the rig to six
    // slots. `Resize` carries no fill of its own, so the host supplies one.
    let stronger = ArrayEdit {
        ops: vec![
            ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(light(1.0, COOL)),
                index: ArrayIndex::Position(-1),
            },
            ArrayEditOp::Resize { len: 6 },
        ],
    };

    // The unlit slot is only built when a resize actually grows the rig.
    let mut unlit_requests = 0;
    let unlit = FillWith(|| {
        unlit_requests += 1;
        Some(UNLIT)
    });

    // Composition keeps the edits sparse: one program, weaker then stronger.
    let composed = stronger.compose_over(&weaker);
    let rig = composed.compose_over_array(&base, unlit);

    println!("Base rig:     {base:?}");
    println!("Composed rig: {rig:?}");
    assert_eq!(
        rig,
        vec![
            light(10.0, WHITE),
            light(6.0, WARM),
            light(4.0, WARM),
            light(2.0, COOL),
            light(1.0, COOL),
            UNLIT,
        ],
        "slots from both edits, then an unlit light for the grown slot"
    );
    assert_eq!(
        unlit_requests, 1,
        "the unlit light fills the one growing resize"
    );

    // Applying the composed program equals applying each layer in turn.
    let in_turn = stronger.compose_over_array(&weaker.compose_over_array(&base, None), Some(UNLIT));
    assert_eq!(
        rig, in_turn,
        "composition must match applying the edits in turn"
    );

    // Without a fill, growth is skipped rather than inventing a light: the rig
    // keeps the five slots the other instructions produced.
    let unfilled = composed.compose_over_array(&base, None);
    println!("Without a fill: {unfilled:?}");
    assert_eq!(
        unfilled[..],
        rig[..5],
        "a missing fill skips the growth and leaves the other edits"
    );
}
