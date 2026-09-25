// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored property specs: attributes and relationships.
//!
//! A property spec keeps its identity and kind apart from the authored slots
//! it may carry. Every slot is stored independently and may be absent, so an
//! attribute can hold a default value, time samples, a spline and connection
//! paths at the same time, exactly as a layer can author them. Nothing is
//! folded at ingestion time: which slot answers a query is decided when the
//! query runs (see [`crate::Stage::resolve_value`] for default-time queries and
//! [`crate::Stage::resolve_value_at_time`] for numeric times).
//!
//! Spec: AOUSD Core §7.3.7 (property specs), §7.6.3 (property spec fields),
//! §7.6.4 (attribute spec fields; §7.6.4.2.3 notes that attributes "may have a
//! value, have a connection, or both"), §7.6.5 (relationship spec fields).
//! OpenUSD stores the same fields separately on `SdfAttributeSpec`
//! (`pxr/usd/sdf/attributeSpec.h`) and `SdfPropertySpec`
//! (`pxr/usd/sdf/propertySpec.h`); the field set per spec form is registered in
//! `pxr/usd/sdf/schema.cpp`.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::{
    doc::{FieldEntry, FieldValue, Value, get_field, remove_field, set_field_vec},
    interner::TokenId,
    listop::ListOp,
    path::TargetPath,
    spline::SplineData,
};

/// Declared type information for an authored attribute.
///
/// `type_name` preserves the authored USD type name. `default_scalar` is the
/// zero/default value for the non-array form of the property type; sparse
/// array edits use it to synthesize appended elements for `minsize` /
/// `resize`.
///
/// Spec: AOUSD Core §7.6.4.1.1 (`typeName`).
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyType {
    /// Authored USD type name, such as `int`, `point3f`, or `token`.
    pub type_name: Arc<str>,
    /// Whether the declared property is array-valued.
    pub is_array: bool,
    /// Default scalar value for one element of this type.
    pub default_scalar: Value,
}

impl PropertyType {
    /// Creates property type metadata from an authored USD type name.
    #[must_use]
    pub fn new(type_name: impl Into<Arc<str>>, is_array: bool, default_scalar: Value) -> Self {
        Self {
            type_name: type_name.into(),
            is_array,
            default_scalar,
        }
    }

    /// Returns the declared default value for the whole property.
    #[must_use]
    pub fn default_property_value(&self) -> Value {
        if self.is_array {
            Value::Array(Vec::default())
        } else {
            self.default_scalar.clone()
        }
    }

    /// Returns the default scalar element for an array property.
    #[must_use]
    pub fn default_array_element(&self) -> Option<Value> {
        self.is_array.then(|| self.default_scalar.clone())
    }
}

/// The two forms of property spec.
///
/// Spec: AOUSD Core §7.3.7 (attribute specs and relationship specs are
/// collectively property specs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PropertyKind {
    /// An attribute spec: typed values, optionally varying over time, plus
    /// optional connection paths.
    #[default]
    Attribute,
    /// A relationship spec: a list of target paths.
    Relationship,
}

/// Attribute variability (`uniform` or not).
///
/// The field is speculative: it does not imply that any value was authored.
///
/// Spec: AOUSD Core §7.6.4.1.2 (`variability`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Variability {
    /// The resolved value may vary over time (no qualifier).
    #[default]
    Varying,
    /// The resolved value is not expected to vary over time (`uniform`).
    Uniform,
}

/// One authored time sample: a time code and the value at that time.
///
/// The value may be [`Value::Blocked`] to block the attribute at that time
/// (AOUSD Core §12.3.6).
pub type TimeSample = (f64, Value);

/// An authored property spec: identity and kind, plus independently stored
/// authored slots and metadata.
///
/// Every value slot is optional and independent of the others. A slot that is
/// `None` was not authored; removing one slot never disturbs another:
///
/// ```
/// use layerstack::{PropertySpec, Value};
///
/// let mut spec = PropertySpec::attribute()
///     .with_default(Value::Float(1.0))
///     .with_time_samples(vec![(0.0, Value::Float(2.0))]);
/// spec.default = None;
/// assert_eq!(spec.time_samples.as_deref(), Some(&[(0.0, Value::Float(2.0))][..]));
/// ```
///
/// Which slot answers a value query is decided at query time, not here:
/// default-time queries read only [`Self::default`]; numeric-time queries
/// prefer, per spec, time samples, then a spline, then the default.
///
/// Spec: AOUSD Core §7.6.3 (property spec fields), §7.6.4 (attribute spec),
/// §7.6.5 (relationship spec), §12.3 (attribute value resolution), §12.4
/// (relationships and attribute connections).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PropertySpec {
    /// Attribute or relationship.
    pub kind: PropertyKind,
    /// The `custom` qualifier.
    ///
    /// Spec: AOUSD Core §7.6.3.1.1 (`custom`), §12.2.4.
    pub custom: bool,
    /// The `uniform` qualifier.
    ///
    /// Spec: AOUSD Core §7.6.4.1.2 (`variability`).
    pub variability: Variability,
    /// Declared attribute type (`typeName`). Relationships have none.
    ///
    /// Spec: AOUSD Core §7.6.4.1.1 (`typeName`).
    pub type_name: Option<PropertyType>,
    /// Authored default value, possibly [`Value::Blocked`].
    ///
    /// Spec: AOUSD Core §7.6.4.2.1 (`default`).
    pub default: Option<Value>,
    /// Authored time samples, sorted by time.
    ///
    /// `Some(vec![])` records an explicitly authored empty sample map, which
    /// contributes no value.
    ///
    /// Spec: AOUSD Core §7.6.4.2.2 (`timeSamples`).
    pub time_samples: Option<Vec<TimeSample>>,
    /// Authored spline.
    ///
    /// Spec: AOUSD Core §7.6.4.2.4 (`spline`).
    pub spline: Option<SplineData>,
    /// Authored target paths: `connectionPaths` for an attribute,
    /// `targetPaths` for a relationship.
    ///
    /// Connections are inspected separately from attribute value resolution
    /// (see [`crate::Stage::resolve_target_list`]).
    ///
    /// Spec: AOUSD Core §7.6.4.2.3 (`connectionPaths`), §7.6.5.1.1
    /// (`targetPaths`), §12.4.
    pub targets: Option<ListOp<TargetPath>>,
    /// All other authored property metadata, in authored order: for example
    /// `interpolation`, `elementSize`, `customData`, `displayName`, `doc`
    /// (stored as `documentation`), `limits` and `colorSpace`.
    ///
    /// Spec: AOUSD Core §7.4 (metadata fields), §7.6.3.2–§7.6.3.3.
    pub metadata: Vec<FieldEntry>,
}

impl PropertySpec {
    /// Creates an attribute spec with no type and no authored slots.
    #[must_use]
    pub fn attribute() -> Self {
        Self::default()
    }

    /// Creates an attribute spec declared with `property_type`.
    #[must_use]
    pub fn typed_attribute(property_type: PropertyType) -> Self {
        Self {
            type_name: Some(property_type),
            ..Self::default()
        }
    }

    /// Creates a relationship spec with no authored targets.
    ///
    /// Relationships are uniform unless authored `varying`, as in OpenUSD:
    /// `SdfRelationshipSpec::New` defaults to `SdfVariabilityUniform`
    /// (`pxr/usd/sdf/relationshipSpec.h`), and the USDA parser gives `rel`
    /// statements uniform variability (`pxr/usd/sdf/textFileFormatParser.cpp`).
    #[must_use]
    pub fn relationship() -> Self {
        Self {
            kind: PropertyKind::Relationship,
            variability: Variability::Uniform,
            ..Self::default()
        }
    }

    /// Creates an empty spec of `kind`: [`Self::attribute`] or
    /// [`Self::relationship`].
    #[must_use]
    pub fn of_kind(kind: PropertyKind) -> Self {
        match kind {
            PropertyKind::Attribute => Self::attribute(),
            PropertyKind::Relationship => Self::relationship(),
        }
    }

    /// Returns `true` for an attribute spec.
    #[must_use]
    pub fn is_attribute(&self) -> bool {
        self.kind == PropertyKind::Attribute
    }

    /// Returns `true` for a relationship spec.
    #[must_use]
    pub fn is_relationship(&self) -> bool {
        self.kind == PropertyKind::Relationship
    }

    /// Marks the spec `custom` (builder, consuming).
    #[must_use]
    pub fn custom(mut self) -> Self {
        self.custom = true;
        self
    }

    /// Marks the spec `uniform` (builder, consuming).
    #[must_use]
    pub fn uniform(mut self) -> Self {
        self.variability = Variability::Uniform;
        self
    }

    /// Sets the declared type (builder, consuming).
    #[must_use]
    pub fn with_type(mut self, property_type: PropertyType) -> Self {
        self.type_name = Some(property_type);
        self
    }

    /// Sets the default value slot (builder, consuming).
    #[must_use]
    pub fn with_default(mut self, value: impl Into<Value>) -> Self {
        self.default = Some(value.into());
        self
    }

    /// Sets the time-sample slot (builder, consuming).
    ///
    /// Samples are sorted by time; for equal times the last one wins.
    #[must_use]
    pub fn with_time_samples(mut self, samples: Vec<TimeSample>) -> Self {
        self.time_samples = Some(sort_samples(samples));
        self
    }

    /// Sets the spline slot (builder, consuming).
    #[must_use]
    pub fn with_spline(mut self, spline: SplineData) -> Self {
        self.spline = Some(spline);
        self
    }

    /// Sets the target-path slot (builder, consuming): connection paths for
    /// an attribute, target paths for a relationship.
    #[must_use]
    pub fn with_targets(mut self, targets: ListOp<TargetPath>) -> Self {
        self.targets = Some(targets);
        self
    }

    /// Inserts or replaces a metadata field (builder, consuming).
    #[must_use]
    pub fn with_metadata(mut self, key: TokenId, value: impl Into<FieldValue>) -> Self {
        self.set_metadata(key, value);
        self
    }

    /// Inserts or replaces a metadata field.
    pub fn set_metadata(&mut self, key: TokenId, value: impl Into<FieldValue>) -> &mut Self {
        set_field_vec(&mut self.metadata, key, value.into());
        self
    }

    /// Returns an authored metadata field, if present.
    #[must_use]
    pub fn metadata(&self, key: TokenId) -> Option<&FieldValue> {
        get_field(&self.metadata, &key)
    }

    /// Removes an authored metadata field, returning its value.
    pub fn remove_metadata(&mut self, key: TokenId) -> Option<FieldValue> {
        remove_field(&mut self.metadata, key)
    }

    /// Returns `true` if the spec authors a default, samples or a spline,
    /// i.e. anything attribute value resolution can read.
    #[must_use]
    pub fn has_value(&self) -> bool {
        self.default.is_some()
            || self.time_samples.as_ref().is_some_and(|s| !s.is_empty())
            || self.spline.is_some()
    }
}

/// Sorts samples by time, keeping the last of any samples with equal times.
pub(crate) fn sort_samples(mut samples: Vec<TimeSample>) -> Vec<TimeSample> {
    // A stable sort keeps authored order among equal times, so the dedup pass
    // below can keep the last-authored sample.
    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<TimeSample> = Vec::with_capacity(samples.len());
    for sample in samples {
        match out.last_mut() {
            Some(last) if last.0.total_cmp(&sample.0).is_eq() => *last = sample,
            _ => out.push(sample),
        }
    }
    out
}

/// A named property spec on a prim or variant spec.
///
/// Spec: AOUSD Core §7.3.3 (a prim's attributes and relationships share one
/// name space: two properties of one prim cannot have the same name).
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyEntry {
    /// The interned property name, possibly namespaced (`primvars:st`).
    pub name: TokenId,
    /// The authored property spec.
    pub spec: PropertySpec,
}

/// Returns the property named `name`, if present.
#[must_use]
pub fn get_property(properties: &[PropertyEntry], name: TokenId) -> Option<&PropertySpec> {
    properties.iter().find(|e| e.name == name).map(|e| &e.spec)
}

/// Returns the property named `name` mutably, if present.
#[must_use]
pub fn get_property_mut(
    properties: &mut [PropertyEntry],
    name: TokenId,
) -> Option<&mut PropertySpec> {
    properties
        .iter_mut()
        .find(|e| e.name == name)
        .map(|e| &mut e.spec)
}

/// Inserts or replaces the property named `name`, keeping the position of a
/// replaced property (authored order).
pub fn set_property_vec(properties: &mut Vec<PropertyEntry>, name: TokenId, spec: PropertySpec) {
    if let Some(existing) = get_property_mut(properties, name) {
        *existing = spec;
    } else {
        properties.push(PropertyEntry { name, spec });
    }
}

/// Returns the property named `name`, creating an empty spec of `kind` (see
/// [`PropertySpec::of_kind`]) at the end of the list when absent.
pub fn property_entry(
    properties: &mut Vec<PropertyEntry>,
    name: TokenId,
    kind: PropertyKind,
) -> &mut PropertySpec {
    let index = match properties.iter().position(|e| e.name == name) {
        Some(index) => index,
        None => {
            properties.push(PropertyEntry {
                name,
                spec: PropertySpec::of_kind(kind),
            });
            properties.len() - 1
        }
    };
    &mut properties[index].spec
}

/// Removes the property named `name`, returning its spec.
pub fn remove_property(properties: &mut Vec<PropertyEntry>, name: TokenId) -> Option<PropertySpec> {
    let index = properties.iter().position(|e| e.name == name)?;
    Some(properties.remove(index).spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn slots_are_independent() {
        let mut spec = PropertySpec::attribute()
            .with_default(Value::Float(1.0))
            .with_time_samples(vec![(1.0, Value::Float(3.0)), (0.0, Value::Float(2.0))]);
        assert!(spec.has_value());
        assert_eq!(
            spec.time_samples.as_deref(),
            Some(&[(0.0, Value::Float(2.0)), (1.0, Value::Float(3.0))][..]),
            "samples are kept sorted by time"
        );

        spec.time_samples = None;
        assert_eq!(spec.default, Some(Value::Float(1.0)));
        spec.default = None;
        assert!(!spec.has_value());
    }

    #[test]
    fn duplicate_sample_times_keep_the_last_authored() {
        let samples = sort_samples(vec![
            (1.0, Value::Int(1)),
            (0.0, Value::Int(0)),
            (1.0, Value::Int(2)),
        ]);
        assert_eq!(samples, vec![(0.0, Value::Int(0)), (1.0, Value::Int(2))]);
    }

    #[test]
    fn empty_samples_are_not_a_value() {
        let spec = PropertySpec::attribute().with_time_samples(Vec::new());
        assert!(spec.time_samples.is_some());
        assert!(!spec.has_value());
    }

    #[test]
    fn property_list_keeps_authored_order() {
        let mut tokens = crate::interner::TokenInterner::default();
        let b = tokens.intern("b");
        let a = tokens.intern("a");
        let mut properties = Vec::new();
        property_entry(&mut properties, b, PropertyKind::Attribute).default = Some(Value::Int(1));
        property_entry(&mut properties, a, PropertyKind::Relationship);
        property_entry(&mut properties, b, PropertyKind::Attribute).custom = true;

        let names: Vec<_> = properties.iter().map(|e| e.name).collect();
        assert_eq!(names, vec![b, a]);
        let b_spec = get_property(&properties, b).expect("b");
        assert!(b_spec.custom);
        assert_eq!(b_spec.default, Some(Value::Int(1)));
        assert!(get_property(&properties, a).expect("a").is_relationship());

        assert!(remove_property(&mut properties, b).is_some());
        assert!(get_property(&properties, b).is_none());
    }
}
