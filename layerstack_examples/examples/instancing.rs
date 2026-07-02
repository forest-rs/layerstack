// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Instancing and inheritable primvars: a coral reef.
//!
//! This example demonstrates three key USD concepts:
//!
//! 1. **Instancing** (AOUSD Core §11): prims marked `instanceable = true`
//!    with a composition arc share their descendant structure. The composition
//!    engine strips local descendant opinions, so renderers can deduplicate
//!    geometry across thousands of instances.
//!
//! 2. **Instance-root properties survive**: only *descendant* opinions are
//!    stripped. Properties authored directly on the instance prim itself are
//!    kept, giving each instance its own transform, color, etc.
//!
//! 3. **Inheritable primvars**: a rendering convention (not a composition
//!    mechanic) where a property set on a parent "flows down" to descendants
//!    at render time. This lets you vary appearance per-instance without
//!    touching shared descendants. The primvar resolver is application-level
//!    code — layerstack is the composition kernel, not the rendering layer.
//!
//! ## Scene structure
//!
//! ```text
//! coral_asset.usd (LayerId 2)
//!   /Coral                    ← shared coral definition
//!     /branches               ← geometry, vertices = 2400
//!     /polyps                 ← geometry, vertices = 8000
//!
//! reef.usd (LayerId 1)
//!   /Reef
//!     /Coral_01 (instanceable, refs /Coral)
//!       primvars:displayColor = green     ← instance-root, SURVIVES
//!       primvars:bleachFactor = 0.0       ← instance-root, SURVIVES
//!     /Coral_02 (instanceable, refs /Coral)
//!       primvars:displayColor = pale      ← instance-root, SURVIVES
//!       primvars:bleachFactor = 0.85      ← instance-root, SURVIVES
//!     /Coral_03 (instanceable, refs /Coral)
//!       /branches: vertices = 999         ← descendant override, STRIPPED
//!     /Hero_Coral (NOT instanceable, refs /Coral)
//!       /branches: vertices = 6000        ← descendant override, KEPT
//!       primvars:displayColor = orange     ← also kept (not instanced)
//! ```
//!
//! After composition, all instanced corals share identical `/branches` and
//! `/polyps` geometry (vertices = 2400 and 8000 from the asset). The hero
//! coral is uninstanced and keeps its local overrides.
//!
//! The primvar resolver walks up the namespace to find `primvars:displayColor`
//! on each coral's `/branches` — it isn't authored there, but the instance
//! root's value flows down.

use layerstack::{
    InMemoryStore, Layer, LayerId, PathId, PrimSpec, Reference, Resolved, Stage, StageOptions,
    TokenId, Value,
};

// ---------------------------------------------------------------------------
// Primvar resolver — application-level code, not part of the composition
// kernel. Walks up the namespace hierarchy until it finds the field.
// ---------------------------------------------------------------------------

fn resolve_primvar(
    stage: &Stage,
    store: &InMemoryStore,
    mut prim: PathId,
    field: TokenId,
) -> Option<Value> {
    loop {
        if let Some(resolved) = stage.resolve_field(prim, field) {
            return Some(resolved.value);
        }
        let path = store.paths.resolve(prim);
        let parent = path.parent()?;
        prim = store.paths.lookup(&parent)?;
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn coral_instance(coral_ref: PathId, instanceable: bool) -> PrimSpec {
    PrimSpec::def()
        .with_reference(Reference::with_asset(
            LayerId(2),
            coral_ref,
            "coral_asset.usd",
        ))
        .with_instanceable(instanceable)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let mut store = InMemoryStore::default();

    // Field tokens.
    let f_vertices = store.tokens.intern("vertices");
    let f_display_color = store.tokens.intern("primvars:displayColor");
    let f_bleach = store.tokens.intern("primvars:bleachFactor");

    // Child name tokens.
    let branches_tok = store.tokens.intern("branches");
    let polyps_tok = store.tokens.intern("polyps");

    // Paths — asset layer.
    let coral = store.path("/Coral");
    let coral_branches = store.path("/Coral/branches");
    let coral_polyps = store.path("/Coral/polyps");

    // Paths — reef layer.
    let reef = store.path("/Reef");
    let c01 = store.path("/Reef/Coral_01");
    let c01_branches = store.path("/Reef/Coral_01/branches");
    let c02 = store.path("/Reef/Coral_02");
    let c02_branches = store.path("/Reef/Coral_02/branches");
    let c03 = store.path("/Reef/Coral_03");
    let c03_branches = store.path("/Reef/Coral_03/branches");
    let hero = store.path("/Reef/Hero_Coral");
    let hero_branches = store.path("/Reef/Hero_Coral/branches");

    // -----------------------------------------------------------------------
    // Asset layer: the shared coral definition.
    // -----------------------------------------------------------------------
    let mut asset = Layer::new(LayerId(2));

    asset.insert_prim(
        coral,
        PrimSpec::def()
            .with_children(vec![branches_tok, polyps_tok])
            .with_field(f_display_color, "gray"),
    );
    asset.insert_prim(coral_branches, PrimSpec::def().with_field(f_vertices, 2400));
    asset.insert_prim(coral_polyps, PrimSpec::def().with_field(f_vertices, 8000));

    store.insert_layer(asset);

    // -----------------------------------------------------------------------
    // Reef layer: instances and one hero coral.
    // -----------------------------------------------------------------------
    let mut reef_layer = Layer::new(LayerId(1));

    // /Reef parent (just a grouping prim).
    let c01_tok = store.tokens.intern("Coral_01");
    let c02_tok = store.tokens.intern("Coral_02");
    let c03_tok = store.tokens.intern("Coral_03");
    let hero_tok = store.tokens.intern("Hero_Coral");
    reef_layer.insert_prim(
        reef,
        PrimSpec::def().with_children(vec![c01_tok, c02_tok, c03_tok, hero_tok]),
    );

    // Coral_01: healthy green coral.
    reef_layer.insert_prim(
        c01,
        coral_instance(coral, true)
            .with_field(f_display_color, "green")
            .with_field(f_bleach, 0.0_f32),
    );

    // Coral_02: bleached coral.
    reef_layer.insert_prim(
        c02,
        coral_instance(coral, true)
            .with_field(f_display_color, "pale_white")
            .with_field(f_bleach, 0.85_f32),
    );

    // Coral_03: attempts a local override on branches (will be stripped).
    let c03_spec = coral_instance(coral, true);
    reef_layer.insert_prim(c03, c03_spec);

    reef_layer.insert_prim(c03_branches, PrimSpec::over().with_field(f_vertices, 999));

    // Hero_Coral: uninstanced for close-up — local overrides are kept.
    reef_layer.insert_prim(
        hero,
        coral_instance(coral, false).with_field(f_display_color, "orange"),
    );
    reef_layer.insert_prim(hero_branches, PrimSpec::over().with_field(f_vertices, 6000));

    store.insert_layer(reef_layer);

    // -----------------------------------------------------------------------
    // Compose.
    // -----------------------------------------------------------------------
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );

    // -----------------------------------------------------------------------
    // 1. Instancing: descendant opinions stripped vs. kept.
    // -----------------------------------------------------------------------
    println!("=== Coral Reef: Instancing ===\n");
    println!("Shared asset defines /Coral/branches with 2400 vertices.\n");

    for (label, branches_path, instanced) in [
        ("Coral_01 (instanced)", c01_branches, true),
        ("Coral_02 (instanced)", c02_branches, true),
        (
            "Coral_03 (instanced, had local override)",
            c03_branches,
            true,
        ),
        ("Hero_Coral (uninstanced)", hero_branches, false),
    ] {
        let verts = stage.resolve_field(branches_path, f_vertices);
        match &verts {
            Some(r) => {
                let layer = r.provenance.as_ref().map(|p| p.layer.0).unwrap_or(0);
                let source = if layer == 1 {
                    "reef.usd"
                } else {
                    "coral_asset.usd"
                };
                println!("  {label}: vertices = {} (from {source})", r.value);
            }
            None => println!("  {label}: vertices = <not resolved>"),
        }

        if instanced {
            assert_eq!(
                verts.as_ref().map(Resolved::value),
                Some(&Value::Int(2400)),
                "{label} should share asset geometry"
            );
        }
    }

    // Hero coral keeps its local override.
    assert_eq!(
        stage
            .resolve_field(hero_branches, f_vertices)
            .as_ref()
            .map(Resolved::value),
        Some(&Value::Int(6000)),
        "Hero coral should keep local override"
    );

    // -----------------------------------------------------------------------
    // 2. Instance-root properties survive.
    // -----------------------------------------------------------------------
    println!("\n=== Instance-Root Properties ===\n");
    println!("Properties on the instance prim itself are NOT stripped.\n");

    for (label, path) in [("Coral_01", c01), ("Coral_02", c02), ("Hero_Coral", hero)] {
        let color = stage.resolve_field(path, f_display_color);
        let bleach = stage.resolve_field(path, f_bleach);
        println!(
            "  {label}: color = {}, bleach = {}",
            color
                .as_ref()
                .map_or("none".into(), |r| format!("{}", r.value)),
            bleach
                .as_ref()
                .map_or("none".into(), |r| format!("{}", r.value)),
        );
    }

    // -----------------------------------------------------------------------
    // 3. Inheritable primvars (application-level resolver).
    // -----------------------------------------------------------------------
    println!("\n=== Inheritable Primvars ===\n");
    println!("primvars:displayColor is NOT authored on /branches.");
    println!("The primvar resolver walks up the namespace to find it.\n");

    // The composition engine doesn't know about primvar inheritance —
    // that's a rendering convention. This resolver is ~10 lines of
    // application code on top of layerstack's Stage API.

    for (label, branches_path) in [
        ("Coral_01/branches", c01_branches),
        ("Coral_02/branches", c02_branches),
        ("Coral_03/branches", c03_branches),
        ("Hero_Coral/branches", hero_branches),
    ] {
        // Direct resolve: not authored on branches.
        let direct = stage.resolve_field(branches_path, f_display_color);

        // Primvar resolve: walks up to the instance root.
        let inherited = resolve_primvar(&stage, &store, branches_path, f_display_color);

        println!(
            "  {label}: direct = {}, inherited = {}",
            direct.map_or("none".into(), |r| format!("{}", r.value)),
            inherited.as_ref().map_or("none".into(), |v| format!("{v}")),
        );
    }

    // Coral_03 has no local displayColor — it inherits the asset default.
    let c03_inherited = resolve_primvar(&stage, &store, c03_branches, f_display_color);
    assert_eq!(
        c03_inherited,
        Some(Value::string("gray")),
        "Coral_03 should fall back to asset default"
    );

    println!("\n  Coral_03 has no per-instance color, so the primvar resolver");
    println!("  walks up through /Reef/Coral_03 (nothing) to the referenced");
    println!("  asset's /Coral default: \"gray\".");
}
