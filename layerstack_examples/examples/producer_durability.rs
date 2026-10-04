// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Two retained producers: unchanged work, source replacement, stale results and
//! bounded-history recovery. Run `cargo run -p layerstack_examples --example producer_durability`.
#[path = "support/generated_assets.rs"]
mod support;

fn main() {
    let mut generated = support::GeneratedScene::new(100);
    generated.prove_durability();
    let retained_work = generated.publication.work;
    assert_eq!(
        generated.publish(),
        layerstack::Changes::default(),
        "recovered publication is unchanged"
    );
    println!(
        "Two producers passed unchanged requests, unrelated edits, output repair, source replacement, stale delayed publication and history recovery."
    );
    println!(
        "Asset work: {:?}; publication work: {:?}",
        generated.asset_generator.work(),
        retained_work
    );
    println!(
        "Composed history: {:?}; terrain history: {:?}",
        generated.live.change_history_stats(),
        generated.store.layers[&support::TERRAIN].change_history_stats()
    );
}
