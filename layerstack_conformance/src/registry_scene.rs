// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A generated record registry: the shape of a large asset registry or parts
//! index, one layer of many small prims with a few scalar attributes each
//! and many distinct strings.
//!
//! [`RegistryScene::generate`] builds, from a seed and a [`RegistryShape`]:
//!
//! - one root, `/Registry`, holding groups (`/Registry/Group_000`), each
//!   holding records (`/Registry/Group_000/Record_000000`);
//! - on every record, a `string label` and a `string sourceId` no other
//!   record shares, a `token category` from a few dozen, an `int revision`,
//!   a `uniform token status` and a short `token[] tags`;
//! - on every [`RegistryShape::linked_every`]th record, a `supersedes`
//!   relationship naming an earlier record.
//!
//! Unlike the parts library (`crate::library_scene`), almost no two specs
//! share a fieldset, so a reader's per-spec, per-field and per-string costs
//! dominate rather than its value arrays. [`RegistryScene::write`] writes
//! `registry.usda` or `registry.usdc`. Generation is deterministic: the same
//! shape gives byte-identical files.
//!
//! Spec: AOUSD Core §7.6 (spec fields), §16.3 (crate files).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use layerstack_usda::writer::{Attribute, Document, Prim, Relationship, Value};

use crate::library_scene::LibraryFormat;

/// How big the generated registry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistryShape {
    /// Seed of every random choice.
    pub seed: u64,
    /// Groups under `/Registry`.
    pub groups: usize,
    /// Records in each group.
    pub records_per_group: usize,
    /// Every this many records, one authors a `supersedes` relationship.
    pub linked_every: usize,
}

impl RegistryShape {
    /// 25,000 records in 100 groups: about 177,000 specs.
    pub const FULL: Self = Self {
        seed: 0x5eed_0000_0000_0002,
        groups: 100,
        records_per_group: 250,
        linked_every: 10,
    };

    /// 600 records, for debug-build tests.
    pub const SMALL: Self = Self {
        seed: Self::FULL.seed,
        groups: 6,
        records_per_group: 100,
        linked_every: 10,
    };
}

/// What [`RegistryScene::generate`] made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegistryCounts {
    /// Prims defined, the pseudo-root left out.
    pub prims: usize,
    /// Record prims.
    pub records: usize,
    /// Attribute specs.
    pub attributes: usize,
    /// Relationship specs.
    pub relationships: usize,
    /// `tags` cells, in all.
    pub tags: usize,
}

/// The generated registry, one layer.
#[derive(Clone, Debug)]
pub struct RegistryScene {
    /// The registry layer.
    pub document: Document,
    /// What was generated.
    pub counts: RegistryCounts,
}

const CATEGORIES: [&str; 32] = [
    "alder", "birch", "cedar", "elm", "fir", "hazel", "larch", "linden", "maple", "oak", "pine",
    "poplar", "rowan", "spruce", "willow", "yew", "amber", "basalt", "cobalt", "copper", "garnet",
    "indigo", "jade", "ochre", "onyx", "opal", "pearl", "quartz", "ruby", "slate", "topaz",
    "umber",
];

const STATUSES: [&str; 4] = ["draft", "review", "released", "retired"];

const TAGS: [&str; 16] = [
    "north", "south", "east", "west", "early", "late", "small", "large", "light", "heavy", "plain",
    "marked", "spare", "core", "legacy", "pilot",
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
}

impl RegistryScene {
    /// Generates the registry for `shape`.
    pub fn generate(shape: &RegistryShape) -> Self {
        let mut rng = Rng(shape.seed);
        let mut counts = RegistryCounts::default();
        let mut registry = Prim::def("Scope", "Registry");
        counts.prims += 1;
        let mut records: Vec<String> = Vec::new();
        for g in 0..shape.groups {
            let group_name = format!("Group_{g:03}");
            let mut group = Prim::def("Scope", group_name.clone());
            counts.prims += 1;
            for r in 0..shape.records_per_group {
                let index = g * shape.records_per_group + r;
                let name = format!("Record_{index:06}");
                let mut record = Prim::def("Scope", name.clone());
                let category = CATEGORIES[rng.below(CATEGORIES.len())];
                let serial = rng.next();
                record.push_property(Attribute::new(
                    "label",
                    "string",
                    Value::String(format!("{category} part {index} rev {}", serial % 97)),
                ));
                record.push_property(Attribute::new(
                    "sourceId",
                    "string",
                    Value::String(format!("src:{serial:016x}")),
                ));
                record.push_property(Attribute::new(
                    "category",
                    "token",
                    Value::Token(category.into()),
                ));
                record.push_property(Attribute::new(
                    "revision",
                    "int",
                    Value::Int(i32::try_from(serial % 1000).expect("small")),
                ));
                record.push_property(
                    Attribute::new(
                        "status",
                        "token",
                        Value::Token(STATUSES[rng.below(STATUSES.len())].into()),
                    )
                    .uniform(),
                );
                let tags = 1 + rng.below(4);
                counts.tags += tags;
                record.push_property(Attribute::new(
                    "tags",
                    "token[]",
                    Value::TokenArray(
                        (0..tags)
                            .map(|_| TAGS[rng.below(TAGS.len())].to_string())
                            .collect(),
                    ),
                ));
                counts.attributes += 6;
                if shape.linked_every > 0 && index % shape.linked_every == 1 {
                    let earlier = &records[rng.below(records.len())];
                    record.push_property(Relationship::new("supersedes", earlier.clone()));
                    counts.relationships += 1;
                }
                records.push(format!("/Registry/{group_name}/{name}"));
                group.children.push(record);
                counts.prims += 1;
                counts.records += 1;
            }
            registry.children.push(group);
        }
        let mut document = Document::new();
        document.prims.push(registry);
        Self { document, counts }
    }

    /// Writes `registry.usda` or `registry.usdc` into `dir` and returns its
    /// path and size in bytes.
    ///
    /// # Errors
    ///
    /// When the directory or file cannot be written, or the layer cannot be
    /// serialized.
    pub fn write(&self, dir: &Path, format: LibraryFormat) -> std::io::Result<(PathBuf, u64)> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("registry.{}", format.extension()));
        let bytes = match format {
            LibraryFormat::Usda => self
                .document
                .to_usda()
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .into_bytes(),
            LibraryFormat::Usdc => layerstack_usdc::writer::write_document(&self.document)
                .map_err(|error| std::io::Error::other(error.to_string()))?,
        };
        std::fs::write(&path, &bytes)?;
        Ok((path, bytes.len() as u64))
    }

    /// A one-line summary of [`Self::counts`].
    pub fn summary(&self) -> String {
        let c = &self.counts;
        let mut out = String::new();
        let _ = write!(
            out,
            "{} prims: {} records, {} attributes, {} relationships, {} tags",
            c.prims, c.records, c.attributes, c.relationships, c.tags
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_registries_are_deterministic_and_counted() {
        let a = RegistryScene::generate(&RegistryShape::SMALL);
        let b = RegistryScene::generate(&RegistryShape::SMALL);
        assert_eq!(a.counts, b.counts);
        assert_eq!(a.document.to_usda().unwrap(), b.document.to_usda().unwrap());
        assert_eq!(a.counts.records, 600);
        assert_eq!(a.counts.prims, 607);
        assert_eq!(a.counts.attributes, 3600);
        assert_eq!(a.counts.relationships, 60);
    }
}
