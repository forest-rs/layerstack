// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::borrow::Cow;
use hashbrown::HashMap;
use layerstack::{TargetPath, TypedArray, Value as LayerValue, property::PropertyEntry};
use layerstack_usda::save::{canonical_property, prepare_comparison};

use super::*;

/// A validated snapshot of one authored layer, reusable across comparisons.
///
/// Exact-typed declarations keep their parsed values and interned names. Other
/// opinions use the canonical document representation, including variant bodies.
#[derive(Debug)]
pub struct ComparableLayer {
    document: Document,
    retained: Option<Retained>,
}

#[derive(Debug)]
struct Retained {
    properties: HashMap<String, Vec<PropertyEntry>>,
    tokens: TokenInterner,
    paths: PathInterner,
}

impl ComparableLayer {
    /// Reads and validates one complete USDA or USDC layer.
    ///
    /// # Errors
    ///
    /// Returns the same reader or validation diagnostic as [`read_authored_document`].
    pub fn read(input: &[u8]) -> Result<Self, String> {
        let (layer, tokens, paths) = read_authored_layer(input)?;
        let parts = prepare_comparison(layer, &tokens, &paths).map_err(|e| e.to_string())?;
        let retained = (!parts.properties.is_empty()).then_some(Retained {
            properties: parts.properties,
            tokens,
            paths,
        });
        Ok(Self {
            document: parts.document,
            retained,
        })
    }

    /// Takes ownership of an already validated document, such as a flattened stage.
    #[must_use]
    pub fn from_document(document: Document) -> Self {
        Self {
            document,
            retained: None,
        }
    }

    /// Compares all authored opinions, in the same order as [`diff_documents`].
    #[must_use]
    pub fn diff(&self, after: &Self) -> LayerDiff {
        let mut changes = Vec::new();
        let _ = self.compare(after, &mut changes);
        ordered_diff(changes)
    }

    /// Counts differing paths and records without formatting or retaining changes.
    #[must_use]
    pub fn stats(&self, after: &Self) -> DiffStats {
        let mut counter = Counter::default();
        let _ = self.compare(after, &mut counter);
        counter.stats
    }

    /// Returns whether any opinion differs, stopping at the first difference.
    #[must_use]
    pub fn differs(&self, after: &Self) -> bool {
        self.compare(after, &mut FirstDifference).is_break()
    }

    fn compare<C: Changes>(&self, after: &Self, changes: &mut C) -> ControlFlow<()> {
        walk_documents(
            &self.document,
            &after.document,
            changes,
            |changes, path, left, right| {
                let a = self.group(path);
                let b = after.group(path);
                if a.is_none() && b.is_none() {
                    return compare_properties(changes, path, left, right);
                }
                let left = indexed(a, left);
                let right = indexed(b, right);
                for_each_key(&left, &right, |name, a, b| {
                    if let (Some(Entry::Exact(a, source)), Some(Entry::Exact(b, target))) = (a, b)
                        && source.same_property(a, target, b)
                    {
                        return ControlFlow::Continue(());
                    }
                    let a = a.map(|e| e.canonical(path));
                    let b = b.map(|e| e.canonical(path));
                    compare_property(
                        changes,
                        format_args!("{path}.{name}"),
                        a.as_deref(),
                        b.as_deref(),
                    )
                })
            },
        )
    }

    fn group(&self, path: &str) -> Option<(&Retained, &[PropertyEntry])> {
        let retained = self.retained.as_ref()?;
        Some((retained, retained.properties.get(path)?))
    }
}

enum Entry<'a> {
    Document(&'a Property),
    Exact(&'a PropertyEntry, &'a Retained),
}

fn indexed<'a>(
    group: Option<(&'a Retained, &'a [PropertyEntry])>,
    properties: &'a [Property],
) -> BTreeMap<&'a str, Entry<'a>> {
    if let Some((source, entries)) = group {
        return entries
            .iter()
            .map(|entry| {
                (
                    source.tokens.resolve(entry.name),
                    Entry::Exact(entry, source),
                )
            })
            .collect();
    }
    properties
        .iter()
        .map(|p| (p.name(), Entry::Document(p)))
        .collect()
}

impl<'a> Entry<'a> {
    fn canonical(&self, path: &str) -> Cow<'a, Property> {
        match self {
            Self::Document(p) => Cow::Borrowed(p),
            Self::Exact(p, source) => Cow::Owned(
                canonical_property(p, path, &source.tokens, &source.paths)
                    .expect("retained declarations have validated names, types and targets"),
            ),
        }
    }
}

impl Retained {
    // Both declarations passed the writer's exact type and field checks. IDs
    // belong to independent interners. Compare the names they resolve to.
    fn same_property(&self, a: &PropertyEntry, after: &Self, b: &PropertyEntry) -> bool {
        let (a, b) = (&a.spec, &b.spec);
        if a.kind != b.kind
            || a.custom != b.custom
            || a.variability != b.variability
            || a.type_name.as_ref().map(|t| t.type_name())
                != b.type_name.as_ref().map(|t| t.type_name())
        {
            return false;
        }
        match (&a.targets, &b.targets) {
            (Some(a), Some(b)) => {
                let a = a.explicit.as_deref().expect("validated explicit targets");
                let b = b.explicit.as_deref().expect("validated explicit targets");
                return a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|(&a, &b)| self.same_target(a, after, b));
            }
            (None, None) => {}
            _ => return false,
        }
        match (&a.default, &b.default) {
            (None, None) => true,
            (Some(LayerValue::String(a)), Some(LayerValue::String(b))) => a == b,
            (Some(LayerValue::Token(a)), Some(LayerValue::Token(b))) => {
                self.tokens.resolve(*a) == after.tokens.resolve(*b)
            }
            (Some(LayerValue::Int(a)), Some(LayerValue::Int(b))) => a == b,
            (Some(LayerValue::Int64(a)), Some(LayerValue::Int64(b))) => a == b,
            (Some(LayerValue::Bool(a)), Some(LayerValue::Bool(b))) => a == b,
            (Some(LayerValue::Float(a)), Some(LayerValue::Float(b))) => a.to_bits() == b.to_bits(),
            (Some(LayerValue::Double(a)), Some(LayerValue::Double(b))) => {
                a.to_bits() == b.to_bits()
            }
            (Some(LayerValue::TypedArray(a)), Some(LayerValue::TypedArray(b))) => match (a, b) {
                (TypedArray::Int64(a), TypedArray::Int64(b)) => a == b,
                (TypedArray::Float(a), TypedArray::Float(b)) => a
                    .iter()
                    .map(|v| v.to_bits())
                    .eq(b.iter().map(|v| v.to_bits())),
                (TypedArray::Double(a), TypedArray::Double(b)) => a
                    .iter()
                    .map(|v| v.to_bits())
                    .eq(b.iter().map(|v| v.to_bits())),
                (TypedArray::Token(a), TypedArray::Token(b)) => a
                    .iter()
                    .map(|t| self.tokens.resolve(*t))
                    .eq(b.iter().map(|t| after.tokens.resolve(*t))),
                (TypedArray::String(a), TypedArray::String(b)) => a.iter().eq(b.iter()),
                _ => false,
            },
            _ => false,
        }
    }

    fn same_target(&self, a: TargetPath, after: &Self, b: TargetPath) -> bool {
        a.property_path().map(|p| self.tokens.resolve(p.property()))
            == b.property_path()
                .map(|p| after.tokens.resolve(p.property()))
            && self
                .paths
                .resolve(a.prim_path())
                .segments()
                .iter()
                .map(|&t| self.tokens.resolve(t))
                .eq(after
                    .paths
                    .resolve(b.prim_path())
                    .segments()
                    .iter()
                    .map(|&t| after.tokens.resolve(t)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(before: &str, after: &str) {
        let a = read_authored_document(before.as_bytes()).unwrap();
        let b = read_authored_document(after.as_bytes()).unwrap();
        let expected = diff_documents(&a, &b);
        let stats = diff_document_stats(&a, &b);
        let left = ComparableLayer::read(before.as_bytes()).unwrap();
        let right = ComparableLayer::read(after.as_bytes()).unwrap();
        let doc_left = ComparableLayer::from_document(a);
        let doc_right = ComparableLayer::from_document(b);
        for (a, b) in [(&left, &right), (&doc_left, &right), (&left, &doc_right)] {
            assert_eq!(a.diff(b), expected);
            assert_eq!(a.stats(b), stats);
            assert_eq!(a.differs(b), !expected.changes.is_empty());
        }
    }

    #[test]
    fn exact_declarations_match_canonical_values_across_independent_interners() {
        let a = r#"#usda 1.0
def "P" {
 custom token tag = "red"
 custom token[] labels = ["red", "blue"]
 custom double[] samples = [0, 2]
 custom rel links = [</P.tag>, </P/Child>]
 def "Child" {}
}
"#;
        let reordered = r#"#usda 1.0
def "P" {
 custom rel links = [</P.tag>, </P/Child>]
 custom double[] samples = [0, 2]
 custom token[] labels = ["red", "blue"]
 custom token tag = "red"
 def "Child" {}
}
"#;
        check(a, reordered);
        check(
            a,
            &reordered
                .replace("red", "green")
                .replace("[0, 2]", "[-0, 2]")
                .replace("/P.tag", "/P.labels"),
        );
        // A time-sampled property makes the revised group use canonical declarations.
        check(
            a,
            &reordered.replace(
                "custom token tag",
                "custom double animated.timeSamples = {1: 2}\n custom token tag",
            ),
        );
    }
}
