// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The conformance tests, as one test binary.
//!
//! Each module is one area's tests, and was once a test binary of its own:
//! one binary links the workspace once, where a binary per area linked it
//! for every area. Run one area with
//! `cargo test -p layerstack_conformance --test conformance <area>::`
//! (filters match substrings, so `flatten::` also runs
//! `layer_stack_flatten::`; `--exact` takes whole test names).
//!
//! The tests of every module share this process: a test must not change
//! process state (environment, working directory) and must name its
//! scratch files after its module. A test that needs a process of its own,
//! such as `usdc_robustness`, which installs a counting global allocator,
//! stays a file in `tests/` and so its own binary.

#[path = "../support/mod.rs"]
mod support;

mod affine;
mod ancestral_arcs;
mod animation_rates;
mod arc_cycles;
mod arc_target_sites;
mod asset_capture;
mod asset_expressions;
mod asset_previews;
mod authored_animation;
mod authored_sublayers;
mod authoring;
mod authoring_inspection;
mod authoring_targets;
mod base_material;
mod binding;
mod binding_authoring;
mod bounds;
mod camera;
mod camera_writeback;
mod change_evidence;
mod checked_motion;
mod checked_schema_reads;
mod checked_skel;
mod checked_subsets;
mod clip_authoring;
mod clip_consumers;
mod clip_offsets;
mod clip_sparse;
mod codeless_api;
mod collapsed_nodes;
mod collision_groups;
mod common_xform;
mod common_xform_api;
mod composition_errors;
mod composition_limits;
mod composition_misc;
mod composition_pcp;
mod composition_strict;
mod connectable_plug;
mod coord_sys;
mod coord_sys_authoring;
mod copy_spec;
mod custom_nodes;
mod dangling_targets;
mod default_prim;
mod define_prim;
mod dictionary_writing;
mod durations;
mod edit_target_maps;
mod edit_targets;
mod error_order;
mod explain_consistency;
mod export_interop;
mod expression_variables;
mod extents_hint;
mod flatten;
mod frustum;
mod fuzz_regressions;
mod generated_assets;
mod geometry_extent_coverage;
mod guarded_xform;
mod implied_classes;
mod incremental_edge_cases;
mod inner_loops;
mod instance_proxies;
mod instanceability;
mod instanced_references;
mod internal_arcs;
mod intrinsic_bounds;
mod labels;
mod layer_dangling_targets;
mod layer_field_edits;
mod layer_muting;
mod layer_offsets;
mod layer_save;
mod layer_stack_flatten;
mod lazy_usdc;
mod light_cache;
mod light_inputs;
mod light_parameters;
mod lights;
mod listop_combine_chain;
mod load_rules;
mod lux_connections;
mod lux_extents;
mod lux_helpers;
mod lux_nodes;
mod make_visible;
mod mass;
mod material_subsets;
mod membership_cache;
mod membership_index;
mod mesh_export;
mod metadata;
mod metadata_by_key;
mod missing_assets;
mod motion;
mod namespace_brackets;
mod namespace_coordinated;
mod namespace_deletes;
mod namespace_edits;
mod namespace_orders;
mod namespace_preparation;
mod namespace_unrepaired;
mod namespace_variants;
mod nested_arc_depth;
mod numeric_ingestion_bounds;
mod observers;
mod openusd_schemas;
mod particles;
mod path_expression;
mod peer_regressions;
mod physics_parse;
mod plugin_kinds;
mod point_curve_bounds;
mod point_instancer;
mod point_instancer_authoring;
mod point_instancer_behavior;
mod point_instancer_ids;
mod point_instancer_motion;
mod point_instancer_transforms;
mod point_motion;
mod prim_field_edits;
mod prim_index_graph;
mod prim_relocates;
mod primvars;
mod property_discovery;
mod property_fidelity;
mod property_metadata;
mod prototype_order;
mod proxy_edits;
mod pseudo_root_edits;
mod ray;
mod registered_metadata_ingestion;
mod registry_metadata;
mod relational_arcs;
mod relocates;
mod render_assets;
mod render_spec;
mod retained_queries;
mod retained_text_values;
mod runtime_prototypes;
mod sample_ranges;
mod schema_bases;
mod schema_compiler;
mod schema_metadata;
mod schema_views;
mod schemas;
mod session_layers;
mod shade_connections;
mod shade_node_def;
mod shade_sdr_metadata;
mod shader_nodes;
mod shading;
mod shading_defaults;
mod shading_ports;
mod skel_animation_samples;
mod skel_authoring;
mod skel_bake;
mod skel_binding_properties;
mod skel_blend_cache;
mod skel_blend_shapes;
mod skel_bounds;
mod skel_cache;
mod skel_deformed_normals;
mod skel_discovery;
mod skel_dual_quaternion;
mod skel_extents;
mod skel_influences;
mod skel_inputs;
mod skel_interleaved;
mod skel_normals;
mod skel_pose;
mod skel_prepared;
mod skel_samples;
mod skel_skinning;
mod specializes_placement;
mod specializes_propagation;
mod stage_api;
mod stage_controls;
mod stage_notices;
mod sublayer_offset_counts;
mod subset_authoring;
mod subset_family_type;
mod subsets;
mod target_explanations;
mod target_list_edits;
mod targeted_controls;
mod targeted_source_edits;
mod temporal_sparse;
mod text_arrays;
mod threaded_composition;
mod threaded_usdc_read;
mod time_values;
mod time_varying;
mod token_order;
mod transaction_schemas;
mod transform_queries;
mod typed_reads;
mod udim_tiles;
mod usda_byte_order_marks;
mod usda_corpus;
mod usda_field_order;
mod usda_namespace_depth;
mod usda_openusd_text;
mod usda_parity;
mod usda_streaming;
mod usdc_binary;
mod usdc_corpus;
mod usdc_encodings;
mod usdc_namespace_depth;
mod usdc_versions;
mod usdz_package;
mod usdz_packages;
mod value_clips;
mod value_matrix;
mod value_transforms;
mod value_types;
mod variant_branch_fields;
mod variant_children;
mod variant_names;
mod variant_orders;
mod variant_selection;
mod variant_set_names;
mod variant_specs;
mod variant_target_offsets;
mod volume;
mod xform;
mod xform_ops;

/// Every file in this directory is declared above: a file that is not is
/// never compiled, and its tests never run.
#[test]
fn every_module_file_is_declared() {
    let declared: Vec<&str> = include_str!("main.rs")
        .lines()
        .filter_map(|line| line.strip_prefix("mod ")?.strip_suffix(';'))
        .collect();
    let mut missing: Vec<String> = std::fs::read_dir("tests/conformance")
        .expect("the tests run from the crate directory")
        .map(|entry| entry.expect("lists the directory").file_name())
        .filter_map(|name| name.to_str()?.strip_suffix(".rs").map(str::to_owned))
        .filter(|name| name != "main" && !declared.contains(&name.as_str()))
        .collect();
    missing.sort();
    assert!(missing.is_empty(), "declare these modules: {missing:?}");
}
