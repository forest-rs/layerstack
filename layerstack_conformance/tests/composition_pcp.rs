// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Conformance tests derived from the supplemental composition suite.

use std::path::{Path, PathBuf};

use layerstack::{
    ArcKind, CompositionError, LayerId, LayerStack, PropertyPath, Stage, StageOptions,
    SublayerCycle, Value,
};

use layerstack_conformance::{
    pcp::load_pcp_json,
    scalar::assert_scalar_values,
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
                // A declared property need not author a value, so ask for
                // its opinions rather than a resolved value.
                assert!(
                    stage.has_property_path(PropertyPath::new(prim_id, tok)),
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

                let Some(opinions) =
                    stage.explain_property_path(PropertyPath::new(prim_id, dest_field))
                else {
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
                    .resolve_target_list_path(PropertyPath::new(prim_id, field))
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
                    .resolve_target_list_path(PropertyPath::new(prim_id, field))
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

    // `pcp.txt` reports one `PcpErrorSublayerCycle` per repeated visit:
    // B.usd names A.usd (via root → A → B), and A.usd names B.usd (via
    // root → B → A).
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let layer = |name: &str| {
        *loaded
            .layer_names
            .iter()
            .find(|(_, n)| n.as_str() == name)
            .unwrap_or_else(|| panic!("layer {name}"))
            .0
    };
    assert_eq!(
        stage.composition_errors(),
        [
            CompositionError::SublayerCycle(SublayerCycle {
                layer: layer("B.usd"),
                sublayer: layer("A.usd"),
            }),
            CompositionError::SublayerCycle(SublayerCycle {
                layer: layer("A.usd"),
                sublayer: layer("B.usd"),
            }),
        ]
    );
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
    // The local opinion inside the selected variant is stronger than the
    // internal reference / inherit to `_prototype` (LIVRPS).
    assert_scalar_values(
        &mut loaded,
        &[
            (
                "/ModelRefWithChildren/InstanceViaReference.attr2",
                Value::Int(456),
                "model.usd",
                "/Model{vset=with_children}InstanceViaReference.attr2",
            ),
            (
                "/ModelRefWithChildren/InstanceViaClass.attr2",
                Value::Int(789),
                "model.usd",
                "/Model{vset=with_children}InstanceViaClass.attr2",
            ),
            (
                "/ModelRefWithChildren/_prototype.attr2",
                Value::Int(123),
                "model.usd",
                "/Model{vset=with_children}_prototype.attr2",
            ),
            (
                "/ModelRefWithChildren.modelRootAttribute",
                Value::Int(123),
                "model.usd",
                "/Model.modelRootAttribute",
            ),
        ],
    );
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
#[ignore = "USDA ingestion keys variant-introduced descendants by namespace path, so the `tidscene` branch's `/Sarah/FaceRig/EyesRig` overwrites the selected `full` branch's spec; needs per-branch spec storage"]
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
    // `set.usd` authors `geom.x = 2.0` under the instanceable
    // `/Set/InstancedProp`, but that site comes from `/Set_1`'s reference,
    // an arc above the instance, so only the instance's own arcs (and the
    // classes they imply) contribute. The uninstanced sibling keeps it.
    assert_scalar_values(
        &mut loaded,
        &[
            (
                "/Set_1/InstancedProp/geom.x",
                Value::Double(3.5),
                "root.usd",
                "/_class_Prop/geom.x",
            ),
            (
                "/Set_1/UninstancedProp/geom.x",
                Value::Double(3.0),
                "root.usd",
                "/Set_1/UninstancedProp/geom.x",
            ),
        ],
    );
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
    // Only the selected variant (`x = "a"` / `x = "b"`) may contribute.
    assert_scalar_values(
        &mut loaded,
        &[
            (
                "/InstancedModel.x",
                Value::Double(1.0),
                "root.usd",
                "/InstancedModel{x=a}.x",
            ),
            (
                "/InstancedModel/geom.x",
                Value::Double(1.0),
                "root.usd",
                "/InstancedModel{x=a}geom.x",
            ),
            (
                "/UninstancedModel.x",
                Value::Double(4.0),
                "root.usd",
                "/UninstancedModel{x=b}.x",
            ),
            (
                "/UninstancedModel/geom.x",
                Value::Double(4.0),
                "root.usd",
                "/UninstancedModel{x=b}geom.x",
            ),
        ],
    );
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

#[test]
#[ignore = "requires relocates: `pcp.json` expects `/RelocatedInheritOfChild/Object`, relocated from `/RelocatedInheritOfChild/Child/Object`, and layerstack does not compose relocates; `error_arc_cycle_root_composes` checks the rest"]
fn error_arc_cycle_root_layer_stack_matches() {
    let (mut loaded, pcp_path) = load_fixture("ErrorArcCycle_root");
    assert_layer_stack_matches(&loaded, &pcp_path);
    assert_pcp_composing(&mut loaded, &pcp_path);
}

/// `ErrorArcCycle_root` authors reference, inherit and ancestral cycles.
/// Composition must terminate, report each cycle, skip the arc that closes
/// it, and match the ordered prim stacks and child names in `pcp.txt`.
///
/// Not checked: `/RelocatedInheritOfChild/Object` and its cycle error (no
/// relocates support), and the `/InheritOfChild` cycle error, because USDA
/// ingestion drops the relative inherit path `<Child>` that closes it (its
/// prim stack still matches).
#[test]
fn error_arc_cycle_root_composes() {
    let (mut loaded, _) = load_fixture("ErrorArcCycle_root");
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );

    let expected: &[(&str, &[&str], &[&str])] = &[
        (
            "/GroupRoot",
            &["root.usd /GroupRoot", "A.usd /GroupA", "B.usd /GroupB"],
            &["ChildA"],
        ),
        ("/GroupRoot/ChildA", &["A.usd /GroupA/ChildA"], &[]),
        ("/Parent", &["root.usd /Parent"], &["Child1", "Child2"]),
        (
            "/Parent/Child1",
            &["root.usd /Parent/Child1", "root.usd /Parent/Child2"],
            &[],
        ),
        (
            "/Parent/Child2",
            &["root.usd /Parent/Child2", "root.usd /Parent/Child1"],
            &[],
        ),
        (
            "/AnotherParent",
            &["root.usd /AnotherParent"],
            &["AnotherChild"],
        ),
        (
            "/AnotherParent/AnotherChild",
            &["root.usd /AnotherParent/AnotherChild", "model.usd /Model"],
            &[],
        ),
        (
            "/YetAnotherParent",
            &["root.usd /YetAnotherParent"],
            &["Child"],
        ),
        (
            "/YetAnotherParent/Child",
            &["root.usd /YetAnotherParent/Child"],
            &[],
        ),
        (
            "/CoRecursiveParent1",
            &["root.usd /CoRecursiveParent1"],
            &["Child1"],
        ),
        (
            "/CoRecursiveParent1/Child1",
            &[
                "root.usd /CoRecursiveParent1/Child1",
                "root.usd /CoRecursiveParent2",
            ],
            &["Child2"],
        ),
        (
            "/CoRecursiveParent1/Child1/Child2",
            &["root.usd /CoRecursiveParent2/Child2"],
            &[],
        ),
        (
            "/CoRecursiveParent2",
            &["root.usd /CoRecursiveParent2"],
            &["Child2"],
        ),
        (
            "/CoRecursiveParent2/Child2",
            &[
                "root.usd /CoRecursiveParent2/Child2",
                "root.usd /CoRecursiveParent1",
            ],
            &["Child1"],
        ),
        (
            "/CoRecursiveParent2/Child2/Child1",
            &["root.usd /CoRecursiveParent1/Child1"],
            &[],
        ),
        ("/InheritOfChild", &["root.usd /InheritOfChild"], &["Child"]),
        (
            "/InheritOfChild/Child",
            &["root.usd /InheritOfChild/Child"],
            &[],
        ),
        (
            "/RelocatedInheritOfChild/Child",
            &["root.usd /RelocatedInheritOfChild/Child"],
            &["Class"],
        ),
        (
            "/RelocatedInheritOfChild/Child/Class",
            &["root.usd /RelocatedInheritOfChild/Child/Class"],
            &["Object"],
        ),
        (
            "/RelocatedInheritOfChild/Child/Class/Object",
            &["root.usd /RelocatedInheritOfChild/Child/Class/Object"],
            &[],
        ),
    ];
    for (prim, stack, children) in expected {
        let path =
            layerstack::Path::parse_absolute(prim, &mut loaded.store.tokens).expect("prim path");
        let id = loaded.store.paths.intern(path);
        let actual_stack: Vec<String> = stage
            .prim_stack(id)
            .unwrap_or_else(|| panic!("{prim} is not composed"))
            .iter()
            .map(|(layer, spec)| {
                format!(
                    "{} {}",
                    loaded.layer_names[layer],
                    spec.display(&loaded.store.tokens)
                )
            })
            .collect();
        assert_eq!(&actual_stack, stack, "prim stack of {prim}");
        let actual_children: Vec<String> = stage
            .children_of(id)
            .unwrap_or(&[])
            .iter()
            .map(|child| {
                let leaf = loaded.store.paths.resolve(*child).leaf().expect("leaf");
                loaded.store.tokens.resolve(leaf).to_string()
            })
            .collect();
        assert_eq!(&actual_children, children, "children of {prim}");
    }

    // `pcp.txt`'s errors, in its notation.
    let mut expected_errors = vec![
        "</GroupRoot>: @root.usd@</GroupRoot> references @A.usd@</GroupA> references @B.usd@</GroupB> CANNOT references @A.usd@</GroupA>",
        "</Parent/Child1>: @root.usd@</Parent/Child1> inherits @root.usd@</Parent/Child2> CANNOT inherits @root.usd@</Parent/Child1>",
        "</Parent/Child2>: @root.usd@</Parent/Child2> inherits @root.usd@</Parent/Child1> CANNOT inherits @root.usd@</Parent/Child2>",
        "</AnotherParent/AnotherChild>: @root.usd@</AnotherParent/AnotherChild> references @model.usd@</Model> CANNOT references @root.usd@</AnotherParent>",
        "</YetAnotherParent/Child>: @root.usd@</YetAnotherParent/Child> CANNOT inherits @root.usd@</YetAnotherParent>",
        "</CoRecursiveParent1/Child1/Child2>: @root.usd@</CoRecursiveParent1/Child1/Child2> inherits @root.usd@</CoRecursiveParent2/Child2> CANNOT inherits @root.usd@</CoRecursiveParent1>",
        "</CoRecursiveParent2/Child2/Child1>: @root.usd@</CoRecursiveParent2/Child2/Child1> inherits @root.usd@</CoRecursiveParent1/Child1> CANNOT inherits @root.usd@</CoRecursiveParent2>",
        "</RelocatedInheritOfChild/Child>: @root.usd@</RelocatedInheritOfChild/Child> CANNOT inherits @root.usd@</RelocatedInheritOfChild/Child/Class>",
    ];
    let display = |path: layerstack::PathId| loaded.store.paths.display(path, &loaded.store.tokens);
    let mut actual_errors: Vec<String> = stage
        .composition_errors()
        .iter()
        .map(|error| {
            let CompositionError::ArcCycle(cycle) = error else {
                panic!("unexpected error {error:?}");
            };
            let last = cycle.sites.len() - 1;
            let mut out = format!("<{}>:", display(cycle.prim));
            for (i, site) in cycle.sites.iter().enumerate() {
                if let Some(arc) = site.arc {
                    let verb = match arc {
                        ArcKind::Inherits => "inherits",
                        ArcKind::References => "references",
                        other => panic!("unexpected arc {other:?}"),
                    };
                    let cannot = if i == last { "CANNOT " } else { "" };
                    out.push_str(&format!(" {cannot}{verb}"));
                }
                out.push_str(&format!(
                    " @{}@<{}>",
                    loaded.layer_names[&site.layer_stack],
                    display(site.path)
                ));
            }
            out
        })
        .collect();
    expected_errors.sort_unstable();
    actual_errors.sort_unstable();
    assert_eq!(actual_errors, expected_errors);
}
