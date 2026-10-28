// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A generated parts library: the shape of a parametric asset library whose
//! hot question is which collections include a prim.
//!
//! [`LibraryScene::generate`] builds, from a seed and a [`LibraryShape`]:
//!
//! - one library root, `/Library`, with groups (`Stones`, `Seeds`,
//!   `Plants`, `Terrain`), families in each group, and leaf prims in each
//!   family (`/Library/Stones/Granite/Granite_000`);
//! - slot prims (`/Slots/Bed_00/Slot_00`) holding single-target
//!   relationships into the library;
//! - `CollectionAPI` collections on `/Library`, on each group and on
//!   `/Sets`, with includes, excludes, every `expansionRule` (some
//!   `expandPrims` includes name a family, an ancestor of leaves), one
//!   `includeRoot`, one collection included by another, and two decided by
//!   their `membershipExpression`. Includes are list-edited across three
//!   sublayers: appended in the base and sets layers, some deleted in the
//!   stronger overrides layer, which also authors some excludes;
//! - collection-based material bindings (`material:binding:collection:*`,
//!   with both `bindMaterialAs` strengths) and some direct bindings;
//! - large `token[]` attributes (`tags`) on every leaf, slot and field
//!   patch (`/Field/Row_00/Patch_000`);
//! - optionally ([`LibraryShape::dangling`]), slot relationships and
//!   collection includes and excludes that name nothing: a leaf that does
//!   not exist, or a property no leaf has.
//!
//! [`LibraryScene::write`] serializes the same layers as USDA or USDC
//! (`library.usda` sublayering `library_overrides`, `library_sets` and
//! `library_base`, strongest first). Generation is deterministic: the same
//! shape and seed give byte-identical files. Nothing is checked in; tests
//! and benches write it to a temporary directory.
//!
//! Spec: AOUSD Core §10.3.1 (sublayers), §6.6.3 (list operations), §15
//! (collections); OpenUSD's `UsdCollectionAPI` and `UsdShadeMaterialBindingAPI`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use layerstack::Specifier;
use layerstack_usda::writer::{
    Attribute, Document, LayerOffset, ListOp, Metadatum, Prim, Relationship, SubLayer, Value,
};

/// How big the generated library is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LibraryShape {
    /// Seed of every random choice.
    pub seed: u64,
    /// Families in each of the four groups.
    pub families: usize,
    /// Leaf prims in each family.
    pub leaves: usize,
    /// Slot beds, each holding [`Self::slots_per_bed`] slots.
    pub beds: usize,
    /// Slots in each bed.
    pub slots_per_bed: usize,
    /// Relationships from slots into the library, in all.
    pub relationships: usize,
    /// `CollectionAPI` collections, at least eight.
    pub collections: usize,
    /// Leaves each rule-map collection includes from each of the two
    /// layers that append includes.
    pub includes_per_layer: usize,
    /// Field rows, each holding [`Self::patches_per_row`] patches.
    pub rows: usize,
    /// Patches in each row.
    pub patches_per_row: usize,
    /// Average `tags` array length.
    pub average_tags: usize,
    /// Slot relationships whose target names nothing, spread evenly over
    /// [`Self::relationships`]: alternately a leaf that does not exist
    /// (`Granite_042_lost`) and a property no leaf has (`Granite_042.lost`).
    /// When not zero, each rule-map collection also includes one leaf that
    /// does not exist and excludes another. The choice is by position, not
    /// by the seed, so the rest of the library is the same as without them.
    pub dangling: usize,
}

impl LibraryShape {
    /// About 4,000 leaves, 400 slots with 15,000 relationships, 40
    /// collections, and 10,000 prims with about 800,000 array cells.
    pub const FULL: Self = Self {
        seed: 0x5eed_0000_0000_0001,
        families: 10,
        leaves: 100,
        beds: 20,
        slots_per_bed: 20,
        relationships: 15_000,
        collections: 40,
        includes_per_layer: 60,
        rows: 56,
        patches_per_row: 100,
        average_tags: 80,
        dangling: 0,
    };

    /// A tenth of [`Self::FULL`] in each population, for debug-build tests.
    pub const SMALL: Self = Self {
        seed: Self::FULL.seed,
        families: 4,
        leaves: 25,
        beds: 4,
        slots_per_bed: 10,
        relationships: 1_500,
        collections: 16,
        includes_per_layer: 12,
        rows: 8,
        patches_per_row: 70,
        average_tags: 8,
        dangling: 0,
    };
}

/// What [`LibraryScene::generate`] made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibraryCounts {
    /// Leaf prims under `/Library`.
    pub leaves: usize,
    /// Slot prims under `/Slots`.
    pub slots: usize,
    /// Relationships from slots into the library.
    pub relationships: usize,
    /// `CollectionAPI` instances.
    pub collections: usize,
    /// Of those, decided by their `membershipExpression`.
    pub expression_collections: usize,
    /// Include targets authored, over all layers (appends, before deletes).
    pub include_targets: usize,
    /// Include targets deleted in the overrides layer.
    pub deleted_includes: usize,
    /// Exclude targets authored.
    pub exclude_targets: usize,
    /// `material:binding:collection:*` relationships.
    pub collection_bindings: usize,
    /// Direct `material:binding` relationships.
    pub direct_bindings: usize,
    /// Prims with a `tags` array.
    pub array_prims: usize,
    /// `tags` cells, in all.
    pub array_cells: usize,
    /// Prims defined, the pseudo-root left out.
    pub prims: usize,
    /// Slot relationships whose target names no prim
    /// ([`LibraryShape::dangling`]).
    pub dangling_prims: usize,
    /// Slot relationships whose target names no property.
    pub dangling_properties: usize,
    /// Collection includes and excludes that name no prim.
    pub dangling_collection_paths: usize,
}

/// Serialization of a [`LibraryScene`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryFormat {
    /// USDA text.
    Usda,
    /// USDC crate files.
    Usdc,
}

impl LibraryFormat {
    /// The file extension.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Usda => "usda",
            Self::Usdc => "usdc",
        }
    }
}

/// Files written by [`LibraryScene::write`].
#[derive(Clone, Debug)]
pub struct WrittenLibrary {
    /// The root layer, which sublayers the others.
    pub root: PathBuf,
    /// Every file written, root first, with its size in bytes.
    pub files: Vec<(PathBuf, u64)>,
}

impl WrittenLibrary {
    /// Bytes written, in all.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|(_, size)| size).sum()
    }

    /// Removes every file written.
    pub fn remove(&self) {
        for (path, _) in &self.files {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The generated library: its three sublayers, weakest last.
#[derive(Clone, Debug)]
pub struct LibraryScene {
    /// `library_overrides`, `library_sets`, `library_base`: strongest first.
    pub layers: [(&'static str, Document); 3],
    /// What was generated.
    pub counts: LibraryCounts,
    /// The collections, as `(owner prim path, collection name)`.
    pub collections: Vec<(String, String)>,
}

const GROUPS: [(&str, [&str; 10]); 4] = [
    (
        "Stones",
        [
            "Granite", "Basalt", "Slate", "Marble", "Quartz", "Flint", "Shale", "Chalk", "Jasper",
            "Obsidian",
        ],
    ),
    (
        "Seeds",
        [
            "Acorn",
            "Maple",
            "Pine",
            "Sunflower",
            "Poppy",
            "Clover",
            "Barley",
            "Rye",
            "Millet",
            "Flax",
        ],
    ),
    (
        "Plants",
        [
            "Fern", "Moss", "Ivy", "Reed", "Sedge", "Thistle", "Heather", "Bracken", "Yarrow",
            "Sorrel",
        ],
    ),
    (
        "Terrain",
        [
            "Dune", "Ridge", "Scree", "Gravel", "Loam", "Clay", "Silt", "Peat", "Marsh", "Mesa",
        ],
    ),
];

const LOOKS: [&str; 8] = [
    "Sandstone",
    "Lichen",
    "Bark",
    "Soil",
    "Leaf",
    "Petal",
    "Water",
    "Ash",
];

const TAGS: [&str; 48] = [
    "rough",
    "smooth",
    "wet",
    "dry",
    "mossy",
    "cracked",
    "polished",
    "weathered",
    "small",
    "large",
    "tall",
    "short",
    "pale",
    "dark",
    "bright",
    "dull",
    "round",
    "flat",
    "sharp",
    "soft",
    "hard",
    "heavy",
    "light",
    "dense",
    "porous",
    "glossy",
    "matte",
    "striped",
    "speckled",
    "banded",
    "young",
    "old",
    "dormant",
    "flowering",
    "fruiting",
    "evergreen",
    "deciduous",
    "coastal",
    "alpine",
    "desert",
    "wetland",
    "forest",
    "meadow",
    "riverbed",
    "volcanic",
    "glacial",
    "sunny",
    "shaded",
];

/// `SplitMix64`: small, seedable and the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).expect("small")).expect("small")
    }

    /// `count` distinct picks of `items`, in pick order.
    fn pick<'a, T>(&mut self, items: &'a [T], count: usize) -> Vec<&'a T> {
        let mut chosen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        let count = count.min(items.len());
        while out.len() < count {
            let at = self.below(items.len());
            if chosen.insert(at) {
                out.push(&items[at]);
            }
        }
        out
    }
}

fn family_name(group: usize, family: usize) -> String {
    let base = GROUPS[group].1[family % 10];
    if family < 10 {
        base.to_string()
    } else {
        format!("{base}{}", family / 10)
    }
}

/// The `over` prim at `path`, made with its ancestors when missing.
fn over_at<'a>(roots: &'a mut Vec<Prim>, path: &str) -> &'a mut Prim {
    let mut names = path.trim_start_matches('/').split('/');
    let first = names.next().expect("a root prim");
    let mut at = find_or_over(roots, first);
    for name in names {
        at = find_or_over(&mut at.children, name);
    }
    at
}

fn find_or_over<'a>(prims: &'a mut Vec<Prim>, name: &str) -> &'a mut Prim {
    let index = match prims.iter().position(|p| p.name == name) {
        Some(index) => index,
        None => {
            prims.push(Prim::new(Specifier::Over, None, name));
            prims.len() - 1
        }
    };
    &mut prims[index]
}

fn targets(appended: Vec<String>) -> ListOp<String> {
    ListOp {
        appended,
        ..ListOp::default()
    }
}

fn rel(name: String, targets: ListOp<String>) -> Relationship {
    Relationship {
        name,
        custom: false,
        varying: false,
        targets: Some(targets),
        default: None,
        metadata: Vec::new(),
    }
}

fn prepend_api(prim: &mut Prim, schemas: Vec<String>) {
    prim.metadata.push(Metadatum::new(
        "apiSchemas",
        Value::TokenListOp(ListOp::prepend(schemas)),
    ));
}

impl LibraryScene {
    /// Generates the library for `shape`.
    ///
    /// # Panics
    ///
    /// When `shape` has fewer than eight collections or no leaves.
    pub fn generate(shape: &LibraryShape) -> Self {
        assert!(
            shape.collections >= 8 && shape.leaves > 0 && shape.families > 0,
            "a library needs eight collections and some leaves"
        );
        let mut rng = Rng(shape.seed);
        let mut counts = LibraryCounts::default();
        let tags = |rng: &mut Rng, counts: &mut LibraryCounts| {
            let len = shape.average_tags / 2 + rng.below(shape.average_tags + 1);
            counts.array_prims += 1;
            counts.array_cells += len;
            Attribute::new(
                "tags",
                "token[]",
                Value::TokenArray(
                    (0..len)
                        .map(|_| TAGS[rng.below(TAGS.len())].to_string())
                        .collect(),
                ),
            )
        };

        // Base layer: every `def`, the library's first includes, direct
        // bindings.
        let mut library = Prim::def("Scope", "Library");
        library
            .metadata
            .push(Metadatum::new("kind", Value::Token("assembly".into())));
        let mut leaves: Vec<String> = Vec::new();
        let mut families: Vec<(usize, String)> = Vec::new();
        for (g, (group_name, _)) in GROUPS.iter().enumerate() {
            let mut group = Prim::def("Scope", *group_name);
            group
                .metadata
                .push(Metadatum::new("kind", Value::Token("group".into())));
            for f in 0..shape.families {
                let name = family_name(g, f);
                let mut family = Prim::def("Scope", name.clone());
                families.push((g, format!("/Library/{group_name}/{name}")));
                for l in 0..shape.leaves {
                    let leaf_name = format!("{name}_{l:03}");
                    let mut leaf = Prim::def("Xform", leaf_name.clone());
                    leaf.metadata
                        .push(Metadatum::new("kind", Value::Token("component".into())));
                    leaf.push_property(tags(&mut rng, &mut counts));
                    if l % 50 == 7 {
                        prepend_api(&mut leaf, vec!["MaterialBindingAPI".into()]);
                        leaf.push_property(Relationship::new(
                            "material:binding",
                            format!("/Looks/{}", LOOKS[(g + l) % LOOKS.len()]),
                        ));
                        counts.direct_bindings += 1;
                    }
                    family.children.push(leaf);
                    leaves.push(format!("/Library/{group_name}/{name}/{leaf_name}"));
                    counts.leaves += 1;
                }
                group.children.push(family);
            }
            library.children.push(group);
        }

        let mut looks = Prim::def("Scope", "Looks");
        for look in LOOKS {
            looks.children.push(Prim::def("Material", look));
        }

        let mut slots = Prim::def("Scope", "Slots");
        let slot_count = shape.beds * shape.slots_per_bed;
        // Every `stride`th relationship dangles, from the middle of the first.
        let stride = (shape.dangling > 0).then(|| (shape.relationships / shape.dangling).max(1));
        for b in 0..shape.beds {
            let mut bed = Prim::def("Xform", format!("Bed_{b:02}"));
            for s in 0..shape.slots_per_bed {
                let index = b * shape.slots_per_bed + s;
                let mut slot = Prim::def("Xform", format!("Slot_{s:02}"));
                let mine = shape.relationships / slot_count
                    + usize::from(index < shape.relationships % slot_count);
                for k in 0..mine {
                    let mut target = leaves[rng.below(leaves.len())].clone();
                    let at = counts.relationships;
                    if let Some(stride) = stride
                        && at % stride == stride / 2
                        && counts.dangling_prims + counts.dangling_properties < shape.dangling
                    {
                        if (at / stride) % 2 == 0 {
                            target.push_str("_lost");
                            counts.dangling_prims += 1;
                        } else {
                            target.push_str(".lost");
                            counts.dangling_properties += 1;
                        }
                    }
                    slot.push_property(Relationship::new(format!("part_{k:03}"), target));
                    counts.relationships += 1;
                }
                slot.push_property(tags(&mut rng, &mut counts));
                bed.children.push(slot);
                counts.slots += 1;
            }
            slots.children.push(bed);
        }

        let mut field = Prim::def("Scope", "Field");
        for r in 0..shape.rows {
            let mut row = Prim::def("Xform", format!("Row_{r:02}"));
            for p in 0..shape.patches_per_row {
                let mut patch = Prim::def("Xform", format!("Patch_{p:03}"));
                patch.push_property(tags(&mut rng, &mut counts));
                row.children.push(patch);
            }
            field.children.push(row);
        }

        let mut sets = Prim::def("Scope", "Sets");

        // Collections: who owns each, and what it says in each layer.
        let n = shape.collections;
        let expression_a = n - 1;
        let expression_b = n - 2;
        let nested = n - 3;
        let rooted = n - 4;
        let owner = |i: usize| -> String {
            if i < n * 2 / 5 {
                "/Library".into()
            } else if i < n * 4 / 5 {
                format!("/Library/{}", GROUPS[(i - n * 2 / 5) % GROUPS.len()].0)
            } else {
                "/Sets".into()
            }
        };
        let collections: Vec<(String, String)> =
            (0..n).map(|i| (owner(i), format!("set{i:02}"))).collect();
        let mut sets_layer: Vec<Prim> = Vec::new();
        let mut overrides_layer: Vec<Prim> = Vec::new();
        let mut applied: Vec<(String, Vec<String>)> = Vec::new();
        let mut base_rels: Vec<(String, Relationship)> = Vec::new();
        let mut base_attrs: Vec<(String, Attribute)> = Vec::new();
        for (i, (owner, name)) in collections.iter().enumerate() {
            match applied.iter_mut().find(|(o, _)| o == owner) {
                Some((_, list)) => list.push(format!("CollectionAPI:{name}")),
                None => applied.push((owner.clone(), vec![format!("CollectionAPI:{name}")])),
            }
            let property = |base: &str| format!("collection:{name}:{base}");
            if i == expression_a {
                let first = &GROUPS[2].1[0];
                let stone = &GROUPS[0].1[0];
                base_attrs.push((
                    owner.clone(),
                    Attribute::new(
                        property("membershipExpression"),
                        "pathExpression",
                        Value::PathExpression(format!("/Library/Plants/{first}/* + //{stone}_00*")),
                    )
                    .uniform(),
                ));
                counts.expression_collections += 1;
                continue;
            }
            if i == expression_b {
                let acorn = &GROUPS[1].1[0];
                base_attrs.push((
                    owner.clone(),
                    Attribute::new(
                        property("membershipExpression"),
                        "pathExpression",
                        Value::PathExpression(format!(
                            "/Library/Seeds//{{isa:Xform}} - /Library/Seeds/{acorn}//"
                        )),
                    )
                    .uniform(),
                ));
                counts.expression_collections += 1;
                continue;
            }
            let rule = match i % 8 {
                0 => Some("explicitOnly"),
                1 => Some("expandPrimsAndProperties"),
                2 => Some("expandPrims"),
                _ => None,
            };
            if let Some(rule) = rule {
                base_attrs.push((
                    owner.clone(),
                    Attribute::new(
                        property("expansionRule"),
                        "token",
                        Value::Token(rule.into()),
                    )
                    .uniform(),
                ));
            }
            // A group's collections draw from that group's leaves.
            let pool: Vec<&String> = match owner.strip_prefix("/Library/") {
                Some(group) => {
                    let prefix = format!("/Library/{group}/");
                    leaves.iter().filter(|l| l.starts_with(&prefix)).collect()
                }
                None => leaves.iter().collect(),
            };
            let pool: Vec<String> = pool.into_iter().cloned().collect();
            let family_pool: Vec<&String> = families
                .iter()
                .filter(|(_, f)| pool.iter().any(|l| l.starts_with(&format!("{f}/"))))
                .map(|(_, f)| f)
                .collect();
            let mut base_includes: Vec<String> = rng
                .pick(&pool, shape.includes_per_layer)
                .into_iter()
                .cloned()
                .collect();
            if i % 3 == 0 {
                base_includes.push(family_pool[rng.below(family_pool.len())].clone());
            }
            if i == nested {
                base_includes.push(format!(
                    "{}.collection:{}",
                    collections[1].0, collections[1].1
                ));
            }
            let mut sets_includes: Vec<String> = rng
                .pick(&pool, shape.includes_per_layer)
                .into_iter()
                .filter(|l| !base_includes.contains(l))
                .cloned()
                .collect();
            if i % 4 == 1 {
                let family = family_pool[rng.below(family_pool.len())].clone();
                if !base_includes.contains(&family) {
                    sets_includes.push(family);
                }
            }
            counts.include_targets += base_includes.len() + sets_includes.len();
            // The overrides layer deletes some of each layer's includes.
            let mut deleted: Vec<String> = rng
                .pick(&base_includes, 3)
                .into_iter()
                .filter(|p| !p.contains(".collection:"))
                .cloned()
                .collect();
            deleted.extend(rng.pick(&sets_includes, 2).into_iter().cloned());
            counts.deleted_includes += deleted.len();
            // Excludes: leaves of included families first, so they matter.
            let included_families: Vec<&String> = base_includes
                .iter()
                .chain(&sets_includes)
                .filter(|p| families.iter().any(|(_, f)| f == *p))
                .collect();
            let mut excludes: Vec<String> = Vec::new();
            for family in &included_families {
                let prefix = format!("{family}/");
                let inside: Vec<&String> = pool.iter().filter(|l| l.starts_with(&prefix)).collect();
                excludes.extend(rng.pick(&inside, 2).into_iter().map(|l| (*l).clone()));
            }
            excludes.extend(rng.pick(&pool, 2).into_iter().cloned());
            counts.exclude_targets += excludes.len();
            if shape.dangling > 0 {
                // After every pick, so the seed's choices stay the same.
                let lost = |path: &String| format!("{path}_lost");
                base_includes.push(lost(&pool[i % pool.len()]));
                excludes.push(lost(&pool[(i + 1) % pool.len()]));
                counts.include_targets += 1;
                counts.exclude_targets += 1;
                counts.dangling_collection_paths += 2;
            }
            if i == rooted {
                base_attrs.push((
                    owner.clone(),
                    Attribute::new(property("includeRoot"), "bool", Value::Bool(true)).uniform(),
                ));
                excludes.push("/Field".into());
                excludes.push("/Slots".into());
                counts.exclude_targets += 2;
            }
            base_rels.push((
                owner.clone(),
                rel(property("includes"), targets(base_includes)),
            ));
            let at = over_at(&mut sets_layer, owner);
            if !sets_includes.is_empty() {
                at.push_property(rel(property("includes"), targets(sets_includes)));
            }
            // Half the collections take their excludes from the sets layer,
            // half from the stronger overrides layer.
            let (strong_excludes, weak_excludes) = if i % 2 == 0 {
                (excludes, Vec::new())
            } else {
                (Vec::new(), excludes)
            };
            if !weak_excludes.is_empty() {
                at.push_property(rel(property("excludes"), targets(weak_excludes)));
            }
            let at = over_at(&mut overrides_layer, owner);
            at.push_property(rel(
                property("includes"),
                ListOp {
                    deleted,
                    ..ListOp::default()
                },
            ));
            if !strong_excludes.is_empty() {
                at.push_property(rel(property("excludes"), targets(strong_excludes)));
            }
        }

        // Collection bindings, in the sets layer: the library's collections
        // and each group's first.
        let mut bound_groups = std::collections::BTreeSet::new();
        for (i, (owner, name)) in collections.iter().enumerate() {
            let first_of_group = owner.starts_with("/Library/") && bound_groups.insert(owner);
            if !(owner == "/Library" || first_of_group) {
                continue;
            }
            let mut binding = rel(
                format!("material:binding:collection:{name}"),
                ListOp::explicit(vec![
                    format!("{owner}.collection:{name}"),
                    format!("/Looks/{}", LOOKS[i % LOOKS.len()]),
                ]),
            );
            if i % 3 == 0 {
                binding.metadata.push(Metadatum::new(
                    "bindMaterialAs",
                    Value::Token("strongerThanDescendants".into()),
                ));
            }
            over_at(&mut sets_layer, owner).push_property(binding);
            counts.collection_bindings += 1;
        }

        // Owners in the base layer: their applied schemas and first opinions.
        for (owner, mut schemas) in applied {
            let prim = base_owner(&mut library, &mut sets, &owner);
            if owner.starts_with("/Library") {
                schemas.push("MaterialBindingAPI".into());
            }
            prepend_api(prim, schemas);
            if owner.starts_with("/Library/") {
                let g = GROUPS
                    .iter()
                    .position(|(name, _)| owner.ends_with(name))
                    .expect("a group");
                prim.push_property(Relationship::new(
                    "material:binding",
                    format!("/Looks/{}", LOOKS[(g * 3) % LOOKS.len()]),
                ));
                counts.direct_bindings += 1;
            }
        }
        for (owner, attribute) in base_attrs {
            base_owner(&mut library, &mut sets, &owner).push_property(attribute);
        }
        for (owner, relationship) in base_rels {
            base_owner(&mut library, &mut sets, &owner).push_property(relationship);
        }

        let mut base = Document::new();
        base.prims = vec![library, looks, slots, field, sets];
        counts.collections = collections.len();
        counts.prims = count_prims(&base.prims);
        let mut sets_document = Document::new();
        sets_document.prims = sets_layer;
        let mut overrides_document = Document::new();
        overrides_document.prims = overrides_layer;
        Self {
            layers: [
                ("library_overrides", overrides_document),
                ("library_sets", sets_document),
                ("library_base", base),
            ],
            counts,
            collections,
        }
    }

    /// Writes the root layer and its sublayers into `dir`, as `format`.
    ///
    /// # Errors
    ///
    /// When a layer does not serialize or a file cannot be written.
    pub fn write(&self, dir: &Path, format: LibraryFormat) -> std::io::Result<WrittenLibrary> {
        std::fs::create_dir_all(dir)?;
        let mut files = Vec::new();
        for (name, bytes) in self.encode(format)? {
            let path = dir.join(name);
            std::fs::write(&path, &bytes)?;
            files.push((path, bytes.len() as u64));
        }
        Ok(WrittenLibrary {
            root: files[0].0.clone(),
            files,
        })
    }

    /// The root layer and its sublayers as `format`, each with the file
    /// name [`Self::write`] gives it, root first, without touching the
    /// filesystem: a test that needs only the bytes shares no files with
    /// tests running at once in other processes.
    ///
    /// # Errors
    ///
    /// When a layer does not serialize.
    pub fn encode(&self, format: LibraryFormat) -> std::io::Result<Vec<(String, Vec<u8>)>> {
        let extension = format.extension();
        let mut root = Document::new();
        root.sublayers = self
            .layers
            .iter()
            .map(|(stem, _)| SubLayer {
                asset: format!("./{stem}.{extension}"),
                offset: LayerOffset::IDENTITY,
            })
            .collect();
        let documents = core::iter::once(("library", &root))
            .chain(self.layers.iter().map(|(stem, document)| (*stem, document)));
        documents
            .map(|(stem, document)| {
                let bytes = match format {
                    LibraryFormat::Usda => document
                        .to_usda()
                        .map_err(|error| std::io::Error::other(error.to_string()))?
                        .into_bytes(),
                    LibraryFormat::Usdc => layerstack_usdc::writer::write_document(document)
                        .map_err(|error| std::io::Error::other(error.to_string()))?,
                };
                Ok((format!("{stem}.{extension}"), bytes))
            })
            .collect()
    }

    /// A one-line summary of [`Self::counts`].
    pub fn summary(&self) -> String {
        let c = &self.counts;
        let mut out = String::new();
        let _ = write!(
            out,
            "{} prims: {} leaves, {} slots, {} relationships, {} collections ({} by expression), \
             {} include targets ({} deleted), {} exclude targets, {} collection and {} direct \
             bindings, {} array prims with {} cells, {} dangling targets ({} no prim, {} no \
             property) and {} dangling collection paths",
            c.prims,
            c.leaves,
            c.slots,
            c.relationships,
            c.collections,
            c.expression_collections,
            c.include_targets,
            c.deleted_includes,
            c.exclude_targets,
            c.collection_bindings,
            c.direct_bindings,
            c.array_prims,
            c.array_cells,
            c.dangling_prims + c.dangling_properties,
            c.dangling_prims,
            c.dangling_properties,
            c.dangling_collection_paths
        );
        out
    }
}

/// The base layer's spec for a collection owner.
fn base_owner<'a>(library: &'a mut Prim, sets: &'a mut Prim, owner: &str) -> &'a mut Prim {
    match owner {
        "/Library" => library,
        "/Sets" => sets,
        group => {
            let name = group.rsplit('/').next().expect("a group");
            library
                .children
                .iter_mut()
                .find(|g| g.name == name)
                .expect("the group")
        }
    }
}

fn count_prims(prims: &[Prim]) -> usize {
    prims.iter().map(|p| 1 + count_prims(&p.children)).sum()
}
