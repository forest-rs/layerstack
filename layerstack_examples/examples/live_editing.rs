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
    FieldValue, HashMap, InMemoryStore, Layer, LayerId, LiveStage, Path, PathId, PrimSpec,
    Reference, Specifier, StageOptions, Value,
};

fn p(store: &mut InMemoryStore, s: &str) -> PathId {
    store
        .paths
        .intern(Path::parse_absolute(s, &mut store.tokens).expect("valid path"))
}

fn main() {
    let mut store = InMemoryStore::default();

    let field_hp = store.tokens.intern("hitpoints");
    let field_speed = store.tokens.intern("speed");
    let field_color = store.tokens.intern("color");

    let hero = p(&mut store, "/Hero");
    let sword = p(&mut store, "/Hero/Sword");
    let shield = p(&mut store, "/Hero/Shield");
    let enemy = p(&mut store, "/Enemy");
    let power_gem = p(&mut store, "/PowerGem");

    let sword_token = store.tokens.intern("Sword");
    let shield_token = store.tokens.intern("Shield");

    // -----------------------------------------------------------------------
    // Layer 1 (base): defines the hero with children, and an enemy.
    // Layer 2 (items): defines the power gem (referenced by the hero).
    // -----------------------------------------------------------------------

    let mut base = Layer {
        id: LayerId(1),
        sublayers: vec![],
        prims: HashMap::new(),
    };

    let mut hero_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        authored_children: vec![sword_token, shield_token],
        ..PrimSpec::default()
    };
    hero_spec
        .fields
        .insert(field_hp, FieldValue::Value(Value::Int(100)));
    hero_spec
        .fields
        .insert(field_speed, FieldValue::Value(Value::Float(5.0)));
    // Hero references the power gem for bonus stats.
    hero_spec.references.append.push(Reference {
        layer: LayerId(2),
        prim_path: power_gem,
        asset: None,
    });
    base.prims.insert(hero, hero_spec);

    let mut sword_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        ..PrimSpec::default()
    };
    sword_spec.fields.insert(
        field_color,
        FieldValue::Value(Value::String("steel".into())),
    );
    base.prims.insert(sword, sword_spec);

    let mut shield_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        ..PrimSpec::default()
    };
    shield_spec.fields.insert(
        field_color,
        FieldValue::Value(Value::String("bronze".into())),
    );
    base.prims.insert(shield, shield_spec);

    let mut enemy_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        ..PrimSpec::default()
    };
    enemy_spec
        .fields
        .insert(field_hp, FieldValue::Value(Value::Int(50)));
    base.prims.insert(enemy, enemy_spec);
    store.insert_layer(base);

    // Power gem layer: provides speed bonus to whoever references it.
    let mut items = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut gem_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        ..PrimSpec::default()
    };
    gem_spec
        .fields
        .insert(field_speed, FieldValue::Value(Value::Float(10.0)));
    items.prims.insert(power_gem, gem_spec);
    store.insert_layer(items);

    // -----------------------------------------------------------------------
    // Initial composition
    // -----------------------------------------------------------------------

    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

    println!("=== Initial State ===");
    print_hero(
        &live,
        hero,
        sword,
        shield,
        field_hp,
        field_speed,
        field_color,
    );
    print_enemy(&live, enemy, field_hp);

    // -----------------------------------------------------------------------
    // Edit 1: The hero takes damage (opinion edit on base layer).
    // -----------------------------------------------------------------------

    println!("\n--- Edit: Hero takes 30 damage ---");
    {
        let layer = store.layers.get_mut(&LayerId(1)).unwrap();
        let spec = layer.prims.get_mut(&hero).unwrap();
        spec.fields
            .insert(field_hp, FieldValue::Value(Value::Int(70)));
    }

    live.notify_layer_edit(LayerId(1));
    let affected = live.recompose(&mut store);
    println!("Affected prims: {} (of 5 total)", affected.len());
    print_hero(
        &live,
        hero,
        sword,
        shield,
        field_hp,
        field_speed,
        field_color,
    );

    // -----------------------------------------------------------------------
    // Edit 2: Enchant the sword (single prim edit).
    // -----------------------------------------------------------------------

    println!("\n--- Edit: Enchant the sword ---");
    {
        let layer = store.layers.get_mut(&LayerId(1)).unwrap();
        let spec = layer.prims.get_mut(&sword).unwrap();
        spec.fields.insert(
            field_color,
            FieldValue::Value(Value::String("glowing blue".into())),
        );
    }

    live.notify_prim_edit(sword);
    let affected = live.recompose(&mut store);
    println!("Affected prims: {} (only the sword)", affected.len());
    print_hero(
        &live,
        hero,
        sword,
        shield,
        field_hp,
        field_speed,
        field_color,
    );

    // -----------------------------------------------------------------------
    // Edit 3: Power gem gets an upgrade (referenced layer edit).
    // This should propagate through the reference arc to the hero.
    // -----------------------------------------------------------------------

    println!("\n--- Edit: Power gem upgrade (speed 10 → 20) ---");
    {
        let layer = store.layers.get_mut(&LayerId(2)).unwrap();
        let spec = layer.prims.get_mut(&power_gem).unwrap();
        spec.fields
            .insert(field_speed, FieldValue::Value(Value::Float(20.0)));
    }

    live.notify_layer_edit(LayerId(2));
    let affected = live.recompose(&mut store);
    println!(
        "Affected prims: {} (gem + hero via reference arc)",
        affected.len()
    );
    // Note: hero's speed comes from the base layer (5.0) which is stronger
    // than the referenced gem layer. The reference is weaker.
    print_hero(
        &live,
        hero,
        sword,
        shield,
        field_hp,
        field_speed,
        field_color,
    );

    // -----------------------------------------------------------------------
    // Edit 4: Batch edit — damage the enemy AND change shield color.
    // Two notifications, one recompose.
    // -----------------------------------------------------------------------

    println!("\n--- Batch edit: enemy takes damage + shield upgrade ---");
    {
        let layer = store.layers.get_mut(&LayerId(1)).unwrap();
        let e_spec = layer.prims.get_mut(&enemy).unwrap();
        e_spec
            .fields
            .insert(field_hp, FieldValue::Value(Value::Int(25)));
        let s_spec = layer.prims.get_mut(&shield).unwrap();
        s_spec.fields.insert(
            field_color,
            FieldValue::Value(Value::String("mithril".into())),
        );
    }

    live.notify_layer_edit(LayerId(1));
    let affected = live.recompose(&mut store);
    println!(
        "Affected prims: {} (both updated in one pass)",
        affected.len()
    );
    print_hero(
        &live,
        hero,
        sword,
        shield,
        field_hp,
        field_speed,
        field_color,
    );
    print_enemy(&live, enemy, field_hp);

    // -----------------------------------------------------------------------
    // Edit 5: Structural change — add a new prim.
    // -----------------------------------------------------------------------

    println!("\n--- Structural change: add /Treasure ---");
    let treasure = p(&mut store, "/Treasure");
    {
        let layer = store.layers.get_mut(&LayerId(1)).unwrap();
        let mut t_spec = PrimSpec {
            specifier: Some(Specifier::Def),
            ..PrimSpec::default()
        };
        t_spec.fields.insert(
            store.tokens.intern("value"),
            FieldValue::Value(Value::Int(500)),
        );
        layer.prims.insert(treasure, t_spec);
    }

    live.notify_structural_change();
    let affected = live.recompose(&mut store);
    println!("Full rebuild: {} prims recomposed", affected.len());
    assert!(
        live.stage().has_prim(treasure),
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

fn print_hero(
    live: &LiveStage,
    hero: PathId,
    sword: PathId,
    shield: PathId,
    field_hp: layerstack::TokenId,
    field_speed: layerstack::TokenId,
    field_color: layerstack::TokenId,
) {
    let stage = live.stage();
    let hp = stage
        .resolve_field(hero, field_hp)
        .map(|r| format!("{:?}", r.value))
        .unwrap_or_else(|| "???".to_string());
    let speed = stage
        .resolve_field(hero, field_speed)
        .map(|r| format!("{:?}", r.value))
        .unwrap_or_else(|| "???".to_string());
    let sw_color = stage
        .resolve_field(sword, field_color)
        .map(|r| format!("{:?}", r.value))
        .unwrap_or_else(|| "???".to_string());
    let sh_color = stage
        .resolve_field(shield, field_color)
        .map(|r| format!("{:?}", r.value))
        .unwrap_or_else(|| "???".to_string());
    println!("  /Hero        hp={hp}  speed={speed}");
    println!("  /Hero/Sword  color={sw_color}");
    println!("  /Hero/Shield color={sh_color}");
}

fn print_enemy(live: &LiveStage, enemy: PathId, field_hp: layerstack::TokenId) {
    let stage = live.stage();
    let hp = stage
        .resolve_field(enemy, field_hp)
        .map(|r| format!("{:?}", r.value))
        .unwrap_or_else(|| "???".to_string());
    println!("  /Enemy       hp={hp}");
}
