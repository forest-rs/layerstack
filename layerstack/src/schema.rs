// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schemas and the prim definitions they build.
//!
//! A schema ascribes properties, each with a declared type, variability and
//! optional fallback, to composed prims. A *typed* schema is the prim's
//! type (its `typeName`), with single inheritance of properties from its
//! parent, and ancestry that additional bases can widen as a hand-written
//! `plugInfo.json`'s `bases` do ([`SchemaRegistry::ancestors`]); an
//! *applied* schema is
//! named in the prim's `apiSchemas` list op, once for a single-apply schema
//! and once per instance name for a multiple-apply one.
//!
//! Register [`SchemaDefinition`]s with a [`SchemaRegistryBuilder`] and build
//! a [`SchemaRegistry`]. Building computes each schema's own
//! [`PrimDefinition`] once, with its built-ins, auto-applies and override
//! properties composed in; [`SchemaRegistry::prim_definition`] then composes
//! a composed prim's definition from its type and applied schemas. A stage
//! composed with a registry ([`crate::StageOptions::schemas`]) answers the
//! same queries per prim ([`crate::Prim::definition`]) and resolves
//! fallback values through it.
//!
//! Multiple-apply schemas are templates: their property names, and the
//! schemas they include, hold [`INSTANCE_NAME_PLACEHOLDER`] where the
//! instance name goes, as OpenUSD's generated schemas do.
//! [`read_generated_schema`] reads definitions from such a layer
//! (OpenUSD's `generatedSchema.usda`).
//!
//! Every [`TokenId`] a registry holds is interned in the token interner of
//! the store whose stages use it.
//!
//! Spec: AOUSD Core §13.3 (schema types), §13.3.1 (typed schemas), §13.3.2
//! (applied schemas), §13.3.2.1 (inclusions), §13.3.2.2 (override
//! properties), §13.3.2.3 (the prim definition), §13.3.2.4 (fallback value
//! resolution). OpenUSD: `UsdSchemaRegistry` (`pxr/usd/usd/schemaRegistry.cpp`)
//! and `UsdPrimDefinition` (`pxr/usd/usd/primDefinition.cpp`).

mod definition;
mod generated;
mod metadata;

pub use metadata::{
    ListOpItem, MetadataConflict, MetadataDefinition, MetadataEditError, MetadataEditReason,
    MetadataField, MetadataOrigin, MetadataTarget, MetadataType, is_spec_field,
};

pub use definition::{AppliedSchema, PrimDefinition};
pub use generated::{GeneratedSchemaError, SchemaDeclaration, read_generated_schema};

use alloc::{format, string::String, sync::Arc, vec, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    doc::{FieldEntry, FieldValue, Value},
    interner::{TokenId, TokenInterner},
    property::{PropertyKind, PropertyType, Variability},
};

/// The placeholder a multiple-apply schema's property names, and the names
/// of the schemas it includes, hold where the instance name goes: applying
/// `SlotAPI:left` turns `slot:__INSTANCE_NAME__:index` into
/// `slot:left:index`.
///
/// The placeholder must be a whole namespace segment; the first such
/// segment is replaced.
///
/// Spec: AOUSD Core §13.3.2 leaves the placeholder to the implementation;
/// this is OpenUSD's (`UsdSchemaRegistry::MakeMultipleApplyNameInstance`).
pub const INSTANCE_NAME_PLACEHOLDER: &str = "__INSTANCE_NAME__";

/// What kind of schema a [`SchemaDefinition`] is.
///
/// Spec: AOUSD Core §13.3 (typed and applied schemas), §13.3.1 (abstract
/// and concrete typed schemas), §13.3.2 (single and multiple applied
/// schemas).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SchemaKind {
    /// A typed schema a prim's `typeName` may name.
    ConcreteTyped,
    /// A typed schema only other typed schemas inherit from. A prim whose
    /// `typeName` names it is typeless.
    AbstractTyped,
    /// An applied schema applied at most once, by its name.
    SingleApplyApi,
    /// An applied schema applied once per instance name, as
    /// `Name:instance`.
    MultipleApplyApi,
}

impl SchemaKind {
    /// Whether the schema is typed (concrete or abstract).
    #[must_use]
    pub fn is_typed(self) -> bool {
        matches!(self, Self::ConcreteTyped | Self::AbstractTyped)
    }

    /// Whether the schema is applied (single or multiple).
    #[must_use]
    pub fn is_applied(self) -> bool {
        !self.is_typed()
    }
}

/// A property a schema defines: its kind, declared type, variability and
/// fallback value and metadata.
///
/// ```
/// use layerstack::{PropertyDefinition, PropertyType, TokenInterner, Value};
///
/// let tokens = TokenInterner::default();
/// let width = PropertyDefinition::attribute(tokens.intern("width"))
///     .with_type(PropertyType::parse("float").unwrap())
///     .with_fallback(Value::Float(1.0));
/// assert_eq!(width.fallback, Some(Value::Float(1.0)));
/// ```
///
/// Spec: AOUSD Core §13.3 (schema properties), §12.3.5 (fallback values).
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDefinition {
    /// The property name. In a multiple-apply schema it holds
    /// [`INSTANCE_NAME_PLACEHOLDER`].
    pub name: TokenId,
    /// Attribute or relationship.
    pub kind: PropertyKind,
    /// The declared value type of an attribute; `None` for a relationship
    /// or an attribute declared without one.
    pub type_name: Option<PropertyType>,
    /// The declared variability, which a prim the schema applies to has
    /// whatever its opinions author.
    ///
    /// Spec: AOUSD Core §12.2.3 (variability).
    pub variability: Variability,
    /// The value the property resolves to when no opinion authors one, if
    /// the schema gives one.
    ///
    /// Spec: AOUSD Core §12.3.5 (fallback values), §13.3.2.4.
    pub fallback: Option<Value>,
    /// Metadata fallbacks, composed in schema strength order. Runtime fields
    /// such as `customData`, time samples and connections are excluded when
    /// reading generated schemas. Structural fields remain in their dedicated
    /// members above. Struct literals must initialize this field (usually empty).
    ///
    /// Spec: AOUSD Core §13.3.2.3–§13.3.2.4; OpenUSD
    /// `UsdPrimDefinition::ListPropertyMetadataFields`.
    pub metadata: Vec<FieldEntry>,
}

impl PropertyDefinition {
    /// A varying attribute with no declared type and no fallback.
    #[must_use]
    pub fn attribute(name: TokenId) -> Self {
        Self {
            name,
            kind: PropertyKind::Attribute,
            type_name: None,
            variability: Variability::Varying,
            fallback: None,
            metadata: Vec::new(),
        }
    }

    /// A relationship. Relationships are uniform and have no fallback.
    #[must_use]
    pub fn relationship(name: TokenId) -> Self {
        Self {
            name,
            kind: PropertyKind::Relationship,
            type_name: None,
            variability: Variability::Uniform,
            fallback: None,
            metadata: Vec::new(),
        }
    }

    /// Declares the attribute's value type (builder).
    #[must_use]
    pub fn with_type(mut self, type_name: PropertyType) -> Self {
        self.type_name = Some(type_name);
        self
    }

    /// Gives the attribute a fallback value (builder).
    #[must_use]
    pub fn with_fallback(mut self, fallback: impl Into<Value>) -> Self {
        self.fallback = Some(fallback.into());
        self
    }

    /// Sets a schema property metadata fallback (builder).
    #[must_use]
    pub fn with_metadata(mut self, key: TokenId, value: impl Into<FieldValue>) -> Self {
        crate::doc::set_field_vec(&mut self.metadata, key, value.into());
        self
    }

    /// Looks up a composed schema property metadata fallback.
    #[must_use]
    pub fn metadata(&self, key: TokenId) -> Option<&FieldValue> {
        crate::doc::get_field(&self.metadata, &key)
    }

    /// Makes the attribute `uniform` (builder).
    #[must_use]
    pub fn uniform(mut self) -> Self {
        self.variability = Variability::Uniform;
        self
    }

    /// Whether `other` has this property's kind and, for attributes, its
    /// declared type: only then may one definition of a property compose
    /// with another.
    ///
    /// OpenUSD: `UsdPrimDefinition::_PropertyTypesMatch`.
    #[must_use]
    pub fn has_type_of(&self, other: &Self) -> bool {
        fn declared(p: &PropertyDefinition) -> Option<&str> {
            p.type_name.as_ref().map(PropertyType::type_name)
        }
        self.kind == other.kind
            && (self.kind == PropertyKind::Relationship || declared(self) == declared(other))
    }
}

/// A typed or applied schema: its properties, inclusions and override
/// properties.
///
/// ```
/// use layerstack::{PropertyDefinition, SchemaDefinition, SchemaKind, TokenInterner, Value};
///
/// let tokens = TokenInterner::default();
/// let shape = SchemaDefinition::new(tokens.intern("Shape"), SchemaKind::AbstractTyped)
///     .with_property(PropertyDefinition::attribute(tokens.intern("sides")).with_fallback(3));
/// let tile = SchemaDefinition::typed(tokens.intern("Tile"))
///     .with_parent(shape.name)
///     .with_built_in(tokens.intern("LabelAPI"));
/// assert_eq!(tile.kind, SchemaKind::ConcreteTyped);
/// ```
///
/// Spec: AOUSD Core §13.3 (schema types), §13.3.2.1 (inclusions),
/// §13.3.2.2 (override properties).
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaDefinition {
    /// The schema's name: a type name for a typed schema, the name
    /// `apiSchemas` lists for an applied one.
    pub name: TokenId,
    /// Typed or applied, and which of each.
    pub kind: SchemaKind,
    /// The typed schema this typed schema inherits from, whose properties
    /// and built-ins it has too, weaker than its own (unless it is
    /// [flattened](SchemaDefinition::flattened)): the class usdGenSchema's
    /// single `inherits` names, and the first of the `bases` its
    /// `plugInfo.json` declares.
    ///
    /// Spec: AOUSD Core §13.3.1 (single inheritance).
    pub parent: Option<TokenId>,
    /// Further typed schemas the schema is, after its parent, in the order
    /// a hand-written `plugInfo.json` lists them in `bases` (usdGenSchema
    /// writes one base only). They give ancestry and nothing else: the
    /// schema [`is_a`](SchemaRegistry::is_a) each of them and their
    /// ancestors, so the auto-applies and `apiSchemaCanOnlyApplyTo` lists
    /// naming them reach it, and they order its
    /// [`ancestors`](SchemaRegistry::ancestors); no property, built-in or
    /// override of theirs composes in, as none does in OpenUSD, whose prim
    /// definition is the generated class alone.
    ///
    /// OpenUSD: `TfType` bases (`PlugPlugin::_DeclareType`), which
    /// `UsdPrim::IsA`, `UsdPrim::CanApplyAPI` and the auto-applies
    /// (`TfType::GetAllDerivedTypes`) follow.
    pub additional_bases: Vec<TokenId>,
    /// Whether [`SchemaDefinition::properties`], [`SchemaDefinition::built_ins`]
    /// and [`SchemaDefinition::overrides`] already hold those of the schemas
    /// it inherits from, as each class of a generated schema layer does
    /// (usdGenSchema flattens them in). The registry then composes none of
    /// its parent's in: OpenUSD builds a typed schema's prim definition from
    /// its generated class alone, so what the class leaves out it lacks.
    ///
    /// `false` for a definition built by hand: its parent chain's
    /// properties, built-ins and overrides compose in, weaker, up to and
    /// including the first flattened ancestor, as usdGenSchema would
    /// flatten them. [`read_generated_schema`] and `layerstack_schemas`'
    /// tables set it.
    ///
    /// OpenUSD: `UsdSchemaRegistry::_SchemaDefInitHelper::_PopulateTypedPrimDefinitions`
    /// (`schemaRegistry.cpp`), `usdGenSchema.py`'s `GenerateRegistry`.
    pub flattened: bool,
    /// The applied schemas this schema includes as built-ins, strongest
    /// first. A typed or single-apply schema names a single-apply schema,
    /// or a multiple-apply schema with an instance name (`SlotAPI:main`). A
    /// multiple-apply schema names another multiple-apply schema by type
    /// (`PinAPI`, applied with the same instance name) or by named
    /// instance (`PinAPI:extra`, applied as `instance:extra`).
    ///
    /// Spec: AOUSD Core §13.3.2.1 (built-ins).
    pub built_ins: Vec<TokenId>,
    /// The properties the schema defines.
    pub properties: Vec<PropertyDefinition>,
    /// Override properties: overs on properties the schema's built-ins
    /// define, replacing their fallback. An override does not define a
    /// property; one whose property the definition lacks, or has with
    /// another type, is ignored, and its variability always is.
    ///
    /// Spec: AOUSD Core §13.3.2.2 (override properties).
    pub overrides: Vec<PropertyDefinition>,
    /// For an applied schema: the typed schemas a prim's type may be, or
    /// derive from, and the applied schemas the prim may have, for the
    /// schema to be applied to it; empty when it may be applied to any
    /// prim. A multiple-apply schema named with an instance
    /// (`SlotAPI:main`) requires that instance; named alone, any instance.
    ///
    /// This restricts authoring ([`SchemaRegistry::can_apply`]), never
    /// composition: a prim whose `apiSchemas` apply the schema anyway has
    /// it. OpenUSD: `apiSchemaCanOnlyApplyTo`
    /// (`UsdSchemaRegistry::GetAPISchemaCanOnlyApplyToTypeNames`).
    pub can_only_apply_to: Vec<TokenId>,
    /// For a multiple-apply schema: the only instance names it may be
    /// applied with; empty when any valid instance name may be used.
    ///
    /// OpenUSD: `apiSchemaAllowedInstanceNames`.
    pub allowed_instance_names: Vec<TokenId>,
    /// For a multiple-apply schema: instance names whose own
    /// [`SchemaDefinition::can_only_apply_to`] list replaces the schema's.
    /// An empty list replaces nothing.
    ///
    /// OpenUSD: `apiSchemaInstances` entries' `apiSchemaCanOnlyApplyTo`.
    pub instance_can_only_apply_to: Vec<(TokenId, Vec<TokenId>)>,
}

impl SchemaDefinition {
    /// A schema of `kind` with nothing defined yet.
    #[must_use]
    pub fn new(name: TokenId, kind: SchemaKind) -> Self {
        Self {
            name,
            kind,
            parent: None,
            additional_bases: Vec::new(),
            flattened: false,
            built_ins: Vec::new(),
            properties: Vec::new(),
            overrides: Vec::new(),
            can_only_apply_to: Vec::new(),
            allowed_instance_names: Vec::new(),
            instance_can_only_apply_to: Vec::new(),
        }
    }

    /// A concrete typed schema.
    #[must_use]
    pub fn typed(name: TokenId) -> Self {
        Self::new(name, SchemaKind::ConcreteTyped)
    }

    /// A single-apply API schema.
    #[must_use]
    pub fn api(name: TokenId) -> Self {
        Self::new(name, SchemaKind::SingleApplyApi)
    }

    /// Sets the typed schema this one inherits from (builder).
    #[must_use]
    pub fn with_parent(mut self, parent: TokenId) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Adds a typed schema this one also is, after its parent and the
    /// bases added before ([`SchemaDefinition::additional_bases`])
    /// (builder).
    #[must_use]
    pub fn with_additional_base(mut self, base: TokenId) -> Self {
        self.additional_bases.push(base);
        self
    }

    /// Marks the definition as already holding its ancestors' properties,
    /// built-ins and overrides ([`SchemaDefinition::flattened`]) (builder).
    #[must_use]
    pub fn flattened(mut self) -> Self {
        self.flattened = true;
        self
    }

    /// Adds a built-in applied schema, weaker than those added before it
    /// (builder).
    #[must_use]
    pub fn with_built_in(mut self, api: TokenId) -> Self {
        self.built_ins.push(api);
        self
    }

    /// Adds a property (builder).
    #[must_use]
    pub fn with_property(mut self, property: PropertyDefinition) -> Self {
        self.properties.push(property);
        self
    }

    /// Adds an override property (builder).
    #[must_use]
    pub fn with_override(mut self, property: PropertyDefinition) -> Self {
        self.overrides.push(property);
        self
    }

    /// Adds a typed schema this applied schema may be applied to, or an
    /// applied schema (with an instance, as `Name:instance`, for a
    /// multiple-apply one) a prim it is applied to may have
    /// ([`SchemaDefinition::can_only_apply_to`]) (builder).
    #[must_use]
    pub fn with_can_only_apply_to(mut self, name: TokenId) -> Self {
        self.can_only_apply_to.push(name);
        self
    }

    /// Adds an instance name this multiple-apply schema may be applied with
    /// ([`SchemaDefinition::allowed_instance_names`]) (builder).
    #[must_use]
    pub fn with_allowed_instance_name(mut self, instance: TokenId) -> Self {
        self.allowed_instance_names.push(instance);
        self
    }
}

/// Why an applied schema cannot be applied to a prim
/// ([`SchemaRegistry::can_apply`], [`crate::Prim::can_apply`]).
///
/// OpenUSD: `UsdPrim::CanApplyAPI`, whose `whyNot` these name.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CannotApply {
    /// No prim is at the path.
    NoSuchPrim,
    /// The name is not a registered applied schema.
    NotAnAppliedSchema,
    /// A multiple-apply schema was named without an instance name.
    MissingInstanceName,
    /// A single-apply schema was named with an instance name.
    UnexpectedInstanceName,
    /// The instance name is not one the schema allows
    /// ([`SchemaRegistry::is_allowed_instance_name`]).
    InstanceNameNotAllowed,
    /// The prim's type is none of the typed schemas named here and derives
    /// from none of them (a typeless prim's never does), and the prim has
    /// none of the applied schemas named here
    /// ([`SchemaRegistry::can_only_apply_to`]).
    NotATarget {
        /// The typed and applied schemas the schema requires, any one of
        /// them.
        allowed: Vec<TokenId>,
    },
}

impl core::fmt::Display for CannotApply {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::NoSuchPrim => "no prim is at the path",
            Self::NotAnAppliedSchema => "not a registered applied schema",
            Self::MissingInstanceName => "a multiple-apply schema needs an instance name",
            Self::UnexpectedInstanceName => "a single-apply schema takes no instance name",
            Self::InstanceNameNotAllowed => "the schema does not allow this instance name",
            Self::NotATarget { .. } => {
                "the prim has neither a type nor an applied schema the schema can apply to"
            }
        })
    }
}

impl core::error::Error for CannotApply {}

/// Something a [`SchemaRegistryBuilder::build`] skipped, and why. Building
/// never fails: what an issue names is left out, as OpenUSD leaves it out
/// with a warning.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaIssue {
    /// `schema` includes (or is auto-applied) `included`, which names no
    /// registered applied schema.
    UnknownInclusion {
        /// The including schema.
        schema: TokenId,
        /// The included name, as authored.
        included: TokenId,
    },
    /// `schema` includes `included` in a form AOUSD Core §13.3.2.1 does not
    /// allow: a multiple-apply schema without an instance name in a typed or
    /// single-apply schema, a single-apply schema with an instance name, or
    /// anything but another multiple-apply schema in a multiple-apply one.
    InvalidInclusion {
        /// The including schema.
        schema: TokenId,
        /// The included name, as authored.
        included: TokenId,
    },
    /// Including `included` in `schema`'s definition would include a
    /// schema already being built there; the inclusion is skipped, as in
    /// OpenUSD.
    InclusionCycle {
        /// The including schema.
        schema: TokenId,
        /// The included schema.
        included: TokenId,
    },
    /// `schema` overrides `property`, which its definition does not define
    /// with the override's kind and type; the override is ignored.
    ///
    /// Spec: AOUSD Core §13.3.2.2.
    IgnoredOverride {
        /// The overriding schema.
        schema: TokenId,
        /// The overridden property.
        property: TokenId,
    },
    /// An auto-apply names a target that is not a registered schema.
    UnknownAutoApplyTarget {
        /// The auto-applied schema.
        schema: TokenId,
        /// The target, as given.
        target: TokenId,
    },
    /// `schema`'s parent or one of its additional bases is left out of its
    /// ancestry, for `reason`; the rest of its bases still count.
    ///
    /// OpenUSD: a base without schema info fails the registry's
    /// verification ("Could not find schema info for base type",
    /// `UsdSchemaRegistry::_PruneBaseClassPropertiesForPrimDefinition`);
    /// a type among its own bases is a fatal error (`TfType::Declare`).
    InvalidBase {
        /// The schema declaring the base.
        schema: TokenId,
        /// The base, as given.
        base: TokenId,
        /// Why it is left out.
        reason: InvalidBaseReason,
    },
    /// `schema`'s bases list two of its ancestors in an order other
    /// bases contradict, so they have no C3 linearization: the schema still
    /// is every ancestor, but [`SchemaRegistry::ancestors`] orders them only
    /// up to the contradiction, then depth first.
    ///
    /// OpenUSD: `TfType::GetAllAncestorTypes` reports the hierarchy as
    /// inconsistent; `TfType::IsA` still finds every ancestor.
    InconsistentAncestry {
        /// The schema.
        schema: TokenId,
        /// Its parent and additional bases, in order.
        bases: Vec<TokenId>,
    },
}

/// Why a base is left out of a schema's ancestry
/// ([`SchemaIssue::InvalidBase`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InvalidBaseReason {
    /// No registered schema has the name.
    Unregistered,
    /// The base is an applied schema, not a typed one.
    NotTyped,
    /// The base is, or derives from, the schema itself.
    Cycle,
}

/// Collects schema definitions and auto-applies, then builds a
/// [`SchemaRegistry`].
///
/// ```
/// use layerstack::{PropertyDefinition, SchemaDefinition, SchemaRegistry, TokenInterner};
///
/// let tokens = TokenInterner::default();
/// let (tile, label, text) = (tokens.intern("Tile"), tokens.intern("LabelAPI"), tokens.intern("text"));
/// let mut builder = SchemaRegistry::builder();
/// builder
///     .register(SchemaDefinition::typed(tile).with_built_in(label))
///     .register(SchemaDefinition::api(label).with_property(
///         PropertyDefinition::attribute(text).with_fallback("untitled"),
///     ));
/// let registry = builder.build(&tokens);
///
/// let definition = registry.prim_definition(Some(tile), &[], &tokens);
/// assert!(definition.is_a(tile));
/// assert!(definition.has_api(label));
/// assert!(definition.property(text).is_some());
/// ```
#[derive(Clone, Debug, Default)]
pub struct SchemaRegistryBuilder {
    schemas: Vec<SchemaDefinition>,
    auto_applies: Vec<(TokenId, TokenId)>,
    metadata: HashMap<TokenId, MetadataDefinition>,
}

impl SchemaRegistryBuilder {
    /// Registers a plugin's metadata field, independently of prim schemas;
    /// `tokens` is the interner of `definition`'s name.
    ///
    /// Identical declarations are accepted, target order immaterial. A
    /// declaration that conflicts with one registered already is refused
    /// and the original kept, as OpenUSD keeps the first registration of a
    /// field: one of a field Sdf registers itself (`documentation`), one
    /// of a field one of usd-core's own plugins registers unless it is
    /// declared as that plugin declares it (the tables of OpenUSD's schema
    /// domains declare them again), and one of a field this builder holds
    /// a different declaration of.
    ///
    /// # Errors
    ///
    /// [`MetadataConflict`], naming who registers the field already.
    ///
    /// OpenUSD: `"…" is already a registered field`
    /// (`SdfSchemaBase::_UpdateMetadataFromPlugins`).
    pub fn register_metadata(
        &mut self,
        mut definition: MetadataDefinition,
        tokens: &TokenInterner,
    ) -> Result<&mut Self, MetadataConflict> {
        definition.targets.sort_unstable();
        definition.targets.dedup();
        let name = definition.name;
        metadata::check_builtin(&definition, tokens.resolve(name)).map_err(|registered_by| {
            MetadataConflict {
                name,
                registered_by,
            }
        })?;
        match self.metadata.entry(name) {
            hashbrown::hash_map::Entry::Occupied(entry) if entry.get() != &definition => {
                Err(MetadataConflict {
                    name,
                    registered_by: MetadataOrigin::Registry,
                })
            }
            hashbrown::hash_map::Entry::Occupied(_) => Ok(self),
            hashbrown::hash_map::Entry::Vacant(entry) => {
                entry.insert(definition);
                Ok(self)
            }
        }
    }

    /// Registers a schema. A later schema of the same name replaces an
    /// earlier one.
    pub fn register(&mut self, schema: SchemaDefinition) -> &mut Self {
        self.schemas.push(schema);
        self
    }

    /// Auto-applies the applied schema `api` (a single-apply schema, or a
    /// multiple-apply schema with an instance name) to `to`: a typed schema,
    /// whose derived schemas it then applies to as well, or a single-apply
    /// schema.
    ///
    /// Auto-applied schemas follow the target's built-ins, the later ones
    /// in dictionary order stronger, as in OpenUSD, which reads them from
    /// `plugInfo.json` (`apiSchemaAutoApplyTo`, `AutoApplyAPISchemas`).
    ///
    /// Spec: AOUSD Core §13.3.2.1 (auto-applies).
    pub fn auto_apply(&mut self, api: TokenId, to: TokenId) -> &mut Self {
        self.auto_applies.push((api, to));
        self
    }

    /// Builds the registry: each schema's prim definition, with its
    /// inclusions and override properties composed in (AOUSD Core
    /// §13.3.2.3). Names are interned in `tokens`, the interner of the
    /// store whose stages will use the registry.
    #[must_use]
    pub fn build(self, tokens: &TokenInterner) -> SchemaRegistry {
        let mut schemas = HashMap::new();
        for schema in self.schemas {
            schemas.insert(schema.name, schema);
        }
        let mut build = Build {
            placeholder: tokens.intern(INSTANCE_NAME_PLACEHOLDER),
            chains: HashMap::new(),
            auto_applied: HashMap::new(),
            complete: HashMap::new(),
            issues: Vec::new(),
            schemas: &schemas,
            tokens,
        };
        let mut names: Vec<TokenId> = schemas.keys().copied().collect();
        names.sort_unstable();
        for &name in &names {
            build.ancestry(name, &mut Vec::new());
        }
        build.collect_auto_applies(&self.auto_applies);

        let mut definitions = HashMap::new();
        for &name in &names {
            let definition = if schemas[&name].kind.is_typed() {
                build.typed_definition(name)
            } else {
                build.api_definition(name, &mut Vec::new()).0
            };
            definitions.insert(name, Arc::new(definition));
        }
        let issues = build.issues;
        SchemaRegistry {
            metadata: self.metadata,
            schemas,
            definitions,
            issues,
            tokens: Some(tokens.mark()),
        }
    }
}

/// Schema definitions and the prim definitions they build.
///
/// Built once by a [`SchemaRegistryBuilder`], then immutable: each schema's
/// own prim definition is computed at build time and shared by every
/// composed prim definition built from it.
///
/// Spec: AOUSD Core §13 (schemas), §13.3.2.3 (the prim definition).
#[derive(Clone, Debug, Default)]
pub struct SchemaRegistry {
    metadata: HashMap<TokenId, MetadataDefinition>,
    schemas: HashMap<TokenId, SchemaDefinition>,
    definitions: HashMap<TokenId, Arc<PrimDefinition>>,
    issues: Vec<SchemaIssue>,
    /// The ids of the interner the registry was built on, `None` for the
    /// empty default registry, which holds none.
    tokens: Option<crate::interner::TokenMark>,
}

impl SchemaRegistry {
    /// The plugin metadata declaration named `name`. Built-in fields are
    /// not declarations of the registry's; [`Self::metadata_field`] finds
    /// those too.
    #[must_use]
    pub fn metadata(&self, name: TokenId) -> Option<&MetadataDefinition> {
        self.metadata.get(&name)
    }

    /// The metadata field `name`, built in or registered here, with its
    /// name interned in `tokens` ([`MetadataField::lookup`]).
    #[must_use]
    pub fn metadata_field(&self, name: &str, tokens: &TokenInterner) -> Option<MetadataField<'_>> {
        MetadataField::lookup(name, Some(self), tokens)
    }

    /// Every registered metadata declaration, in no particular order.
    pub fn metadata_definitions(&self) -> impl Iterator<Item = &MetadataDefinition> {
        self.metadata.values()
    }

    /// Whether this registry's names are ids of `tokens`: it was built on
    /// `tokens` ([`SchemaRegistryBuilder::build`]), or `tokens` is a
    /// snapshot of that interner taken after the build, as a published
    /// [`InMemoryStore::snapshot`](crate::InMemoryStore::snapshot) is. The
    /// empty default registry belongs to every interner.
    ///
    /// A registry built on another interner names other strings with the
    /// same ids, so a stage composed with it would match no prim to its
    /// schemas. Composition does not use such a registry, and reports
    /// [`CompositionError::ForeignSchemaRegistry`](crate::CompositionError::ForeignSchemaRegistry).
    ///
    /// ```
    /// use layerstack::{InMemoryStore, SchemaDefinition, SchemaRegistry};
    ///
    /// let store = InMemoryStore::default();
    /// let mut builder = SchemaRegistry::builder();
    /// builder.register(SchemaDefinition::typed(store.tokens.intern("Rock")));
    /// let registry = builder.build(&store.tokens);
    /// assert!(registry.belongs_to(&store.tokens));
    /// assert!(!registry.belongs_to(&InMemoryStore::default().tokens));
    /// ```
    #[must_use]
    pub fn belongs_to(&self, tokens: &TokenInterner) -> bool {
        self.tokens
            .as_ref()
            .is_none_or(|mark| tokens.names_alike(mark))
    }

    /// A builder to register schemas with.
    #[must_use]
    pub fn builder() -> SchemaRegistryBuilder {
        SchemaRegistryBuilder::default()
    }

    /// The registered schema named `name`.
    #[must_use]
    pub fn schema(&self, name: TokenId) -> Option<&SchemaDefinition> {
        self.schemas.get(&name)
    }

    /// Every registered schema, in no particular order.
    pub fn schemas(&self) -> impl Iterator<Item = &SchemaDefinition> {
        self.schemas.values()
    }

    /// The prim definition of the schema `name` itself: its properties
    /// with its inclusions and overrides composed in. A multiple-apply
    /// schema's definition is a template, its names holding
    /// [`INSTANCE_NAME_PLACEHOLDER`].
    ///
    /// Spec: AOUSD Core §13.3.2.3 (`build_prim_definition`).
    #[must_use]
    pub fn schema_definition(&self, name: TokenId) -> Option<&PrimDefinition> {
        self.definitions.get(&name).map(Arc::as_ref)
    }

    /// What building the registry skipped, in the order found.
    #[must_use]
    pub fn issues(&self) -> &[SchemaIssue] {
        &self.issues
    }

    /// The typed schemas a prim's type may be, or derive from, and the
    /// applied schemas the prim may have, for the applied schema `schema`
    /// to be applied to it with `instance`
    /// ([`SchemaDefinition::can_only_apply_to`]): the instance's own list
    /// when the schema gives a non-empty one, otherwise the schema's. An
    /// empty instance list leaves the schema's in force, as OpenUSD reads
    /// `apiSchemaInstances`. Empty when it may be applied to any prim, or
    /// `schema` is not registered.
    ///
    /// OpenUSD: `UsdSchemaRegistry::GetAPISchemaCanOnlyApplyToTypeNames`.
    #[must_use]
    pub fn can_only_apply_to(
        &self,
        schema: TokenId,
        instance: Option<&str>,
        tokens: &TokenInterner,
    ) -> &[TokenId] {
        let Some(definition) = self.schemas.get(&schema) else {
            return &[];
        };
        if let Some(instance) = instance.and_then(|instance| tokens.lookup(instance))
            && let Some((_, types)) = definition
                .instance_can_only_apply_to
                .iter()
                .find(|(name, types)| *name == instance && !types.is_empty())
        {
            return types;
        }
        &definition.can_only_apply_to
    }

    /// Whether the multiple-apply schema `schema` may be applied with the
    /// instance name `instance`: it is a valid property name (identifiers
    /// joined by `:`), one of the schema's
    /// [`SchemaDefinition::allowed_instance_names`] when it lists any, and
    /// its last identifier is not the base name of a property of the
    /// schema's definition (the part of the name after the placeholder), so
    /// no two properties of an instance can share a name.
    ///
    /// Spec: AOUSD Core §13.3.2 (an instance name follows property name
    /// rules). OpenUSD: `UsdSchemaRegistry::IsAllowedAPISchemaInstanceName`.
    #[must_use]
    pub fn is_allowed_instance_name(
        &self,
        schema: TokenId,
        instance: &str,
        tokens: &TokenInterner,
    ) -> bool {
        let Some(definition) = self.schemas.get(&schema) else {
            return false;
        };
        if definition.kind != SchemaKind::MultipleApplyApi || instance.is_empty() {
            return false;
        }
        if !definition.allowed_instance_names.is_empty()
            && !tokens
                .lookup(instance)
                .is_some_and(|name| definition.allowed_instance_names.contains(&name))
        {
            return false;
        }
        let segments: Vec<&str> = instance.split(':').collect();
        // Spec: AOUSD Core §7.3.3, §16.2.8 (identifiers are XID).
        if !segments
            .iter()
            .all(|segment| crate::ident::is_identifier(segment))
        {
            return false;
        }
        let base = segments[segments.len() - 1];
        self.definitions.get(&schema).is_none_or(|template| {
            template
                .properties()
                .iter()
                .all(|property| template_base_name(tokens.resolve(property.name)) != base)
        })
    }

    /// Whether the applied schema `schema`, with `instance` for a
    /// multiple-apply schema, may be applied to a prim whose definition is
    /// `prim` ([`SchemaRegistry::prim_definition`]), and why not.
    ///
    /// The instance name must be allowed
    /// ([`SchemaRegistry::is_allowed_instance_name`]), and when the schema
    /// names what it can only be applied to
    /// ([`SchemaRegistry::can_only_apply_to`]) the prim must match one of
    /// those names: its type is that typed schema or derives from it (a
    /// typeless prim's never does), or it has that applied schema, built-in
    /// and auto-applied ones included. A multiple-apply schema named with an
    /// instance (`SlotAPI:main`) matches only that instance, and named
    /// alone, any instance. A name that is no registered schema matches
    /// nothing.
    ///
    /// These restrict authoring only; composition applies whatever
    /// `apiSchemas` lists (AOUSD Core §13.3.2).
    ///
    /// # Errors
    ///
    /// The [`CannotApply`] reason.
    ///
    /// OpenUSD: `UsdPrim::CanApplyAPI`.
    pub fn can_apply(
        &self,
        prim: &PrimDefinition,
        schema: TokenId,
        instance: Option<&str>,
        tokens: &TokenInterner,
    ) -> Result<(), CannotApply> {
        let definition = self
            .schemas
            .get(&schema)
            .ok_or(CannotApply::NotAnAppliedSchema)?;
        match (definition.kind, instance) {
            (SchemaKind::ConcreteTyped | SchemaKind::AbstractTyped, _) => {
                return Err(CannotApply::NotAnAppliedSchema);
            }
            (SchemaKind::SingleApplyApi, Some(_)) => {
                return Err(CannotApply::UnexpectedInstanceName);
            }
            (SchemaKind::MultipleApplyApi, None) => {
                return Err(CannotApply::MissingInstanceName);
            }
            (SchemaKind::MultipleApplyApi, Some(instance)) => {
                if !self.is_allowed_instance_name(schema, instance, tokens) {
                    return Err(CannotApply::InstanceNameNotAllowed);
                }
            }
            (SchemaKind::SingleApplyApi, None) => {}
        }
        let allowed = self.can_only_apply_to(schema, instance, tokens);
        if allowed.is_empty()
            || allowed
                .iter()
                .any(|&name| self.is_target(prim, name, tokens))
        {
            return Ok(());
        }
        Err(CannotApply::NotATarget {
            allowed: allowed.to_vec(),
        })
    }

    /// Whether `prim` matches the name `name` of a
    /// [`SchemaRegistry::can_only_apply_to`] list: it has the applied schema
    /// `name` names (the instance after its first `:`, if any), or else its
    /// type is the typed schema `name` or derives from it.
    ///
    /// Spec: AOUSD Core §13.3.1 (a prim's type), §13.3.2 (applied schemas
    /// and instance names). OpenUSD 26.11: `_IsPrimValidApplyToTarget`
    /// (`usd/prim.cpp`), which splits the name with
    /// `UsdSchemaRegistry::GetTypeNameAndInstance`.
    fn is_target(&self, prim: &PrimDefinition, name: TokenId, tokens: &TokenInterner) -> bool {
        let (schema_name, instance) = split_instance(tokens.resolve(name));
        let instance = instance.filter(|instance| !instance.is_empty());
        let applied = tokens.lookup(schema_name).filter(|schema| {
            self.schemas
                .get(schema)
                .is_some_and(|s| s.kind.is_applied())
        });
        match (applied, instance) {
            (Some(schema), None) => prim.has_api(schema),
            (Some(schema), Some(instance)) => tokens
                .lookup(instance)
                .is_some_and(|instance| prim.has_api_instance(schema, instance)),
            (None, _) => prim.is_a(name),
        }
    }

    /// Whether the typed schema `type_name` is `schema` or inherits from it,
    /// abstract ancestors and additional bases included.
    ///
    /// Spec: AOUSD Core §13.3.1. OpenUSD: `TfType::IsA` of the schema types.
    #[must_use]
    pub fn is_a(&self, type_name: TokenId, schema: TokenId) -> bool {
        self.definitions
            .get(&type_name)
            .is_some_and(|definition| definition.is_a(schema))
    }

    /// The typed schema `type_name` and the typed schemas it is, in
    /// OpenUSD's order ([`PrimDefinition::ancestors`]); empty when it is not
    /// a registered typed schema. An abstract schema has ancestors too,
    /// though a prim typed with it is typeless.
    ///
    /// OpenUSD: `TfType::GetAllAncestorTypes` (C3), less its non-schema
    /// types.
    #[must_use]
    pub fn ancestors(&self, type_name: TokenId) -> &[TokenId] {
        self.definitions
            .get(&type_name)
            .map_or(&[], |definition| definition.ancestors())
    }

    /// The prim definition of a composed prim whose resolved `typeName` is
    /// `type_name` and whose composed `apiSchemas` list is `applied`.
    ///
    /// The type's definition comes first; an abstract or unknown type name
    /// makes the prim typeless. Each applied schema's definition then
    /// composes in as a weaker one, in list order, with its instance name
    /// filled in for a multiple-apply schema. A name that is not a
    /// registered applied schema, a multiple-apply schema without an
    /// instance name and a single-apply schema with one are skipped.
    /// Instance names may contain `:`; the schema name ends at the first
    /// one.
    ///
    /// The names this builds for multiple-apply instances (the instance
    /// names, and the instantiated schema and property names) are looked up
    /// in `tokens`, never interned: [`SchemaRegistry::intern_instance_names`]
    /// must have interned them for `applied`, as composing a stage does for
    /// each prim's `apiSchemas`. A name that is missing is a bug, which a
    /// debug assertion reports.
    ///
    /// Each call builds a new definition.
    ///
    /// Spec: AOUSD Core §13.3.1 (typeless prims), §13.3.2 (instance names),
    /// §13.3.2.3 (the final prim definition).
    #[must_use]
    pub fn prim_definition(
        &self,
        type_name: Option<TokenId>,
        applied: &[TokenId],
        tokens: &TokenInterner,
    ) -> PrimDefinition {
        let mut names = Lookup(tokens);
        let mut definition = self.typed(type_name).cloned().unwrap_or_default();
        for &name in applied {
            let Some((schema, instance)) = self.applied(name, tokens) else {
                continue;
            };
            let instance = match instance {
                None => None,
                Some(instance) => match names.name(instance) {
                    Some(instance) => Some(instance),
                    None => continue,
                },
            };
            definition.compose_weaker(schema, instance, &mut names);
        }
        definition
    }

    /// Interns every name [`SchemaRegistry::prim_definition`] builds for
    /// the multiple-apply instances `applied` names: the instance names and
    /// the instantiated names of their schemas and properties, including
    /// those of the multiple-apply schemas they include.
    ///
    /// A stage composed with schemas ([`crate::StageOptions::schemas`])
    /// does this for each prim's composed `apiSchemas`, so its prim
    /// definitions are built without mutating the store.
    ///
    /// Spec: AOUSD Core §13.3.2 (a multiple-apply schema's property names are
    /// formed from the instance name).
    pub fn intern_instance_names(&self, applied: &[TokenId], tokens: &TokenInterner) {
        for &name in applied {
            let Some((schema, Some(instance))) = self
                .applied(name, tokens)
                .map(|(schema, instance)| (schema, instance.map(String::from)))
            else {
                continue;
            };
            let instance = tokens.intern(instance);
            PrimDefinition::default().compose_weaker(schema, Some(instance), &mut Intern(tokens));
        }
    }

    /// The definition of the property `property` in
    /// [`SchemaRegistry::prim_definition`]`(type_name, applied)`, found
    /// without building the whole definition or interning anything.
    ///
    /// Spec: AOUSD Core §13.3.2.4 (the schemas are visited in the order the
    /// definition composes them).
    #[must_use]
    pub fn property_definition(
        &self,
        type_name: Option<TokenId>,
        applied: &[TokenId],
        property: TokenId,
        tokens: &TokenInterner,
    ) -> Option<PropertyDefinition> {
        let mut found: Option<PropertyDefinition> = None;
        let mut visit = |candidate: &PropertyDefinition| match &mut found {
            None => {
                found = Some(PropertyDefinition {
                    name: property,
                    ..candidate.clone()
                });
            }
            Some(stronger) => stronger.compose_weaker(candidate, tokens),
        };
        if let Some(typed) = self.typed(type_name)
            && let Some(candidate) = typed.property(property)
        {
            visit(candidate);
        }
        let name = tokens.resolve(property);
        for &entry in applied {
            let Some((schema, instance)) = self.applied(entry, tokens) else {
                continue;
            };
            match instance {
                None => {
                    if let Some(candidate) = schema.property(property) {
                        visit(candidate);
                    }
                }
                Some(instance) => {
                    for candidate in schema.instance_properties(name, instance, tokens) {
                        visit(candidate);
                    }
                }
            }
        }
        found
    }

    /// Reuse the registry-owned definition for a plain concrete type.
    pub(crate) fn shared_typed(&self, name: Option<TokenId>) -> Option<Arc<PrimDefinition>> {
        let name = name?;
        (self.schemas.get(&name)?.kind == SchemaKind::ConcreteTyped)
            .then(|| self.definitions.get(&name).cloned())
            .flatten()
    }

    /// The definition of the concrete typed schema `type_name`, if it is
    /// one; otherwise the prim is typeless.
    ///
    /// Spec: AOUSD Core §13.3.1.
    fn typed(&self, type_name: Option<TokenId>) -> Option<&PrimDefinition> {
        let type_name = type_name?;
        (self.schemas.get(&type_name)?.kind == SchemaKind::ConcreteTyped)
            .then(|| self.definitions.get(&type_name).map(Arc::as_ref))
            .flatten()
    }

    /// The definition of the applied schema an `apiSchemas` entry names,
    /// with its instance name: `None` when the entry names no registered
    /// applied schema or has an instance name exactly when the schema is
    /// single-apply.
    ///
    /// Spec: AOUSD Core §13.2.1.2 (`apiSchemas`), §13.3.2 (the instance name
    /// follows the first `:`).
    fn applied<'t>(
        &self,
        name: TokenId,
        tokens: &'t TokenInterner,
    ) -> Option<(&PrimDefinition, Option<&'t str>)> {
        let (schema_name, instance) = split_instance(tokens.resolve(name));
        let schema = self.schemas.get(&tokens.lookup(schema_name)?)?;
        let instance = instance.filter(|instance| !instance.is_empty());
        let fits = match schema.kind {
            SchemaKind::SingleApplyApi => instance.is_none(),
            SchemaKind::MultipleApplyApi => instance.is_some(),
            SchemaKind::ConcreteTyped | SchemaKind::AbstractTyped => false,
        };
        fits.then(|| Some((self.definitions.get(&schema.name)?.as_ref(), instance)))
            .flatten()
    }
}

/// The part of a multiple-apply template name after its placeholder: `index`
/// for `slot:__INSTANCE_NAME__:index`, empty for `slot:__INSTANCE_NAME__`,
/// the name itself when it has no placeholder.
///
/// OpenUSD: `UsdSchemaRegistry::GetMultipleApplyNameTemplateBaseName`.
fn template_base_name(name: &str) -> &str {
    match placeholder_offset(name) {
        None => name,
        Some(offset) => name
            .get(offset + INSTANCE_NAME_PLACEHOLDER.len() + 1..)
            .unwrap_or(""),
    }
}

/// Splits an applied schema name at its first `:` into the schema name and
/// the instance name, which may itself contain `:`.
fn split_instance(name: &str) -> (&str, Option<&str>) {
    match name.split_once(':') {
        Some((schema, instance)) => (schema, Some(instance)),
        None => (name, None),
    }
}

/// The byte offset of the first namespace segment of `name` that is
/// [`INSTANCE_NAME_PLACEHOLDER`].
fn placeholder_offset(name: &str) -> Option<usize> {
    let mut start = 0;
    for segment in name.split(':') {
        if segment == INSTANCE_NAME_PLACEHOLDER {
            return Some(start);
        }
        start += segment.len() + 1;
    }
    None
}

/// How a prim definition being built finds the tokens of names it forms:
/// interned while the registry is built ([`Intern`]), looked up once it is
/// in use ([`Lookup`]).
trait Names {
    /// The token of `text`, if it has one.
    fn name(&mut self, text: &str) -> Option<TokenId>;

    /// The interner the names are in.
    fn tokens(&self) -> &TokenInterner;

    /// `name` with its placeholder replaced by `instance`; `name` itself
    /// when there is no instance or no placeholder.
    ///
    /// OpenUSD: `UsdSchemaRegistry::MakeMultipleApplyNameInstance`.
    fn instantiate(&mut self, name: TokenId, instance: Option<TokenId>) -> Option<TokenId> {
        let Some(instance) = instance else {
            return Some(name);
        };
        let tokens = self.tokens();
        let text = tokens.resolve(name);
        let Some(offset) = placeholder_offset(text) else {
            return Some(name);
        };
        let instantiated = format!(
            "{}{}{}",
            &text[..offset],
            tokens.resolve(instance),
            &text[offset + INSTANCE_NAME_PLACEHOLDER.len()..]
        );
        self.name(&instantiated)
    }
}

/// [`Names`] that interns what it forms.
struct Intern<'t>(&'t TokenInterner);

impl Names for Intern<'_> {
    fn name(&mut self, text: &str) -> Option<TokenId> {
        Some(self.0.intern(text))
    }

    fn tokens(&self) -> &TokenInterner {
        self.0
    }
}

/// [`Names`] that looks up what it forms, which
/// [`SchemaRegistry::intern_instance_names`] interned beforehand.
struct Lookup<'t>(&'t TokenInterner);

impl Names for Lookup<'_> {
    fn name(&mut self, text: &str) -> Option<TokenId> {
        let found = self.0.lookup(text);
        debug_assert!(
            found.is_some(),
            "`{text}` was never interned; `SchemaRegistry::intern_instance_names` must run \
             for the applied schemas first"
        );
        found
    }

    fn tokens(&self) -> &TokenInterner {
        self.0
    }
}

/// The state of one [`SchemaRegistryBuilder::build`].
struct Build<'a> {
    schemas: &'a HashMap<TokenId, SchemaDefinition>,
    tokens: &'a TokenInterner,
    placeholder: TokenId,
    /// Each typed schema and its ancestors, in C3 order.
    chains: HashMap<TokenId, Vec<TokenId>>,
    /// The schemas auto-applied to each schema, strongest first.
    auto_applied: HashMap<TokenId, Vec<TokenId>>,
    /// Applied schema definitions built without meeting a cycle, which
    /// every inclusion of them reuses.
    complete: HashMap<TokenId, PrimDefinition>,
    issues: Vec<SchemaIssue>,
}

impl<'a> Build<'a> {
    fn issue(&mut self, issue: SchemaIssue) {
        if !self.issues.contains(&issue) {
            self.issues.push(issue);
        }
    }

    /// `name` and the typed schemas it is, in OpenUSD's order: the C3
    /// linearization of its parent and additional bases (`TfType`'s
    /// `GetAllAncestorTypes`), each base's ancestry merged after it. Bases
    /// that are not registered typed schemas, or are being linearized
    /// (`visiting`, a cycle), are left out with an issue. Memoized in
    /// `chains`.
    fn ancestry(&mut self, name: TokenId, visiting: &mut Vec<TokenId>) -> Vec<TokenId> {
        if let Some(chain) = self.chains.get(&name) {
            return chain.clone();
        }
        let Some(schema) = self.schemas.get(&name) else {
            return vec![name];
        };
        if !schema.kind.is_typed() {
            return vec![name];
        }
        let declared: Vec<TokenId> = schema
            .parent
            .iter()
            .chain(&schema.additional_bases)
            .copied()
            .collect();
        visiting.push(name);
        let mut bases = Vec::new();
        for &base in &declared {
            let reason = match self.schemas.get(&base) {
                None => Some(InvalidBaseReason::Unregistered),
                Some(found) if !found.kind.is_typed() => Some(InvalidBaseReason::NotTyped),
                Some(_) if visiting.contains(&base) => Some(InvalidBaseReason::Cycle),
                Some(_) => None,
            };
            match reason {
                Some(reason) => self.issue(SchemaIssue::InvalidBase {
                    schema: name,
                    base,
                    reason,
                }),
                None if !bases.contains(&base) => bases.push(base),
                None => {}
            }
        }
        let inherited: Vec<Vec<TokenId>> = bases
            .iter()
            .map(|&base| self.ancestry(base, visiting))
            .collect();
        visiting.pop();
        let chain = match inherited.as_slice() {
            [] => vec![name],
            [only] => core::iter::once(name).chain(only.iter().copied()).collect(),
            _ => {
                let mut sequences = vec![vec![name], bases.clone()];
                sequences.extend(inherited.iter().cloned());
                let (mut chain, consistent) = c3_merge(sequences);
                if !consistent {
                    self.issue(SchemaIssue::InconsistentAncestry {
                        schema: name,
                        bases: declared,
                    });
                    // The rest depth first, so the schema is still each.
                    for ancestor in inherited.iter().flatten() {
                        if !chain.contains(ancestor) {
                            chain.push(*ancestor);
                        }
                    }
                }
                chain
            }
        };
        self.chains.insert(name, chain.clone());
        chain
    }

    /// `name` and the parents whose properties, built-ins and overrides
    /// its definition composes: up to the first flattened one, which holds
    /// the rest, and never past a parent left out of its ancestry.
    fn property_chain(&self, name: TokenId) -> Vec<TokenId> {
        let mut chain = vec![name];
        let mut current = &self.schemas[&name];
        while !current.flattened
            && let Some(parent) = current.parent
            && self.chains[&current.name].contains(&parent)
            && !chain.contains(&parent)
        {
            chain.push(parent);
            current = &self.schemas[&parent];
        }
        chain
    }

    /// Distributes the auto-applies to their targets: a typed target and
    /// every schema derived from it through any of its bases
    /// (`TfType::GetAllDerivedTypes`), or an applied target. Each target's
    /// list is in reverse dictionary order, as OpenUSD orders it
    /// (`Usd_SortAutoAppliedAPISchemas`).
    fn collect_auto_applies(&mut self, auto_applies: &[(TokenId, TokenId)]) {
        for &(api, target) in auto_applies {
            let Some(target_schema) = self.schemas.get(&target) else {
                self.issue(SchemaIssue::UnknownAutoApplyTarget {
                    schema: api,
                    target,
                });
                continue;
            };
            let targets: Vec<TokenId> = if target_schema.kind.is_typed() {
                self.chains
                    .iter()
                    .filter(|(_, chain)| chain.contains(&target))
                    .map(|(name, _)| *name)
                    .collect()
            } else {
                vec![target]
            };
            for target in targets {
                let list = self.auto_applied.entry(target).or_default();
                if !list.contains(&api) {
                    list.push(api);
                }
            }
        }
        let tokens = self.tokens;
        // Descending dictionary order: ascending, reversed. Two names
        // compare equal only when they are the same token, so this is
        // the descending order exactly.
        for list in self.auto_applied.values_mut() {
            tokens.sort_in_dictionary_order(list);
            list.reverse();
        }
    }

    /// Resolves the inclusion `included` of `schema` to the included
    /// schema and the instance name it is applied with, in template form
    /// within a multiple-apply schema.
    ///
    /// Spec: AOUSD Core §13.3.2.1.
    fn inclusion(
        &mut self,
        schema: &SchemaDefinition,
        included: TokenId,
    ) -> Option<(TokenId, Option<TokenId>)> {
        let text = String::from(self.tokens.resolve(included));
        let (name, instance) = split_instance(&text);
        let instance = instance.filter(|instance| !instance.is_empty());
        let Some(target) = self
            .tokens
            .lookup(name)
            .and_then(|name| self.schemas.get(&name))
        else {
            self.issue(SchemaIssue::UnknownInclusion {
                schema: schema.name,
                included,
            });
            return None;
        };
        let template = schema.kind == SchemaKind::MultipleApplyApi;
        let resolved = match (template, target.kind, instance) {
            (false, SchemaKind::SingleApplyApi, None) => Some(None),
            (false, SchemaKind::MultipleApplyApi, Some(instance)) => {
                Some(Some(self.tokens.intern(instance)))
            }
            (true, SchemaKind::MultipleApplyApi, None) => Some(Some(self.placeholder)),
            (true, SchemaKind::MultipleApplyApi, Some(sub)) => Some(Some(
                self.tokens
                    .intern(format!("{INSTANCE_NAME_PLACEHOLDER}:{sub}")),
            )),
            _ => None,
        };
        let Some(instance) = resolved else {
            self.issue(SchemaIssue::InvalidInclusion {
                schema: schema.name,
                included,
            });
            return None;
        };
        Some((target.name, instance))
    }

    /// The prim definition of the applied schema `name`, built from the top:
    /// its properties, then each built-in and auto-applied schema's
    /// definition composed in as a weaker one, then its override
    /// properties. `building` holds the schemas whose definitions are being
    /// built around this one; including one of them again is a cycle, and
    /// skipped. Returns whether no cycle was met, in which case the
    /// definition is kept for reuse.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (`build_prim_definition`). OpenUSD:
    /// `_APISchemaPrimDefBuilder::BuildPrimDefinition`, which also rebuilds
    /// a definition that met a cycle from the top when it is asked for.
    fn api_definition(
        &mut self,
        name: TokenId,
        building: &mut Vec<TokenId>,
    ) -> (PrimDefinition, bool) {
        if let Some(definition) = self.complete.get(&name) {
            return (definition.clone(), true);
        }
        let schemas = self.schemas;
        let schema = &schemas[&name];
        let template = schema.kind == SchemaKind::MultipleApplyApi;
        let applied_name = if template {
            let text = format!(
                "{}:{INSTANCE_NAME_PLACEHOLDER}",
                self.tokens.resolve(schema.name)
            );
            self.tokens.intern(text)
        } else {
            name
        };
        let mut definition = PrimDefinition::default();
        definition.push_applied(AppliedSchema {
            name: applied_name,
            schema: name,
            instance: template.then_some(self.placeholder),
        });
        for property in &schema.properties {
            definition.add_property(property.clone(), self.tokens);
        }

        building.push(name);
        let mut inclusions = schema.built_ins.clone();
        if let Some(auto) = self.auto_applied.get(&name) {
            inclusions.extend(auto.iter().copied());
        }
        let mut complete = true;
        for included in inclusions {
            let Some((target, instance)) = self.inclusion(schema, included) else {
                continue;
            };
            if building.contains(&target) {
                self.issue(SchemaIssue::InclusionCycle {
                    schema: name,
                    included: target,
                });
                complete = false;
                continue;
            }
            let (weaker, weaker_complete) = self.api_definition(target, building);
            complete &= weaker_complete;
            definition.compose_weaker(&weaker, instance, &mut Intern(self.tokens));
        }
        building.pop();

        for property in &schema.overrides {
            self.apply_override(&mut definition, name, property);
        }
        if complete {
            self.complete.insert(name, definition.clone());
        }
        (definition, complete)
    }

    /// The prim definition of the typed schema `name`: the properties of
    /// it and its ancestors, nearest first, then the built-ins of each,
    /// then the schemas auto-applied to any of them, then the override
    /// properties of each.
    ///
    /// Spec: AOUSD Core §13.3.1 (inheritance), §13.3.2.3.
    fn typed_definition(&mut self, name: TokenId) -> PrimDefinition {
        let schemas = self.schemas;
        let mut definition = PrimDefinition::default();
        definition.type_name = Some(name);
        definition.ancestors.clone_from(&self.chains[&name]);
        let chain = &self.property_chain(name);
        let mut inclusions = Vec::new();
        for ancestor in chain {
            let ancestor = &schemas[ancestor];
            for property in &ancestor.properties {
                definition.add_property(property.clone(), self.tokens);
            }
            for &included in &ancestor.built_ins {
                if !inclusions.contains(&included) {
                    inclusions.push(included);
                }
            }
        }
        if let Some(auto) = self.auto_applied.get(&name) {
            for &included in auto {
                if !inclusions.contains(&included) {
                    inclusions.push(included);
                }
            }
        }
        let schema = &schemas[&name];
        for included in inclusions {
            if let Some((target, instance)) = self.inclusion(schema, included) {
                let (weaker, _) = self.api_definition(target, &mut Vec::new());
                definition.compose_weaker(&weaker, instance, &mut Intern(self.tokens));
            }
        }
        let mut overridden = HashSet::new();
        for ancestor in chain {
            for property in &schemas[ancestor].overrides {
                if overridden.insert(property.name) {
                    self.apply_override(&mut definition, name, property);
                }
            }
        }
        definition
    }

    /// Composes the override property `over` of `schema` over the
    /// property it names: its fallback and metadata replace defined fields,
    /// including whole dictionaries; its variability is ignored.
    ///
    /// Spec: AOUSD Core §13.3.2.2. OpenUSD:
    /// `UsdPrimDefinition::_ComposeOverAndReplaceExistingProperty`.
    fn apply_override(
        &mut self,
        definition: &mut PrimDefinition,
        schema: TokenId,
        over: &PropertyDefinition,
    ) {
        match definition.property_mut(over.name) {
            Some(defined) if defined.has_type_of(over) => {
                if over.fallback.is_some() {
                    defined.fallback.clone_from(&over.fallback);
                }
                // Core §13.3.2.2: overrides replace whole metadata fields;
                // unlike weaker API composition, dictionaries do not merge.
                for field in &over.metadata {
                    crate::doc::set_field_vec(
                        &mut defined.metadata,
                        field.name,
                        field.value.clone(),
                    );
                }
            }
            _ => self.issue(SchemaIssue::IgnoredOverride {
                schema,
                property: over.name,
            }),
        }
    }
}

/// Merges `sequences` as C3 linearization does: repeatedly takes the first
/// head of a sequence that is in no sequence's tail. Returns what was
/// merged and whether every sequence was consumed; when not, the
/// hierarchy is inconsistent and the merge stopped there.
///
/// OpenUSD: `_MergeAncestors` (`pxr/base/tf/type.cpp`).
fn c3_merge(mut sequences: Vec<Vec<TokenId>>) -> (Vec<TokenId>, bool) {
    let mut merged = Vec::new();
    loop {
        sequences.retain(|sequence| !sequence.is_empty());
        if sequences.is_empty() {
            return (merged, true);
        }
        let Some(candidate) = sequences
            .iter()
            .map(|sequence| sequence[0])
            .find(|candidate| {
                sequences
                    .iter()
                    .all(|sequence| !sequence[1..].contains(candidate))
            })
        else {
            return (merged, false);
        };
        merged.push(candidate);
        for sequence in &mut sequences {
            if sequence[0] == candidate {
                sequence.remove(0);
            }
        }
    }
}
#[cfg(test)]
mod tests;
