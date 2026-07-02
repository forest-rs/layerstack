// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Conformance tests derived from the supplemental composition suite.

use std::path::{Path, PathBuf};

use layerstack::{LayerId, LayerStack, Stage, StageOptions};

use layerstack_conformance::{
    pcp::load_pcp_json,
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};

fn assets_dir() -> PathBuf {
    workspace_root()
        .join("core-spec-supplemental-release_dec2025")
        .join("composition")
        .join("tests")
        .join("assets")
}

fn load_fixture(name: &str) -> (LoadedStage, PathBuf) {
    let dir = assets_dir().join(name);
    let pcp_path = dir.join("pcp.json");
    let pcp = load_pcp_json(&pcp_path);

    // Prefer USDA for these tests.
    let entry_usda = dir.join("usda").join(&pcp.entry);
    assert!(entry_usda.is_file(), "missing USDA entry at {entry_usda:?}");
    (load_entry_usda(&entry_usda), pcp_path)
}

fn layer_stack_names(loaded: &LoadedStage) -> Vec<String> {
    let stack = LayerStack::gather(&loaded.store, loaded.root_layer);
    stack
        .layers
        .into_iter()
        .map(|id| loaded.layer_names.get(&id).cloned().unwrap_or_default())
        .collect()
}

fn assert_layer_stack_matches(loaded: &LoadedStage, pcp_path: &Path) {
    let pcp = load_pcp_json(pcp_path);
    assert_eq!(
        layer_stack_names(loaded),
        pcp.layer_stack,
        "layer stack mismatch for {pcp_path:?}"
    );
}

fn assert_pcp_composing(loaded: &mut LoadedStage, pcp_path: &Path) {
    let pcp = load_pcp_json(pcp_path);

    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );

    for (prim_path, expectations) in pcp.composing {
        let prim = layerstack::Path::parse_absolute(&prim_path, &mut loaded.store.tokens)
            .expect("pcp path")
            .clone();
        let prim_id = loaded.store.paths.intern(prim);
        assert!(stage.has_prim(prim_id), "missing prim {prim_path}");

        let mut by_name = std::collections::HashMap::<String, LayerId>::new();
        for (id, name) in &loaded.layer_names {
            by_name.insert(name.clone(), *id);
        }

        if let Some(children) = expectations.child_names {
            let mut child_ids = Vec::new();
            for child in &children {
                let child_path = format!("{prim_path}/{child}");
                let child = layerstack::Path::parse_absolute(&child_path, &mut loaded.store.tokens)
                    .expect("pcp child path")
                    .clone();
                let child_id = loaded.store.paths.intern(child);
                assert!(stage.has_prim(child_id), "missing child prim {child_path}");
                child_ids.push(child_id);
            }

            let actual = stage
                .children_of(prim_id)
                .unwrap_or_else(|| panic!("missing children list for {prim_path}"));
            let render = |ids: &[layerstack::PathId]| {
                ids.iter()
                    .map(|id| {
                        loaded
                            .store
                            .paths
                            .resolve(*id)
                            .leaf()
                            .map(|tok| loaded.store.tokens.resolve(tok).to_string())
                            .unwrap_or_else(|| "<root>".to_string())
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                actual,
                child_ids,
                "child order mismatch for {prim_path} in {pcp_path:?}\n  actual: {:?}\nexpected: {:?}",
                render(actual),
                render(&child_ids)
            );
        }

        if let Some(stack) = expectations.prim_stack {
            let actual = stage
                .prim_stack(prim_id)
                .unwrap_or_else(|| panic!("missing prim stack for {prim_path}"));

            for (layer_name, expected_spec) in stack {
                let expected_layer = *by_name
                    .get(&layer_name)
                    .unwrap_or_else(|| panic!("unknown layer {layer_name} in pcp.json"));

                let expected_path = layerstack::SpecPath::parse(
                    &expected_spec,
                    &mut loaded.store.tokens,
                    &mut loaded.store.paths,
                )
                .expect("pcp prim stack path");

                let found = actual.iter().any(|(layer_id, spec_path)| {
                    *layer_id == expected_layer && *spec_path == expected_path
                });
                if !found {
                    eprintln!("  Prim stack for {prim_path}:");
                    for (lid, sid) in &actual {
                        let lname = loaded.layer_names.get(lid).cloned().unwrap_or_default();
                        eprintln!("    {lname}: {}", sid.display(&loaded.store.tokens));
                    }
                }
                assert!(
                    found,
                    "missing prim stack entry for {prim_path}: expected {layer_name} {expected_spec}"
                );
            }
        }

        if let Some(props) = expectations.property_names {
            for prop in props {
                let tok = loaded.store.tokens.intern(&prop);
                assert!(
                    stage.resolve_value(prim_id, tok).is_some(),
                    "missing property/field {prim_path}.{prop}"
                );
            }
        }

        if let Some(stacks) = expectations.property_stacks {
            for (prop_path, stack) in stacks {
                let suffix = prop_path
                    .strip_prefix(&format!("{prim_path}."))
                    .unwrap_or_else(|| panic!("unexpected property stack key {prop_path}"));
                let dest_field = loaded.store.tokens.intern(suffix);

                let Some(opinions) = stage.explain_field(prim_id, dest_field) else {
                    panic!("missing property opinions for {prop_path}");
                };

                for (layer_name, expected_spec) in stack {
                    let expected_layer = *by_name
                        .get(&layer_name)
                        .unwrap_or_else(|| panic!("unknown layer {layer_name} in pcp.json"));

                    let expected_spec_path = layerstack::SpecPath::parse(
                        &expected_spec,
                        &mut loaded.store.tokens,
                        &mut loaded.store.paths,
                    )
                    .expect("expected property spec path");

                    assert!(
                        opinions.iter().any(|op| {
                            op.key.layer_id == expected_layer
                                && op.key.spec_path == expected_spec_path
                        }),
                        "missing stack entry for {prop_path}: expected {layer_name} {expected_spec}"
                    );
                }
            }
        }

        if let Some(targets) = expectations.relationship_targets {
            for (prop_path, expected) in targets {
                let suffix = prop_path
                    .strip_prefix(&format!("{prim_path}."))
                    .unwrap_or_else(|| panic!("unexpected relationship key {prop_path}"));
                let field = loaded.store.tokens.intern(suffix);

                let resolved = stage
                    .resolve_path_list(prim_id, field)
                    .unwrap_or_else(|| panic!("missing relationship targets for {prop_path}"));

                let expected_targets: Vec<_> = expected
                    .into_iter()
                    .map(|p| {
                        layerstack::TargetPath::parse(
                            &p,
                            &mut loaded.store.tokens,
                            &mut loaded.store.paths,
                        )
                        .expect("target path")
                    })
                    .collect();
                assert_eq!(
                    resolved.value, expected_targets,
                    "relationship target mismatch for {prop_path}"
                );
            }
        }

        if let Some(connections) = expectations.attribute_connections {
            for (prop_path, expected) in connections {
                let suffix = prop_path
                    .strip_prefix(&format!("{prim_path}."))
                    .unwrap_or_else(|| panic!("unexpected connection key {prop_path}"));
                let field = loaded.store.tokens.intern(suffix);

                let resolved = stage
                    .resolve_path_list(prim_id, field)
                    .unwrap_or_else(|| panic!("missing attribute connections for {prop_path}"));

                let expected_targets: Vec<_> = expected
                    .into_iter()
                    .map(|p| {
                        layerstack::TargetPath::parse(
                            &p,
                            &mut loaded.store.tokens,
                            &mut loaded.store.paths,
                        )
                        .expect("target path")
                    })
                    .collect();
                assert_eq!(
                    resolved.value, expected_targets,
                    "attribute connection mismatch for {prop_path}"
                );
            }
        }
    }
}

#[test]
fn basic_duplicate_sublayer_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicDuplicateSublayer_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn error_sublayer_cycle_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("ErrorSublayerCycle_root");
    let pcp = load_pcp_json(&pcp_path);
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert!(pcp.errors.is_some(), "fixture should record errors");
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_list_editing_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicListEditing_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_owner_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicOwner_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_reference_session_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicReference_session");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_class_hierarchy_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyClassHierarchy_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_reference_and_class_diamond_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicReferenceAndClassDiamond_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn relative_path_references_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("RelativePathReferences_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_reference_diamond_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicReferenceDiamond_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_ancestral_reference_root_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicAncestralReference_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_list_editing_with_inherits_root_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicListEditingWithInherits_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_reference_and_class_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicReferenceAndClass_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_local_and_global_class_combination_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicLocalAndGlobalClassCombination_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_specializes_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicSpecializes_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
#[ignore = "requires nested payload-through-subroot, self-payload, and default prim features"]
fn basic_payload_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicPayload_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_nested_payload_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicNestedPayload_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_specializes_and_inherits_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicSpecializesAndInherits_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_specializes_and_references_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicSpecializesAndReferences_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_specializes_and_variants_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicSpecializesAndVariants_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_nested_variants_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicNestedVariants_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_nested_variants_with_same_name_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicNestedVariantsWithSameName_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_payload_diamond_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicPayloadDiamond_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_nested_specializes_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyNestedSpecializes_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_nested_classes_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyNestedClasses_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_specializes_and_inherits_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickySpecializesAndInherits_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
#[ignore = "still needs specializes to inherit weaker referenced provenance from the specialized prim stack"]
fn variant_specializes_and_reference_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("VariantSpecializesAndReference_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
#[ignore = "requires fallback variant selection (standin=render not authored, comes from PCP test framework config)"]
fn case1_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("case1_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_non_local_variant_selection_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyNonLocalVariantSelection_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
#[ignore = "still needs ancestral source-path remap to lift stronger variant selections onto weaker longer source paths"]
fn tricky_variant_ancestral_selection_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantAncestralSelection_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_weaker_selection_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantWeakerSelection_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_independent_selection_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantIndependentSelection_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn bug74847_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("bug74847_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_nested_specializes2_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyNestedSpecializes2_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_selection_in_variant_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantSelectionInVariant_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_selection_in_variant2_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantSelectionInVariant2_root");
    assert_layer_stack_matches(&loaded, &pcp_path);

    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_variant_with_reference_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicVariantWithReference_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_weaker_selection2_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantWeakerSelection2_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_weaker_selection3_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantWeakerSelection3_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_weaker_selection4_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantWeakerSelection4_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_variant_with_connections_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicVariantWithConnections_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_override_of_local_class_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantOverrideOfLocalClass_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_variant_in_payload_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyVariantInPayload_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn tricky_inherits_in_variants_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyInheritsInVariants_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
#[ignore = "still needs child-source provenance alignment for LEyeRig/SymEyeRig in variant descendants"]
fn tricky_inherits_in_variants2_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("TrickyInheritsInVariants2_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn specializes_and_variants_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("SpecializesAndVariants_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn specializes_and_variants2_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("SpecializesAndVariants2_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_instancing_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicInstancing_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_instancing_and_nested_instances_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicInstancingAndNestedInstances_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_instancing_and_variants_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("BasicInstancingAndVariants_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn basic_time_offset_root() {
    let (mut loaded, pcp_path) = load_fixture("BasicTimeOffset_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

#[test]
fn reference_list_ops_with_offsets_root() {
    let (mut loaded, pcp_path) = load_fixture("ReferenceListOpsWithOffsets_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}
