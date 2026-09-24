// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composed scalar value assertions shared by the composition harnesses.
//!
//! Membership checks against a stack cannot detect a wrong winner: a stack
//! can contain every expected opinion in the wrong order. These checks pin the
//! resolved value and the provenance (layer and spec) of the strongest
//! opinion through the public [`Stage`] APIs.
//!
//! Spec: AOUSD Core §12.2 (value resolution: the strongest opinion wins).

use layerstack::{PropertyPath, SpecPath, Stage, StageOptions, Value};

use crate::usda_real::LoadedStage;

/// One expected scalar: `(property path, value, winning layer, winning spec)`.
///
/// The layer is the fixture-relative layer name (e.g. `model.usd`) and the
/// spec is a spec path such as `/Model{vset=a}Child.attr`.
pub type ScalarExpectation<'a> = (&'a str, Value, &'a str, &'a str);

/// Checks each expectation against `stage`, which must have been composed
/// with [`StageOptions::with_provenance`]. Returns one message per mismatch.
pub fn check_scalar_values(
    loaded: &mut LoadedStage,
    stage: &Stage,
    expected: &[ScalarExpectation<'_>],
) -> Vec<String> {
    let mut failures = Vec::new();
    for (prop_path, value, layer_name, spec) in expected {
        let path =
            PropertyPath::parse(prop_path, &mut loaded.store.tokens, &mut loaded.store.paths)
                .expect("property path");
        let Some(resolved) = stage.resolve_field_path(path) else {
            failures.push(format!("no resolved value for {prop_path}"));
            continue;
        };
        if &resolved.value != value {
            failures.push(format!(
                "wrong composed value for {prop_path}: got {:?}, expected {value:?}",
                resolved.value
            ));
            continue;
        }

        let Some(provenance) = resolved.provenance else {
            failures.push(format!("missing provenance for {prop_path}"));
            continue;
        };
        let actual_layer = loaded
            .layer_names
            .get(&provenance.layer)
            .cloned()
            .unwrap_or_default();
        let expected_spec =
            SpecPath::parse(spec, &mut loaded.store.tokens, &mut loaded.store.paths)
                .expect("expected spec path");
        if actual_layer != *layer_name || provenance.spec_path != expected_spec {
            failures.push(format!(
                "wrong winning opinion for {prop_path}: got {actual_layer} {}, expected {layer_name} {spec}",
                provenance.spec_path.display(&loaded.store.tokens)
            ));
        }
    }
    failures
}

/// Composes `loaded` with provenance and asserts every expectation.
pub fn assert_scalar_values(loaded: &mut LoadedStage, expected: &[ScalarExpectation<'_>]) {
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    let failures = check_scalar_values(loaded, &stage, expected);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
