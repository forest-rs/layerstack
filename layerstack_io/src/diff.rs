// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Read-only authored layer comparison.
//!
//! Specs match by authored path, including variant selections, and fields
//! match by name (AOUSD Core §7.3–7.6). Asset paths and list edits are kept as
//! authored (Core §6.6.3, §10.3). Each layer has its own interners.

use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::{cmp::Ordering, fmt};

use layerstack::{Layer, LayerId, NoAssets, PathInterner, TokenInterner};
use layerstack_usda::writer::{
    Document, ListOp, Metadatum, Prim, Property, Reference, Specifier, SubLayer, Value, Variability,
};

mod values;
use values::{array_change, debug_summary, same_value};

/// Which input prevented the comparison from completing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSide {
    /// The baseline layer.
    Before,
    /// The revised layer.
    After,
}

impl fmt::Display for DiffSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Before => "before",
            Self::After => "after",
        })
    }
}

/// A layer could not be read or represented without dropping authored content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffError {
    /// The input that failed.
    pub side: DiffSide,
    /// Detail from the reader or authored layer validator.
    pub message: String,
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} layer: {}", self.side, self.message)
    }
}

impl core::error::Error for DiffError {}

/// A complete comparison, ordered by path, field name, and sample time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerDiff {
    /// Differences in authored specs and fields.
    pub changes: Vec<Change>,
}

/// How an authored opinion differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// Present only in the revised layer.
    Added,
    /// Present only in the baseline layer.
    Removed,
    /// Present in both layers with different authored content.
    Changed,
}

impl ChangeKind {
    /// Stable name for display and machine output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

/// One difference in an authored spec or opinion.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Absolute spec path, with variant selections where applicable. The
    /// layer metadata spec is at `/`.
    pub path: String,
    /// Authored field name, absent for a whole-spec addition or removal.
    pub field: Option<String>,
    /// Authored sample time, when the difference identifies one time sample.
    pub time: Option<f64>,
    /// What changed.
    pub kind: ChangeKind,
    /// Compact representation of the baseline opinion or spec kind.
    pub before: Option<String>,
    /// Compact representation of the revised opinion or spec kind.
    pub after: Option<String>,
    /// Positional differences when both opinions are arrays of the same type.
    pub array: Option<ArrayChange>,
}

/// Positional summary of an array edit. Insertions and removals in the middle
/// are reported by their resulting index differences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayChange {
    /// Element count in the baseline.
    pub before_len: usize,
    /// Element count in the revised array.
    pub after_len: usize,
    /// Count of differing positions, including trailing added/removed elements.
    pub changed_count: usize,
    /// First differing zero-based indices, capped at eight.
    pub indices: Vec<usize>,
}

/// Compares complete USDA or USDC inputs, detected by file contents.
///
/// The comparison operates on the typed authored opinions represented by both
/// the reader and the shared layer writer. It compares all variant branches
/// and needs no source identifiers, stage, schemas, or dependency files.
/// Formatting, comments outside authored fields, and property declaration
/// order have no effect. An explicit property ordering opinion is compared.
/// Implicit declaration defaults and the readers' typed interpretations
/// apply equally to both inputs.
///
/// # Errors
///
/// Returns the input side and reason if a format or authored opinion is
/// unsupported, a reader reports incomplete import, or input is malformed.
/// USDZ packages are unsupported.
pub fn diff_layers(before: &[u8], after: &[u8]) -> Result<LayerDiff, DiffError> {
    let read = |input, side| read_document(input).map_err(|message| DiffError { side, message });
    let before = read(before, DiffSide::Before)?;
    let after = read(after, DiffSide::After)?;
    Ok(diff_documents(&before, &after))
}

fn read_document(input: &[u8]) -> Result<Document, String> {
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let id = LayerId(1);
    let layer: Layer = if input.starts_with(b"PXR-USDC") {
        let imported = layerstack_usdc::read_usdc(input, id, &tokens, &paths, &mut NoAssets)
            .map_err(|error| error.to_string())?;
        if let Some(first) = imported.diagnostics.first() {
            let field = first.field.as_deref().unwrap_or("<spec>");
            return Err(format!("{} {field}: {}", first.spec_path, first.message));
        }
        imported.layer
    } else {
        if input.starts_with(b"PK\x03\x04") {
            return Err(String::from("USDZ packages are unsupported"));
        }
        let source = core::str::from_utf8(input).map_err(|error| error.to_string())?;
        if !source.trim_start_matches('\u{feff}').starts_with("#usda ") {
            return Err(String::from("expected a USDA or USDC layer"));
        }
        let imported = layerstack_usda::read_usda(source, id, &tokens, &paths, &mut NoAssets);
        if let Some(first) = imported.diagnostics().next() {
            return Err(format!("{:?}: {}", first.span, first.message));
        }
        if imported.is_rejected() {
            return Err(String::from("USDA reader rejected the layer"));
        }
        imported.emitted.layer
    };
    if let Some(offsets) = layer.unpaired_sublayer_offsets() {
        return Err(format!(
            "/ subLayerOffsets: {} sublayers and {offsets} offsets",
            layer.sublayers().len()
        ));
    }
    layerstack_usda::save::usdc_document(&layer, &tokens, &paths).map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpecKind {
    Layer,
    Prim,
    VariantSet,
    Variant,
    Attribute,
    Relationship,
}

impl SpecKind {
    fn name(self) -> &'static str {
        match self {
            Self::Layer => "layer",
            Self::Prim => "prim",
            Self::VariantSet => "variant set",
            Self::Variant => "variant",
            Self::Attribute => "attribute",
            Self::Relationship => "relationship",
        }
    }
}

#[derive(Debug)]
enum Opinion<'a> {
    Text(&'a str),
    Bool(bool),
    Specifier(Specifier),
    Variability(Variability),
    Value(&'a Value),
    TimeSamples(&'a [(f64, Value)]),
    Names(&'a [String]),
    Children(&'a [Prim]),
    Pairs(&'a [(String, String)]),
    Sublayers(&'a [SubLayer]),
    Paths(&'a ListOp<String>),
    Arcs(&'a ListOp<Reference>),
    Spline(&'a layerstack::spline::SplineData, &'a [KnotData]),
}

type Fields<'a> = BTreeMap<&'a str, Opinion<'a>>;
type KnotData = (f64, Vec<(String, Value)>);

struct Spec<'a> {
    kind: SpecKind,
    fields: Fields<'a>,
    properties: &'a [Property],
}

fn metadata(metadata: &[Metadatum]) -> Fields<'_> {
    metadata
        .iter()
        .map(|entry| (entry.key.as_str(), Opinion::Value(&entry.value)))
        .collect()
}

fn insert<'a, T>(
    fields: &mut Fields<'a>,
    name: &'static str,
    value: Option<T>,
    opinion: impl FnOnce(T) -> Opinion<'a>,
) {
    if let Some(value) = value {
        fields.insert(name, opinion(value));
    }
}

fn order<'a>(fields: &mut Fields<'a>, name: &'static str, value: Option<&'a [String]>) {
    insert(fields, name, value, Opinion::Names);
}

fn index(document: &Document) -> BTreeMap<String, Spec<'_>> {
    let mut specs = BTreeMap::new();
    let mut fields = metadata(&document.metadata);
    insert(
        &mut fields,
        "defaultPrim",
        document.default_prim.as_deref(),
        Opinion::Text,
    );
    insert(
        &mut fields,
        "relocates",
        document.relocates.as_deref(),
        Opinion::Pairs,
    );
    order(&mut fields, "primOrder", document.prim_order.as_deref());
    if !document.sublayers.is_empty() {
        fields.insert("subLayers", Opinion::Sublayers(&document.sublayers));
    }
    children(&mut fields, &document.prims);
    specs.insert(
        String::from("/"),
        Spec {
            kind: SpecKind::Layer,
            fields,
            properties: &[],
        },
    );

    let mut pending: Vec<_> = document
        .prims
        .iter()
        .map(|prim| (format!("/{}", prim.name), prim, SpecKind::Prim))
        .collect();
    while let Some((path, prim, kind)) = pending.pop() {
        let mut fields = metadata(&prim.metadata);
        insert(&mut fields, "specifier", prim.specifier, Opinion::Specifier);
        insert(
            &mut fields,
            "typeName",
            prim.type_name.as_deref(),
            Opinion::Text,
        );
        insert(
            &mut fields,
            "inheritPaths",
            prim.inherits.as_ref(),
            Opinion::Paths,
        );
        insert(
            &mut fields,
            "specializes",
            prim.specializes.as_ref(),
            Opinion::Paths,
        );
        insert(
            &mut fields,
            "payload",
            prim.payloads.as_ref(),
            Opinion::Arcs,
        );
        insert(
            &mut fields,
            "references",
            prim.references.as_ref(),
            Opinion::Arcs,
        );
        insert(
            &mut fields,
            "variantSetNames",
            prim.variant_set_names.as_ref(),
            Opinion::Paths,
        );
        if !prim.variant_selections.is_empty() {
            fields.insert("variantSelection", Opinion::Pairs(&prim.variant_selections));
        }
        order(&mut fields, "primOrder", prim.prim_order.as_deref());
        order(&mut fields, "propertyOrder", prim.property_order.as_deref());
        children(&mut fields, &prim.children);
        let separator = if kind == SpecKind::Variant { "" } else { "/" };
        pending.extend(prim.children.iter().map(|child| {
            (
                format!("{path}{separator}{}", child.name),
                child,
                SpecKind::Prim,
            )
        }));
        for set in &prim.variant_sets {
            specs.insert(
                format!("{path}{{{}=}}", set.name),
                Spec {
                    kind: SpecKind::VariantSet,
                    fields: Fields::new(),
                    properties: &[],
                },
            );
            pending.extend(set.variants.iter().map(|variant| {
                (
                    format!("{path}{{{}={}}}", set.name, variant.name),
                    variant,
                    SpecKind::Variant,
                )
            }));
        }
        specs.insert(
            path,
            Spec {
                kind,
                fields,
                properties: &prim.properties,
            },
        );
    }
    specs
}

fn children<'a>(fields: &mut Fields<'a>, children: &'a [Prim]) {
    if !children.is_empty() {
        fields.insert("primChildren", Opinion::Children(children));
    }
}

fn property_spec(property: &Property) -> Spec<'_> {
    match property {
        Property::Attribute(attribute) => {
            let mut fields = metadata(&attribute.metadata);
            fields.insert("typeName", Opinion::Text(&attribute.type_name));
            fields.insert("custom", Opinion::Bool(attribute.custom));
            fields.insert("variability", Opinion::Variability(attribute.variability));
            insert(
                &mut fields,
                "default",
                attribute.value.as_ref(),
                Opinion::Value,
            );
            insert(
                &mut fields,
                "timeSamples",
                attribute.time_samples.as_deref(),
                Opinion::TimeSamples,
            );
            insert(
                &mut fields,
                "connectionPaths",
                attribute.connections.as_ref(),
                Opinion::Paths,
            );
            insert(
                &mut fields,
                "spline",
                attribute.spline.as_deref(),
                |spline| Opinion::Spline(spline, &attribute.spline_custom_data),
            );
            Spec {
                kind: SpecKind::Attribute,
                fields,
                properties: &[],
            }
        }
        Property::Relationship(relationship) => {
            let mut fields = metadata(&relationship.metadata);
            fields.insert("custom", Opinion::Bool(relationship.custom));
            let variability = if relationship.varying {
                Variability::Varying
            } else {
                Variability::Uniform
            };
            fields.insert("variability", Opinion::Variability(variability));
            insert(
                &mut fields,
                "default",
                relationship.default.as_deref(),
                Opinion::Text,
            );
            insert(
                &mut fields,
                "targetPaths",
                relationship.targets.as_ref(),
                Opinion::Paths,
            );
            Spec {
                kind: SpecKind::Relationship,
                fields,
                properties: &[],
            }
        }
    }
}

/// Compares the specs and fields of two validated writer documents in memory.
/// For composed stage paths, use documents lowered from losslessly flattened
/// layers with expanded instances. Both inputs may use their own interners
/// during lowering because document paths and names are text.
#[must_use]
pub fn diff_documents(before: &Document, after: &Document) -> LayerDiff {
    let before = index(before);
    let after = index(after);
    let mut changes = Vec::new();
    for_each_key(&before, &after, |path, left, right| {
        compare_spec(&mut changes, path, left, right);
        let left = property_index(left.map_or(&[], |s| s.properties));
        let right = property_index(right.map_or(&[], |s| s.properties));
        for_each_key(&left, &right, |name, a, b| {
            if let (Some(a), Some(b)) = (a, b)
                && same_plain_property(a, b)
            {
                return;
            }
            let a = a.map(|p| property_spec(p));
            let b = b.map(|p| property_spec(p));
            compare_spec(
                &mut changes,
                &format!("{path}.{name}"),
                a.as_ref(),
                b.as_ref(),
            );
        });
    });
    // A prim's properties are visited together. Restore global path order
    // (including variant paths) while retaining field and sample order.
    layerstack::sort::sort_by(&mut changes, |a, b| a.path.cmp(&b.path));
    LayerDiff { changes }
}

fn property_index(properties: &[Property]) -> BTreeMap<&str, &Property> {
    properties.iter().map(|p| (p.name(), p)).collect()
}

// Ordinary defaults and target lists can establish equality on their typed
// storage without allocating either property's diagnostic fields. Leave all
// other opinions to the same detailed comparison that reports changes.
fn same_plain_property(a: &Property, b: &Property) -> bool {
    match (a, b) {
        (Property::Attribute(a), Property::Attribute(b)) => {
            a.metadata.is_empty()
                && b.metadata.is_empty()
                && a.time_samples.is_none()
                && b.time_samples.is_none()
                && a.spline.is_none()
                && b.spline.is_none()
                && a.custom == b.custom
                && a.variability == b.variability
                && a.type_name == b.type_name
                && a.connections == b.connections
                && match (&a.value, &b.value) {
                    (Some(a), Some(b)) => same_value(a, b),
                    (None, None) => true,
                    _ => false,
                }
        }
        (Property::Relationship(a), Property::Relationship(b)) => {
            a.metadata.is_empty() && b.metadata.is_empty() && a == b
        }
        _ => false,
    }
}

fn compare_spec(
    changes: &mut Vec<Change>,
    path: &str,
    left: Option<&Spec<'_>>,
    right: Option<&Spec<'_>>,
) {
    match (left, right) {
        (Some(left), Some(right)) if left.kind == right.kind => {
            for_each_key(&left.fields, &right.fields, |field, a, b| match (a, b) {
                (Some(Opinion::TimeSamples(a)), Some(Opinion::TimeSamples(b))) => {
                    compare_samples(changes, path, field, a, b);
                }
                (Some(Opinion::TimeSamples(a)), None) if !a.is_empty() => {
                    compare_samples(changes, path, field, a, &[]);
                }
                (None, Some(Opinion::TimeSamples(b))) if !b.is_empty() => {
                    compare_samples(changes, path, field, &[], b);
                }
                _ => compare_field(changes, path, field, None, a, b),
            });
        }
        _ => changes.push(Change {
            path: path.to_string(),
            field: None,
            time: None,
            kind: kind(left, right),
            before: left.map(|spec| String::from(spec.kind.name())),
            after: right.map(|spec| String::from(spec.kind.name())),
            array: None,
        }),
    }
}

// Visit the union of two ordered maps without allocating another index.
fn for_each_key<K: Ord, V>(
    before: &BTreeMap<K, V>,
    after: &BTreeMap<K, V>,
    mut visit: impl FnMut(&K, Option<&V>, Option<&V>),
) {
    let mut before = before.iter().peekable();
    let mut after = after.iter().peekable();
    loop {
        match (before.peek(), after.peek()) {
            (Some((a, _)), Some((b, _))) => match a.cmp(b) {
                Ordering::Less => {
                    let (key, value) = before.next().expect("peeked baseline");
                    visit(key, Some(value), None);
                }
                Ordering::Greater => {
                    let (key, value) = after.next().expect("peeked revision");
                    visit(key, None, Some(value));
                }
                Ordering::Equal => {
                    let (key, a) = before.next().expect("peeked baseline");
                    let (_, b) = after.next().expect("peeked revision");
                    visit(key, Some(a), Some(b));
                }
            },
            (Some(_), None) => {
                before.for_each(|(key, value)| visit(key, Some(value), None));
                return;
            }
            (None, _) => {
                after.for_each(|(key, value)| visit(key, None, Some(value)));
                return;
            }
        }
    }
}

fn kind<T>(before: Option<T>, after: Option<T>) -> ChangeKind {
    match (before, after) {
        (None, Some(_)) => ChangeKind::Added,
        (Some(_), None) => ChangeKind::Removed,
        _ => ChangeKind::Changed,
    }
}

fn compare_samples(
    changes: &mut Vec<Change>,
    path: &str,
    field: &str,
    mut before: &[(f64, Value)],
    mut after: &[(f64, Value)],
) {
    // Document validation guarantees finite times in ascending order.
    // AOUSD Core §12.3. An authored sample is keyed by its exact time.
    while !before.is_empty() || !after.is_empty() {
        let ordering = match (before.first(), after.first()) {
            (Some(a), Some(b)) => a.0.partial_cmp(&b.0).expect("finite sample times"),
            (Some(_), None) => Ordering::Less,
            _ => Ordering::Greater,
        };
        let a = (ordering != Ordering::Greater).then(|| {
            let ((time, value), rest) = before.split_first().expect("baseline sample");
            before = rest;
            (*time, Opinion::Value(value))
        });
        let b = (ordering != Ordering::Less).then(|| {
            let ((time, value), rest) = after.split_first().expect("revised sample");
            after = rest;
            (*time, Opinion::Value(value))
        });
        let time = a.as_ref().or(b.as_ref()).map(|(time, _)| *time);
        compare_field(
            changes,
            path,
            field,
            time,
            a.as_ref().map(|(_, v)| v),
            b.as_ref().map(|(_, v)| v),
        );
    }
}

fn compare_field(
    changes: &mut Vec<Change>,
    path: &str,
    field: &str,
    time: Option<f64>,
    before: Option<&Opinion<'_>>,
    after: Option<&Opinion<'_>>,
) {
    let mut array = None;
    if let (Some(a), Some(b)) = (before, after) {
        if let (Opinion::Value(a), Opinion::Value(b)) = (a, b) {
            array = array_change(a, b);
        }
        let equal = match &array {
            Some(summary) => summary.changed_count == 0,
            None => a.same(b),
        };
        if equal {
            return;
        }
    }
    let mut before_summary = before.map(Opinion::summary);
    let mut after_summary = after.map(Opinion::summary);
    if let (Some(Opinion::Paths(a)), Some(Opinion::Paths(b))) = (before, after)
        && (before_summary.as_deref().is_some_and(|s| s.ends_with('…'))
            || after_summary.as_deref().is_some_and(|s| s.ends_with('…')))
        && let Some((left, right)) = values::path_list_change(a, b)
    {
        before_summary = Some(left);
        after_summary = Some(right);
    }
    changes.push(Change {
        path: String::from(path),
        field: Some(String::from(field)),
        time,
        kind: kind(before, after),
        before: before_summary,
        after: after_summary,
        array,
    });
}

impl Opinion<'_> {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Specifier(a), Self::Specifier(b)) => a == b,
            (Self::Variability(a), Self::Variability(b)) => a == b,
            (Self::Value(a), Self::Value(b)) => same_value(a, b),
            (Self::Names(a), Self::Names(b)) => a == b,
            (Self::Children(a), Self::Children(b)) => {
                a.iter().map(|p| &p.name).eq(b.iter().map(|p| &p.name))
            }
            (Self::Pairs(a), Self::Pairs(b)) => a == b,
            (Self::Sublayers(a), Self::Sublayers(b)) => {
                a.len() == b.len()
                    && a.iter().zip(*b).all(|(a, b)| {
                        a.asset == b.asset && values::same_offset(&a.offset, &b.offset)
                    })
            }
            (Self::Paths(a), Self::Paths(b)) => a == b,
            (Self::Arcs(a), Self::Arcs(b)) => values::same_arcs(a, b),
            (Self::Spline(a, a_data), Self::Spline(b, b_data)) => {
                values::same_spline(a, a_data, b, b_data)
            }
            _ => false,
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::Text(value) => debug_summary(value),
            Self::Bool(value) => value.to_string(),
            Self::Specifier(value) => debug_summary(value),
            Self::Variability(value) => debug_summary(value),
            Self::Value(value) => values::summary(value),
            Self::TimeSamples(values) => format!("{} samples", values.len()),
            Self::Names(names) => debug_summary(names),
            Self::Children(prims) => {
                debug_summary(prims.iter().take(8).map(|p| &p.name).collect::<Vec<_>>())
            }
            Self::Pairs(pairs) => debug_summary(pairs),
            Self::Sublayers(layers) => debug_summary(layers),
            Self::Paths(paths) => values::list_summary(paths),
            Self::Arcs(arcs) => values::list_summary(arcs),
            Self::Spline(spline, _) => format!("spline with {} knots", spline.knots.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(before: &str, after: &str) -> Vec<Change> {
        diff_layers(before.as_bytes(), after.as_bytes())
            .unwrap()
            .changes
    }

    #[test]
    fn ignores_layout_and_matches_properties_by_name() {
        let before = "#usda 1.0\ndef Xform \"Root\" { float a = 3; int b = 4; }";
        let after = "#usda 1.0\n# A comment\ndef Xform \"Root\" {\n int b = 4\n float a = 3\n}";
        assert!(diff(before, after).is_empty());
    }

    #[test]
    fn compares_authored_opinions_in_all_variant_branches() {
        let scene = r#"#usda 1.0
        (
            subLayers = [@does-not-exist.usda@]
            defaultPrim = "Root"
        )
        def Xform "Root" (
            prepend variantSets = "mode"
            variants = { string mode = "light" }
        ) {
            variantSet "mode" = {
                "light" {}
                "dark" {
                    def "Branch" {
                        float weight.timeSamples = { 1: 2, 3: 4 }
                        prepend token input.connect = </Root.output>
                        rel binding = </Root>
                    }
                }
            }
        }"#;
        let edited = scene
            .replace("1: 2, 3: 4", "1: None, 2: 3")
            .replace("prepend token", "append token")
            .replace("rel binding = </Root>", "rel binding = None")
            .replace("defaultPrim = \"Root\"", "defaultPrim = \"Other\"");
        let changes = diff(scene, &edited);
        let keys: Vec<_> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.field.as_deref(), change.time))
            .collect();
        assert_eq!(
            keys,
            [
                ("/", Some("defaultPrim"), None),
                ("/Root{mode=dark}Branch.binding", Some("targetPaths"), None),
                (
                    "/Root{mode=dark}Branch.input",
                    Some("connectionPaths"),
                    None
                ),
                (
                    "/Root{mode=dark}Branch.weight",
                    Some("timeSamples"),
                    Some(1.0)
                ),
                (
                    "/Root{mode=dark}Branch.weight",
                    Some("timeSamples"),
                    Some(2.0)
                ),
                (
                    "/Root{mode=dark}Branch.weight",
                    Some("timeSamples"),
                    Some(3.0)
                ),
            ]
        );
        assert_eq!(changes[1].after.as_deref(), Some("explicit []"));
        assert_eq!(
            changes[2].before.as_deref(),
            Some(r#"prepend ["/Root.output"]"#)
        );
        assert_eq!(changes[4].kind, ChangeKind::Added);
        assert_eq!(changes[5].kind, ChangeKind::Removed);
    }

    #[test]
    fn wide_relationships_show_the_first_differing_operand() {
        let targets = (0..1000)
            .map(|index| format!("</Target{index}>"))
            .collect::<Vec<_>>()
            .join(",");
        let before = format!("#usda 1.0\ndef \"Root\" {{ rel links = [{targets}]; }}");
        let after = before.replace("</Target999>", "</Changed>");
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].before.as_deref(),
            Some(r#"explicit[999] "/Target999""#)
        );
        assert_eq!(
            changes[0].after.as_deref(),
            Some(r#"explicit[999] "/Changed""#)
        );
    }

    #[test]
    fn arrays_report_lengths_and_bounded_changed_indices() {
        let before = "#usda 1.0\ndef Mesh \"Mesh\" { point3f[] points = [(0, 0, 0), (1, 1, 1)]; }";
        let after = "#usda 1.0\ndef Mesh \"Mesh\" { point3f[] points = [(1, 0, 0), (1, 1, 1), (2, 2, 2)]; }";
        let changes = diff(before, after);
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].array,
            Some(ArrayChange {
                before_len: 2,
                after_len: 3,
                changed_count: 2,
                indices: alloc::vec![0, 2],
            })
        );
        let header = "#usda 1.0\ndef \"A\" { int[] values = [";
        let many = (0..20).map(|n| n.to_string()).collect::<Vec<_>>().join(",");
        let empty = format!("{header}]; }}");
        let full = format!("{header}{many}]; }}");
        let changes = diff(&empty, &full);
        let array = changes[0].array.as_ref().unwrap();
        assert_eq!(array.changed_count, 20);
        assert_eq!(array.indices, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn preserves_blocks_empty_samples_and_float_bits() {
        let baseline = "#usda 1.0\ndef \"A\" { float a = nan; float[] b = [nan, 0]; float c; }";
        assert!(diff(baseline, baseline).is_empty());
        let after = "#usda 1.0\ndef \"A\" { float a = nan; float[] b = [nan, -0.0]; float c = None; float c.timeSamples = {}; }";
        let changes = diff(baseline, after);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].array.as_ref().unwrap().indices, [1]);
        assert_eq!(changes[1].field.as_deref(), Some("default"));
        assert_eq!(changes[2].field.as_deref(), Some("timeSamples"));
        assert_eq!(changes[2].after.as_deref(), Some("0 samples"));
    }

    #[test]
    fn refuses_crate_offsets_that_would_be_replaced_by_defaults() {
        use layerstack_usdc::writer::{Spec, SpecForm, Value, write_crate};

        let malformed = write_crate(&[Spec::new("/", SpecForm::PseudoRoot)
            .with_field(
                "subLayers",
                Value::StringVector(alloc::vec!["sub.usda".into()]),
            )
            .with_field("subLayerOffsets", Value::LayerOffsetVector(alloc::vec![]))])
        .unwrap();
        let error = diff_layers(&malformed, &malformed).unwrap_err();
        assert!(error.message.contains("subLayerOffsets"));
    }

    #[test]
    fn refuses_inputs_that_cannot_be_compared_completely() {
        let valid = b"#usda 1.0\n";
        let malformed = b"#usda 1.0\ndef A {";
        assert_eq!(
            diff_layers(malformed, valid).unwrap_err().side,
            DiffSide::Before
        );
        assert_eq!(
            diff_layers(valid, b"PK\x03\x04").unwrap_err().side,
            DiffSide::After
        );
        assert_eq!(
            diff_layers(valid, b"PXR-USDC").unwrap_err().side,
            DiffSide::After
        );
    }
}
