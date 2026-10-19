// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Interactive editing with `LiveStage`.
//!
//! This example simulates an interactive scene editor: the user makes a
//! series of edits to layers, and `LiveStage` incrementally recomposes
//! only the affected prims. It demonstrates:
//!
//! 1. Initial composition with dependency tracking
//! 2. Opinion edits with `notify_layer_edit` / `notify_prim_edit`
//! 3. Scoped recomposition returning only affected prims
//! 4. Structural changes triggering a full rebuild
//! 5. Batch edits across multiple layers in a single recompose cycle

use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, Reference, Resolved, StageOptions,
    StageView,
};

fn main() {
    let mut store = InMemoryStore::default();

    let field_hp = store.tokens.intern("hitpoints");
    let field_speed = store.tokens.intern("speed");
    let field_color = store.tokens.intern("color");

    let hero = store.path("/Hero");
    let sword = store.path("/Hero/Sword");
    let shield = store.path("/Hero/Shield");
    let enemy = store.path("/Enemy");
    let power_gem = store.path("/PowerGem");

    let sword_token = store.tokens.intern("Sword");
    let shield_token = store.tokens.intern("Shield");

    // -----------------------------------------------------------------------
    // Layer 1 (base): defines the hero with children, and an enemy.
    // Layer 2 (items): defines the power gem (referenced by the hero).
    // -----------------------------------------------------------------------

    let mut base = Layer::new(LayerId(1));

    base.insert_prim(
        hero,
        PrimSpec::def()
            .with_children(vec![sword_token, shield_token])
            .with_field(field_hp, 100_i64)
            .with_field(field_speed, 5.0)
            // Hero references the power gem for bonus stats.
            .with_reference(Reference::new(LayerId(2), power_gem)),
    );
    base.insert_prim(sword, PrimSpec::def().with_field(field_color, "steel"));
    base.insert_prim(shield, PrimSpec::def().with_field(field_color, "bronze"));
    base.insert_prim(enemy, PrimSpec::def().with_field(field_hp, 50_i64));
    store.insert_layer(base);

    // Power gem layer: provides speed bonus to whoever references it.
    let mut items = Layer::new(LayerId(2));
    items.insert_prim(power_gem, PrimSpec::def().with_field(field_speed, 10.0));
    store.insert_layer(items);

    // -----------------------------------------------------------------------
    // Initial composition
    // -----------------------------------------------------------------------

    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

    println!("=== Initial State ===");
    print_hero(live.view(&store));
    print_enemy(live.view(&store));

    // -----------------------------------------------------------------------
    // Edit 1: The hero takes damage (opinion edit on base layer).
    // -----------------------------------------------------------------------

    println!("\n--- Edit: Hero takes 30 damage ---");
    {
        let layer = store.layer_mut(LayerId(1)).unwrap();
        let spec = layer.prim_mut(hero).unwrap();
        spec.set_field(field_hp, 70_i64);
    }

    live.notify_layer_edit(LayerId(1));
    let affected = live.recompose(&mut store);
    println!("Affected prims: {} (of 5 total)", affected.len());
    print_hero(live.view(&store));

    // -----------------------------------------------------------------------
    // Edit 2: Enchant the sword (single prim edit).
    // -----------------------------------------------------------------------

    println!("\n--- Edit: Enchant the sword ---");
    {
        let layer = store.layer_mut(LayerId(1)).unwrap();
        let spec = layer.prim_mut(sword).unwrap();
        spec.set_field(field_color, "glowing blue");
    }

    live.notify_prim_edit(sword);
    let affected = live.recompose(&mut store);
    println!("Affected prims: {} (only the sword)", affected.len());
    print_hero(live.view(&store));

    // -----------------------------------------------------------------------
    // Edit 3: Power gem gets an upgrade (referenced layer edit).
    // This should propagate through the reference arc to the hero.
    // -----------------------------------------------------------------------

    println!("\n--- Edit: Power gem upgrade (speed 10 → 20) ---");
    {
        let layer = store.layer_mut(LayerId(2)).unwrap();
        let spec = layer.prim_mut(power_gem).unwrap();
        spec.set_field(field_speed, 20.0);
    }

    live.notify_layer_edit(LayerId(2));
    let affected = live.recompose(&mut store);
    println!(
        "Affected prims: {} (gem + hero via reference arc)",
        affected.len()
    );
    // Note: hero's speed comes from the base layer (5.0) which is stronger
    // than the referenced gem layer. The reference is weaker.
    print_hero(live.view(&store));

    // -----------------------------------------------------------------------
    // Edit 4: Batch edit — damage the enemy AND change shield color.
    // Two notifications, one recompose.
    // -----------------------------------------------------------------------

    println!("\n--- Batch edit: enemy takes damage + shield upgrade ---");
    {
        let layer = store.layer_mut(LayerId(1)).unwrap();
        let e_spec = layer.prim_mut(enemy).unwrap();
        e_spec.set_field(field_hp, 25_i64);
        let s_spec = layer.prim_mut(shield).unwrap();
        s_spec.set_field(field_color, "mithril");
    }

    live.notify_layer_edit(LayerId(1));
    let affected = live.recompose(&mut store);
    println!(
        "Affected prims: {} (both updated in one pass)",
        affected.len()
    );
    print_hero(live.view(&store));
    print_enemy(live.view(&store));

    // -----------------------------------------------------------------------
    // Edit 5: Structural change — add a new prim.
    // -----------------------------------------------------------------------

    println!("\n--- Structural change: add /Treasure ---");
    let treasure = store.path("/Treasure");
    {
        let value = store.tokens.intern("value");
        let layer = store.layer_mut(LayerId(1)).unwrap();
        layer.insert_prim(treasure, PrimSpec::def().with_field(value, 500_i64));
    }

    live.notify_structural_change();
    let affected = live.recompose(&mut store);
    println!("Full rebuild: {} prims recomposed", affected.len());
    assert!(
        live.view(&store).prim("/Treasure").is_some(),
        "new prim should exist after structural rebuild"
    );
    println!("  /Treasure is now in the stage");

    // -----------------------------------------------------------------------
    // No-op: recompose with no changes should be instant.
    // -----------------------------------------------------------------------

    println!("\n--- No-op recompose ---");
    let affected = live.recompose(&mut store);
    println!("Affected prims: {} (nothing changed)", affected.len());
}

/// Reads `name`, a prim metadata field, of the prim at `path`.
fn field(view: StageView<'_>, path: &str, name: &str) -> String {
    view.prim(path)
        .and_then(|prim| prim.metadata(name))
        .and_then(Resolved::into_value)
        .map_or("???".into(), |r| format!("{}", r.value))
}

fn print_hero(view: StageView<'_>) {
    let hp = field(view, "/Hero", "hitpoints");
    let speed = field(view, "/Hero", "speed");
    let sw_color = field(view, "/Hero/Sword", "color");
    let sh_color = field(view, "/Hero/Shield", "color");
    println!("  /Hero        hp={hp}  speed={speed}");
    println!("  /Hero/Sword  color={sw_color}");
    println!("  /Hero/Shield color={sh_color}");
}

fn print_enemy(view: StageView<'_>) {
    let hp = field(view, "/Enemy", "hitpoints");
    println!("  /Enemy       hp={hp}");
}
