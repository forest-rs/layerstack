// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored property specs: attributes and relationships.
//!
//! A property spec keeps its identity and kind apart from the authored slots
//! it may carry. Every slot is stored independently and may be absent, so an
//! attribute can hold a default value, time samples, a spline and connection
//! paths at the same time, exactly as a layer can author them. Nothing is
//! folded at ingestion time: which slot answers a query is decided when the
//! query runs (see [`crate::Attribute::get`], which reads at a [`Time`]).
//!
//! Spec: AOUSD Core §7.3.7 (property specs), §7.6.3 (property spec fields),
//! §7.6.4 (attribute spec fields; §7.6.4.2.3 notes that attributes "may have a
//! value, have a connection, or both"), §7.6.5 (relationship spec fields).
//! OpenUSD stores the same fields separately on `SdfAttributeSpec`
//! (`pxr/usd/sdf/attributeSpec.h`) and `SdfPropertySpec`
//! (`pxr/usd/sdf/propertySpec.h`); the field set per spec form is registered in
//! `pxr/usd/sdf/schema.cpp`.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::{
    SharedVec,
    doc::{
        FieldEntry, FieldValue, InterpolationType, Value, get_field, remove_field, set_field_vec,
    },
    interner::TokenId,
    listop::ListOp,
    path::TargetPath,
    spline::SplineData,
    value_type::{UnknownValueType, ValueType},
};

/// When a composed property or scene computation is read: the default time, or a time code
/// with the interpolation between its time samples.
///
/// OpenUSD: `UsdTimeCode` with the stage's `UsdInterpolationType`; linear
/// is OpenUSD's default.
///
/// Spec: AOUSD Core §12.3 (default values and time samples), §12.5
/// (interpolation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Time {
    /// The default time: only default values, never time samples.
    Default,
    /// A time code, interpolating between time samples as `interpolation`
    /// says.
    At {
        /// The time code.
        code: f64,
        /// How values between time samples are interpolated.
        interpolation: InterpolationType,
    },
}

impl Time {
    /// The time code `code`, interpolating linearly (OpenUSD's default).
    #[must_use]
    pub fn at(code: f64) -> Self {
        Self::At {
            code,
            interpolation: InterpolationType::Linear,
        }
    }

    /// The time code `code`, holding each time sample until the next.
    #[must_use]
    pub fn held(code: f64) -> Self {
        Self::At {
            code,
            interpolation: InterpolationType::Held,
        }
    }
}

/// The value type an attribute declares (`typeName`): a registered
/// [`ValueType`], scalar or array (`color3f`, `color3f[]`).
///
/// [`PropertyType::parse`] takes a type name through the value type
/// table, so the name, its array form and the default scalar always agree.
/// A layer may author a name the table lacks, which OpenUSD's readers keep;
/// a reader keeps it as [`PropertyType::unregistered`], and edits reject
/// such a type
/// ([`Rejection::UnknownValueType`](crate::edit::Rejection::UnknownValueType)).
///
/// ```
/// use layerstack::{PropertyType, Value, ValueType};
///
/// let colors = PropertyType::parse("color3f[]").unwrap();
/// assert_eq!(colors.type_name(), "color3f[]");
/// assert_eq!(colors.value_type(), Some(ValueType::Color3f));
/// assert!(colors.is_array());
/// assert_eq!(colors.default_scalar(), &Value::Vec3f([0.0; 3]));
/// assert!(PropertyType::parse("float5").is_err());
/// ```
///
/// A registered type whose zero owns no allocation is a few bytes naming
/// its table entry, as OpenUSD's handle into its registry is; any other
/// type keeps its name and zero in one allocation its clones share.
///
/// OpenUSD: `SdfValueTypeName`, which `SdfSchema::FindType` looks up.
///
/// Spec: AOUSD Core §6.2 (value types), §7.6.4.1.1 (`typeName`).
#[derive(Clone)]
pub struct PropertyType(TypeRepr);

/// How a [`PropertyType`] is held.
#[derive(Clone)]
enum TypeRepr {
    /// A registered type whose zero is a constant
    /// ([`ValueType::constant_zero`]): its name and zero come from the
    /// value type table.
    Registered {
        value_type: ValueType,
        is_array: bool,
        zero: &'static Value,
    },
    /// Any other type, with its zero beside it.
    Kept(Arc<KeptType>),
}

/// A [`PropertyType`] that keeps its zero: a registered text or matrix
/// type, or a name no registered type has.
struct KeptType {
    of: KeptOf,
    is_array: bool,
    default_scalar: Value,
}

/// What a [`KeptType`] is a type of.
enum KeptOf {
    /// A registered type, whose name the value type table holds.
    Registered(ValueType),
    /// A name no registered type has.
    Unregistered(Arc<str>),
}

impl PropertyType {
    /// The type named `type_name`: a registered value type's name, or the
    /// name of its array form (`color3f[]`).
    ///
    /// # Errors
    ///
    /// [`UnknownValueType`] when no registered type has the name, including
    /// `opaque[]` and `group[]`, which have no array form.
    pub fn parse(type_name: &str) -> Result<Self, UnknownValueType> {
        let (value_type, is_array) =
            ValueType::parse(type_name).ok_or_else(|| UnknownValueType::new(type_name))?;
        Ok(Self::registered(value_type, is_array))
    }

    /// The type of a type name a layer authors that no registered type has,
    /// as a reader keeps it: its name is the unknown one, it is an array
    /// when the name ends in `[]`, and its default scalar is
    /// [`Value::Null`], which any value conforms to. Edits reject it.
    ///
    /// OpenUSD's readers keep a type name they do not know, and its
    /// authoring calls reject one (`SdfAttributeSpec::New`).
    #[must_use]
    pub fn unregistered(unknown: UnknownValueType) -> Self {
        let type_name = unknown.into_type_name();
        Self(TypeRepr::Kept(Arc::new(KeptType {
            is_array: type_name.ends_with("[]"),
            of: KeptOf::Unregistered(type_name),
            default_scalar: Value::Null,
        })))
    }

    /// The registered type `value_type`, its array form when `is_array`
    /// (which the caller checks it has).
    pub(crate) fn registered(value_type: ValueType, is_array: bool) -> Self {
        Self(match value_type.constant_zero() {
            Some(zero) => TypeRepr::Registered {
                value_type,
                is_array,
                zero,
            },
            None => TypeRepr::Kept(Arc::new(KeptType {
                of: KeptOf::Registered(value_type),
                is_array,
                default_scalar: value_type.zero(),
            })),
        })
    }

    /// The type name, as a layer writes it: `color3f[]` for an array.
    #[must_use]
    pub fn type_name(&self) -> &str {
        let (value_type, is_array) = match &self.0 {
            TypeRepr::Registered {
                value_type,
                is_array,
                ..
            } => (*value_type, *is_array),
            TypeRepr::Kept(kept) => match &kept.of {
                KeptOf::Registered(value_type) => (*value_type, kept.is_array),
                KeptOf::Unregistered(name) => return name,
            },
        };
        match value_type.array_name() {
            Some(array) if is_array => array,
            _ => value_type.name(),
        }
    }

    /// The registered value type, of one element for an array; `None` for
    /// an [unregistered](Self::unregistered) type.
    #[must_use]
    pub fn value_type(&self) -> Option<ValueType> {
        match &self.0 {
            TypeRepr::Registered { value_type, .. } => Some(*value_type),
            TypeRepr::Kept(kept) => match kept.of {
                KeptOf::Registered(value_type) => Some(value_type),
                KeptOf::Unregistered(_) => None,
            },
        }
    }

    /// Whether the type is an array type.
    #[must_use]
    pub fn is_array(&self) -> bool {
        match &self.0 {
            TypeRepr::Registered { is_array, .. } => *is_array,
            TypeRepr::Kept(kept) => kept.is_array,
        }
    }

    /// The zero value of one element ([`ValueType::zero`]), which sparse
    /// array edits append when they grow an array (`minsize`, `resize`);
    /// [`Value::Null`] for an unregistered type.
    #[must_use]
    pub fn default_scalar(&self) -> &Value {
        match &self.0 {
            TypeRepr::Registered { zero, .. } => zero,
            TypeRepr::Kept(kept) => &kept.default_scalar,
        }
    }

    /// Returns the declared default value for the whole property.
    #[must_use]
    pub fn default_property_value(&self) -> Value {
        if self.is_array() {
            Value::array_with_element(Vec::default(), Some(self.default_scalar()))
        } else {
            self.default_scalar().clone()
        }
    }

    /// Returns the default scalar element for an array property.
    #[must_use]
    pub fn default_array_element(&self) -> Option<Value> {
        self.is_array().then(|| self.default_scalar().clone())
    }
}

impl PartialEq for PropertyType {
    /// Types are equal when their names are: a name settles the value
    /// type, the array form and the zero.
    fn eq(&self, other: &Self) -> bool {
        self.type_name() == other.type_name()
            && self.value_type() == other.value_type()
            && self.is_array() == other.is_array()
    }
}

impl core::fmt::Debug for PropertyType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PropertyType")
            .field("type_name", &self.type_name())
            .field("value_type", &self.value_type())
            .field("is_array", &self.is_array())
            .field("default_scalar", self.default_scalar())
            .finish()
    }
}

impl core::str::FromStr for PropertyType {
    type Err = UnknownValueType;

    /// See [`PropertyType::parse`].
    fn from_str(type_name: &str) -> Result<Self, Self::Err> {
        Self::parse(type_name)
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
    /// Authored time samples, sorted by time. Clones share the buffer; use
    /// [`SharedVec::make_mut`](crate::SharedVec::make_mut) to edit it.
    ///
    /// `Some(SharedVec::new())` records an explicitly authored empty sample map, which
    /// contributes no value.
    ///
    /// Spec: AOUSD Core §7.6.4.2.2 (`timeSamples`).
    pub time_samples: Option<SharedVec<TimeSample>>,
    /// Authored spline, boxed: few attributes author one.
    ///
    /// Spec: AOUSD Core §7.6.4.2.4 (`spline`).
    pub spline: Option<Box<SplineData>>,
    /// Authored target paths: `connectionPaths` for an attribute,
    /// `targetPaths` for a relationship.
    ///
    /// Connections are inspected separately from attribute value resolution
    /// (see [`crate::Relationship::targets`]).
    ///
    /// Spec: AOUSD Core §7.6.4.2.3 (`connectionPaths`), §7.6.5.1.1
    /// (`targetPaths`), §12.4. Boxed: most properties are attributes
    /// without connections.
    pub targets: Option<Box<ListOp<TargetPath>>>,
    /// Shared authored property metadata, in authored order: for example
    /// `interpolation`, `elementSize`, `customData`, `displayName`, `doc`
    /// (stored as `documentation`), `limits` and `colorSpace`.
    /// Use [`Self::set_metadata`] or [`Self::remove_metadata`] to edit one
    /// snapshot without copying its untouched samples.
    ///
    /// Spec: AOUSD Core §7.4 (metadata fields), §7.6.3.2–§7.6.3.3.
    pub metadata: SharedVec<FieldEntry>,
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
        self.time_samples = Some(sort_samples(samples).into());
        self
    }

    /// Sets the spline slot (builder, consuming).
    #[must_use]
    pub fn with_spline(mut self, spline: SplineData) -> Self {
        self.spline = Some(Box::new(spline));
        self
    }

    /// Sets the target-path slot (builder, consuming): connection paths for
    /// an attribute, target paths for a relationship.
    #[must_use]
    pub fn with_targets(mut self, targets: ListOp<TargetPath>) -> Self {
        self.targets = Some(Box::new(targets));
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
        set_field_vec(self.metadata.make_mut(), key, value.into());
        self
    }

    /// Returns an authored metadata field, if present.
    #[must_use]
    pub fn metadata(&self, key: TokenId) -> Option<&FieldValue> {
        get_field(&self.metadata, &key)
    }

    /// Removes an authored metadata field, returning its value.
    pub fn remove_metadata(&mut self, key: TokenId) -> Option<FieldValue> {
        self.metadata(key)?;
        remove_field(self.metadata.make_mut(), key)
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
    /// The authored property snapshot, shared with composed opinions. Use
    /// [`Arc::make_mut`] to edit this record without changing other snapshots.
    /// Samples and metadata detach independently when those slots are edited.
    pub spec: Arc<PropertySpec>,
}

/// Returns the property named `name`, if present.
#[must_use]
pub fn get_property(properties: &[PropertyEntry], name: TokenId) -> Option<&PropertySpec> {
    properties.iter().find(|e| e.name == name).map(|e| &*e.spec)
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
        .map(|e| Arc::make_mut(&mut e.spec))
}

/// Inserts or replaces the property named `name`, keeping the position of a
/// replaced property (authored order).
pub fn set_property_vec(properties: &mut Vec<PropertyEntry>, name: TokenId, spec: PropertySpec) {
    if let Some(existing) = properties.iter_mut().find(|entry| entry.name == name) {
        existing.spec = Arc::new(spec);
    } else {
        properties.push(PropertyEntry {
            name,
            spec: Arc::new(spec),
        });
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
                spec: Arc::new(PropertySpec::of_kind(kind)),
            });
            properties.len() - 1
        }
    };
    Arc::make_mut(&mut properties[index].spec)
}

/// Removes the property named `name`, returning its spec.
pub fn remove_property(properties: &mut Vec<PropertyEntry>, name: TokenId) -> Option<PropertySpec> {
    let index = properties.iter().position(|e| e.name == name)?;
    Some(Arc::unwrap_or_clone(properties.remove(index).spec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn types_parse_through_the_value_type_table() {
        let colors = PropertyType::parse("color3f[]").unwrap();
        assert_eq!(colors.type_name(), "color3f[]");
        assert_eq!(colors.value_type(), Some(ValueType::Color3f));
        assert!(colors.is_array());
        assert_eq!(colors.default_scalar(), &Value::Vec3f([0.0; 3]));
        let float = PropertyType::parse("float").unwrap();
        assert!(!float.is_array());
        assert_eq!(float.default_scalar(), &Value::Float(0.0));
        assert_eq!(
            "token".parse::<PropertyType>().unwrap().type_name(),
            "token"
        );
        for name in ["float5", "opaque[]", "color3f[][]", "Vec3f", ""] {
            let unknown = PropertyType::parse(name).unwrap_err();
            assert_eq!(unknown.type_name(), name);
        }
    }

    #[test]
    fn every_registered_type_reads_back_from_its_handle() {
        // A property spec keeps one per attribute: two words, whatever
        // the type.
        assert_eq!(size_of::<Option<PropertyType>>(), 2 * size_of::<usize>());
        for &ty in ValueType::ALL {
            let names = [Some(ty.name()), ty.array_name()];
            for (is_array, name) in [false, true].into_iter().zip(names) {
                let Some(name) = name else { continue };
                let parsed = PropertyType::parse(name).unwrap();
                assert_eq!(parsed.type_name(), name);
                assert_eq!(parsed.value_type(), Some(ty));
                assert_eq!(parsed.is_array(), is_array);
                assert_eq!(parsed.default_scalar(), &ty.zero(), "{name}");
                assert_eq!(parsed, parsed.clone());
            }
        }
        let text = PropertyType::parse("asset[]").unwrap();
        assert_eq!(text.default_scalar(), &Value::Asset(Arc::from("")));
        assert_ne!(text, PropertyType::parse("asset").unwrap());
        assert_ne!(text, PropertyType::parse("string[]").unwrap());
    }

    #[test]
    fn an_unregistered_type_keeps_its_name() {
        let unknown = PropertyType::parse("notatype[]").unwrap_err();
        assert_eq!(
            alloc::string::ToString::to_string(&unknown),
            "`notatype[]` is not a registered value type"
        );
        let kept = PropertyType::unregistered(unknown);
        assert_eq!(kept.type_name(), "notatype[]");
        assert_eq!(kept.value_type(), None);
        assert!(kept.is_array());
        assert_eq!(kept.default_scalar(), &Value::Null);
    }

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
        let tokens = crate::interner::TokenInterner::default();
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
