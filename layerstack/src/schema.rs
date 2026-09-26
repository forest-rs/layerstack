// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schemas and the prim definitions they build.
//!
//! A schema ascribes properties, each with a declared type, variability and
//! optional fallback, to composed prims. A *typed* schema is the prim's
//! type (its `typeName`), with single inheritance; an *applied* schema is
//! named in the prim's `apiSchemas` list op, once for a single-apply schema
//! and once per instance name for a multiple-apply one.
//!
//! Register [`SchemaDefinition`]s with a [`SchemaRegistryBuilder`] and build
//! a [`SchemaRegistry`]. Building computes each schema's own
//! [`PrimDefinition`] once, with its built-ins, auto-applies and override
//! properties composed in; [`SchemaRegistry::prim_definition`] then composes
//! a composed prim's definition from its type and applied schemas. A stage
//! composed with a registry ([`crate::StageOptions::schemas`]) answers the
//! same queries per prim ([`crate::Stage::prim_definition`]) and resolves
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

pub use definition::{AppliedSchema, PrimDefinition};
pub use generated::{GeneratedSchemaError, SchemaDeclaration, read_generated_schema};

use alloc::{format, string::String, vec, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    doc::Value,
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
/// fallback value.
///
/// ```
/// use layerstack::{PropertyDefinition, PropertyType, TokenInterner, Value};
///
/// let mut tokens = TokenInterner::default();
/// let width = PropertyDefinition::attribute(tokens.intern("width"))
///     .with_type(PropertyType::new("float", false, Value::Float(0.0)))
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
        let declared = |p: &Self| {
            p.type_name
                .as_ref()
                .map(|t| (t.type_name.clone(), t.is_array))
        };
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
/// let mut tokens = TokenInterner::default();
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
    /// and built-ins it has too, weaker than its own.
    ///
    /// Spec: AOUSD Core §13.3.1 (single inheritance).
    pub parent: Option<TokenId>,
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
}

impl SchemaDefinition {
    /// A schema of `kind` with nothing defined yet.
    #[must_use]
    pub fn new(name: TokenId, kind: SchemaKind) -> Self {
        Self {
            name,
            kind,
            parent: None,
            built_ins: Vec::new(),
            properties: Vec::new(),
            overrides: Vec::new(),
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
}

/// Something a [`SchemaRegistryBuilder::build`] skipped, and why. Building
/// never fails: what an issue names is left out, as OpenUSD leaves it out
/// with a warning.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// `schema` inherits from `parent`, which is not a registered typed
    /// schema or inherits from `schema` itself; the inheritance chain stops
    /// there.
    InvalidParent {
        /// The inheriting schema.
        schema: TokenId,
        /// The parent, as given.
        parent: TokenId,
    },
}

/// Collects schema definitions and auto-applies, then builds a
/// [`SchemaRegistry`].
///
/// ```
/// use layerstack::{PropertyDefinition, SchemaDefinition, SchemaRegistry, TokenInterner};
///
/// let mut tokens = TokenInterner::default();
/// let (tile, label, text) = (tokens.intern("Tile"), tokens.intern("LabelAPI"), tokens.intern("text"));
/// let mut builder = SchemaRegistry::builder();
/// builder
///     .register(SchemaDefinition::typed(tile).with_built_in(label))
///     .register(SchemaDefinition::api(label).with_property(
///         PropertyDefinition::attribute(text).with_fallback("untitled"),
///     ));
/// let registry = builder.build(&mut tokens);
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
}

impl SchemaRegistryBuilder {
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
    pub fn build(self, tokens: &mut TokenInterner) -> SchemaRegistry {
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
            let chain = build.type_chain(name);
            build.chains.insert(name, chain);
        }
        build.collect_auto_applies(&self.auto_applies);

        let mut definitions = HashMap::new();
        for &name in &names {
            let definition = if schemas[&name].kind.is_typed() {
                build.typed_definition(name)
            } else {
                build.api_definition(name, &mut Vec::new()).0
            };
            definitions.insert(name, definition);
        }
        let issues = build.issues;
        SchemaRegistry {
            schemas,
            definitions,
            issues,
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
    schemas: HashMap<TokenId, SchemaDefinition>,
    definitions: HashMap<TokenId, PrimDefinition>,
    issues: Vec<SchemaIssue>,
}

impl SchemaRegistry {
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
        self.definitions.get(&name)
    }

    /// What building the registry skipped, in the order found.
    #[must_use]
    pub fn issues(&self) -> &[SchemaIssue] {
        &self.issues
    }

    /// Whether the typed schema `type_name` is `schema` or inherits from it,
    /// abstract ancestors included.
    ///
    /// Spec: AOUSD Core §13.3.1.
    #[must_use]
    pub fn is_a(&self, type_name: TokenId, schema: TokenId) -> bool {
        self.definitions
            .get(&type_name)
            .is_some_and(|definition| definition.is_a(schema))
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
    pub fn intern_instance_names(&self, applied: &[TokenId], tokens: &mut TokenInterner) {
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
            Some(stronger) => stronger.compose_weaker(candidate),
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

    /// The definition of the concrete typed schema `type_name`, if it is
    /// one; otherwise the prim is typeless.
    ///
    /// Spec: AOUSD Core §13.3.1.
    fn typed(&self, type_name: Option<TokenId>) -> Option<&PrimDefinition> {
        let type_name = type_name?;
        (self.schemas.get(&type_name)?.kind == SchemaKind::ConcreteTyped)
            .then(|| self.definitions.get(&type_name))
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
        fits.then(|| Some((self.definitions.get(&schema.name)?, instance)))
            .flatten()
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
struct Intern<'t>(&'t mut TokenInterner);

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
    tokens: &'a mut TokenInterner,
    placeholder: TokenId,
    /// Each typed schema and its ancestors, nearest first.
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

    /// `name` and the typed schemas it inherits from, nearest first.
    fn type_chain(&mut self, name: TokenId) -> Vec<TokenId> {
        let mut chain = vec![name];
        let Some(schema) = self.schemas.get(&name) else {
            return chain;
        };
        if !schema.kind.is_typed() {
            return chain;
        }
        let mut current = schema;
        while let Some(parent) = current.parent {
            match self.schemas.get(&parent) {
                Some(next) if next.kind.is_typed() && !chain.contains(&parent) => {
                    chain.push(parent);
                    current = next;
                }
                _ => {
                    self.issue(SchemaIssue::InvalidParent {
                        schema: current.name,
                        parent,
                    });
                    break;
                }
            }
        }
        chain
    }

    /// Distributes the auto-applies to their targets: a typed target and
    /// every schema derived from it, or an applied target. Each target's
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
        let tokens = &*self.tokens;
        for list in self.auto_applied.values_mut() {
            list.sort_by(|a, b| {
                crate::stage::dictionary_cmp(tokens.resolve(*b), tokens.resolve(*a))
            });
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
            definition.add_property(property.clone());
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
            definition.compose_weaker(&weaker, instance, &mut Intern(&mut *self.tokens));
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
        let chain = self.chains[&name].clone();
        let mut definition = PrimDefinition::default();
        definition.type_name = Some(name);
        definition.type_chain.clone_from(&chain);
        let mut inclusions = Vec::new();
        for ancestor in &chain {
            let ancestor = &schemas[ancestor];
            for property in &ancestor.properties {
                definition.add_property(property.clone());
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
                definition.compose_weaker(&weaker, instance, &mut Intern(&mut *self.tokens));
            }
        }
        let mut overridden = HashSet::new();
        for ancestor in &chain {
            for property in &schemas[ancestor].overrides {
                if overridden.insert(property.name) {
                    self.apply_override(&mut definition, name, property);
                }
            }
        }
        definition
    }

    /// Composes the override property `over` of `schema` over the
    /// property it names: its fallback replaces the defined one, its
    /// variability is ignored.
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
            }
            _ => self.issue(SchemaIssue::IgnoredOverride {
                schema,
                property: over.name,
            }),
        }
    }
}

#[cfg(test)]
mod tests;
