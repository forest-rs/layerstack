// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A schema compiler: `schema.usda`, in usdGenSchema's authoring form, to
//! the `generatedSchema.usda` and `plugInfo.json` OpenUSD's `usdGenSchema`
//! writes for it, so the schemas load in OpenUSD tools and in layerstack
//! ([`layerstack::schema::read_generated_schema`]) alike. No C++ or Python
//! is generated.
//!
//! ```no_run
//! use layerstack_schemagen::compile::{CompileOptions, compile};
//!
//! let options = CompileOptions::new().with_openusd_source("../OpenUSD");
//! let compiled = compile("schema/schema.usda".as_ref(), &options)?;
//! std::fs::write("generatedSchema.usda", compiled.generated_schema_usda())?;
//! if let Some(plug_info) = compiled.plug_info_json() {
//!     std::fs::write("plugInfo.json", plug_info)?;
//! }
//! for class in compiled.classes() {
//!     println!("{} ({})", class.name, class.kind.as_str());
//! }
//! // Typed views and registration, for a crate using the schemas.
//! std::fs::write("schemas.rs", compiled.rust_module()?)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What it does
//!
//! The schema file is composed with the layers it sublayers, as
//! usdGenSchema composes it (`ParseUsd`). Each class prim of the file
//! (`class`, at the root) is a schema, checked against every rule
//! usdGenSchema checks (`ClassInfo`, `_GenerateClassInfo`,
//! `_ValidateFields`, and for a library that is not codeless the token and
//! reflected-schema checks of its code generation); the first broken rule
//! is the [`CompileError`]. Then:
//!
//! - `generatedSchema.usda` is the composed stage flattened
//!   ([`layerstack::Stage::flatten`]) and edited with a
//!   [`layerstack::Transaction`] as `GenerateRegistry` edits it: each
//!   class keeps the properties its inherited classes compose into it, and
//!   only its own documentation, which becomes the first sentence (or the
//!   authored `userDocBrief`, or the one in `userDoc/schemaUserDoc.usda`
//!   next to the schema file) in `customData.userDocBrief`;
//!   `documentation` and the rest of `customData` are removed, but for
//!   `apiSchemaOverridePropertyNames`; a multiple-apply schema's
//!   properties get its `propertyNamespacePrefix` and `__INSTANCE_NAME__`;
//!   `apiSchemas` lists every applied schema the class composes; the
//!   classes of other libraries and `/GLOBAL` are removed; and
//!   `fallbackTypes` become the layer's `fallbackPrimTypes`. It is written
//!   as `SdfLayer::ExportToString` writes it, so for OpenUSD's own schemas
//!   it is the file OpenUSD checks in, byte for byte.
//! - `plugInfo.json` declares each class's C++ type name, base, kind,
//!   identifier and application rules (`GeneratePlugInfo`). It is built on
//!   an existing `plugInfo.json` when one is given
//!   ([`CompileOptions::with_existing_plug_info`]), keeping every edit
//!   but the generated types, as usdGenSchema builds on the file in its
//!   output directory; otherwise on OpenUSD's template, whose
//!   `@PLUG_INFO_LIBRARY_PATH@`, `@PLUG_INFO_RESOURCE_PATH@` and
//!   `@PLUG_INFO_ROOT@` OpenUSD's build fills in. Like usdGenSchema's,
//!   that file starts with `#` comment lines, which OpenUSD's plugin
//!   reader accepts and strict JSON readers do not, and it does not
//!   register as written: its placeholders are not paths.
//!   [`CompiledSchema::installed_plug_info_json`] is the form to register,
//!   for a plugin directory with both files in `resources/` (`compile
//!   --install <dir>`): placeholders filled as OpenUSD's build fills them,
//!   no comments. Edits to an existing file survive either way.
//!
//! # Rust views
//!
//! [`CompiledSchema::rust_module`] is the library as a Rust module, for a
//! crate that reads and authors its schemas with layerstack. For each
//! schema with a prim definition it has a typed view, an edit handle and
//! token enums of the shape `layerstack_schemas` gives OpenUSD's own
//! schemas (its crate docs, "Views"), built on that crate's public API
//! alone, and it has the static tables that register the schemas:
//!
//! - a typed schema's view (`Spectrometer`) is had with `new`, which
//!   checks `IsA`, and derefs to the view of the schema it inherits from,
//!   OpenUSD's included: a `Telescope` inheriting `Xformable` derefs to
//!   `layerstack_schemas::usd_geom::Xformable`, with its getters and
//!   transform computations, and `TelescopeEdit` to `XformableEdit`, with its
//!   op authoring. A concrete schema's view has `define`, and a typed
//!   view an accessor for each single-apply built-in it adds
//!   (`calibration_api`);
//! - a single-apply schema's view is had with `get`, which checks
//!   `HasAPI`, and applied with `apply`; a multiple-apply schema's with
//!   `get(scene, path, instance)` or `instances`, and applied with
//!   `apply(edit, path, instance)`;
//! - getters return the resolved value, the registry's fallback included
//!   (`max_rpm`; `max_rpm_at` for a time code), relationship getters their
//!   targets, and a token attribute with `allowedTokens` reads as an enum
//!   with `Other` for any other token (`ChannelApiMode`); the edit handle's
//!   setters (`set_max_rpm`, `set_max_rpm_at`) author through a
//!   `SchemaEdit`;
//! - accessors are named for each property's `apiName`, snake-cased, which
//!   `schema.usda` keeps and `generatedSchema.usda` does not, else for its
//!   USD name without a multiple-apply schema's prefix: `centrifuge:maxSpeed`
//!   with `apiName = "maxRpm"` makes `max_rpm`, `telescope:aperture` makes
//!   `telescope_aperture`. A name that is a Rust keyword or collides with another
//!   accessor of the view or of a view it derefs to, a multiple-apply
//!   property named for the instance alone (`__INSTANCE_NAME__`) without an
//!   `apiName`, and two schemas that make one Rust type are a
//!   [`CompileError::Rust`]: nothing is renamed silently;
//! - `register(builder, tokens)` adds the schemas, their auto-applies and
//!   the plugin's `SdfMetadata` declarations (from an existing
//!   `plugInfo.json`) to a `SchemaRegistryBuilder` from static
//!   `layerstack_schemas::table` tables, as `layerstack_schemas::register`
//!   adds OpenUSD's; nothing is parsed at run time. `DEPENDENCIES` lists
//!   the OpenUSD domains the schemas build on, and `registry(tokens)`
//!   builds a registry of both. `LIBRARY` is the library's name.
//!
//! A library's schemas can build on each other and on OpenUSD's domains,
//! which `layerstack_schemas` has views of; building on another library
//! is a [`CompileError::Rust`]. The OpenUSD schemas come from the schema
//! stage, so compile against the OpenUSD release `layerstack_schemas` is
//! generated from (`layerstack_schemas::OPENUSD_VERSION`).
//!
//! The module needs the crates `layerstack` and `layerstack_schemas`, with
//! the features of the domains it builds on (its first comment names
//! them); it works in `no_std` with `alloc`. It declares `extern crate
//! alloc` and its imports itself, so it is a module of its own. From a
//! build script (`layerstack_schemagen` is not published; depend on the
//! repository):
//!
//! ```toml
//! [dependencies]
//! layerstack = "0.1"
//! layerstack_schemas = { version = "0.1", default-features = false, features = ["usd-geom"] }
//!
//! [build-dependencies]
//! layerstack_schemagen = { git = "https://github.com/forest-rs/layerstack" }
//! ```
//!
//! ```no_run
//! // build.rs
//! use std::path::{Path, PathBuf};
//!
//! use layerstack_schemagen::compile::{CompileOptions, compile};
//!
//! fn main() {
//!     // OpenUSD's schema sources, for `@usdGeom/schema.usda@`: a checkout
//!     // (or `with_plugin_root` and the usd-core wheel's `pxr/pluginfo`).
//!     let openusd = std::env::var_os("OPENUSD_SOURCE").expect("an OpenUSD checkout");
//!     let options = CompileOptions::new().with_openusd_source(openusd);
//!     let compiled = compile(Path::new("schema/schema.usda"), &options)
//!         .unwrap_or_else(|e| panic!("{e}"));
//!     let module = compiled.rust_module().unwrap_or_else(|e| panic!("{e}"));
//!     let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("set by Cargo"));
//!     std::fs::write(out.join("schemas.rs"), module).expect("OUT_DIR is writable");
//!     println!("cargo:rerun-if-changed=schema");
//! }
//! ```
//!
//! ```ignore
//! // src/lib.rs
//! pub mod schemas {
//!     include!(concat!(env!("OUT_DIR"), "/schemas.rs"));
//! }
//! ```
//!
//! Or check the module in: `compile schema/schema.usda --openusd-source
//! <OpenUSD> --rust src/schemas.rs` writes it formatted (`mod schemas;`
//! reads it), and `--check` in CI fails when it is stale. Then, with a
//! store and a stage composed with the registry:
//!
//! ```ignore
//! let registry = schemas::registry(&mut store.tokens)?;
//! let options = StageOptions { schemas: Some(Arc::new(registry)), ..StageOptions::default() };
//! // ... compose `stage` from `store` with `options` ...
//! let scene = Scene::new(stage, &store);
//! let scope = schemas::Telescope::new(&scene, store.path("/Scope")).expect("a telescope");
//! let aperture: Option<f32> = scope.telescope_aperture();
//! let order = scope.xform_op_order(); // `Xformable`'s
//! ```
//!
//! For several libraries in one registry, register OpenUSD's domains once
//! (`layerstack_schemas::register` with every library's `DEPENDENCIES`),
//! then each library's `register`. `layerstack_custom_schemas` in this
//! repository is such a crate, for the `lab` and `observatory` fixtures.
//!
//! # Resolving upstream schemas
//!
//! A schema file sublayers the schemas it builds on by search path:
//! `@usd/schema.usda@`, `@usdGeom/schema.usda@`. usdGenSchema resolves
//! them with `ArDefaultResolver`, whose search paths it sets to the
//! resource directory of every schema plugin installed. Here a sublayer
//! is looked up next to the layer naming it, then in each search path in
//! order ([`CompileOptions::with_search_path`]):
//!
//! - an OpenUSD source checkout holds each domain's `schema.usda` at
//!   `pxr/usd/<domain>/schema.usda`, so its `pxr/usd` directory is the
//!   search path ([`CompileOptions::with_openusd_source`]);
//! - an OpenUSD installation, or the usd-core wheel's `pxr/pluginfo`,
//!   holds each schema plugin's `<plugin>/resources/<plugin>/schema.usda`,
//!   so each plugin's `resources` directory is one
//!   ([`CompileOptions::with_plugin_root`]).
//!
//! A sublayer found in none of them is [`CompileError::UnresolvedSublayer`],
//! naming the layer and the asset path; usdGenSchema warns and goes on
//! without the layer instead.
//!
//! # What is not ported
//!
//! C++, Python and token code generation (`GenerateCode`,
//! `GenerateBuildHelpers`) and their templates (the Rust module is
//! layerstack's counterpart, not a port); `apiGetImplementation`
//! (only reported by usdGenSchema); the type of an inherited applied API
//! schema from a C++ library usdGenSchema finds through `TfType` (the
//! rules it checks first already reject every case it would); and
//! usdGenSchema's warnings, such as a token renamed to a valid identifier.
//!
//! Spec: AOUSD Core §13 (schemas; §13.1 leaves how schemas are defined to
//! the implementation, and this is OpenUSD's `usdGenSchema.py`), §11
//! (stage population, for the flatten), §16.2 (USDA).

mod classes;
mod codegen;
mod error;
mod names;
mod plug_info;
mod registry;
mod rust;
mod source;

use std::path::{Path, PathBuf};

pub use error::{CompileError, InvalidField, Rule};

/// Where [`compile`] looks for the layers a schema file sublayers, and
/// what it builds `plugInfo.json` on.
#[derive(Clone, Debug, Default)]
pub struct CompileOptions {
    search: Vec<Search>,
    existing_plug_info: Option<String>,
}

#[derive(Clone, Debug)]
enum Search {
    Directory(PathBuf),
    PluginRoot(PathBuf),
}

impl CompileOptions {
    /// No search paths: only sublayers next to the layers naming them
    /// resolve.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a directory that sublayer asset paths resolve in
    /// (`ArDefaultResolver`'s search path), after those added before.
    #[must_use]
    pub fn with_search_path(mut self, directory: impl Into<PathBuf>) -> Self {
        self.search.push(Search::Directory(directory.into()));
        self
    }

    /// Adds the schema sources of an OpenUSD checkout: its `pxr/usd`
    /// directory, which holds `usdGeom/schema.usda` and the other domains'.
    #[must_use]
    pub fn with_openusd_source(self, checkout: impl AsRef<Path>) -> Self {
        self.with_search_path(checkout.as_ref().join("pxr").join("usd"))
    }

    /// Adds the resource directory of each plugin under `root` (each
    /// `<root>/<plugin>/resources`, in name order), as usdGenSchema adds
    /// every schema plugin's: `root` is an OpenUSD installation's
    /// `lib/usd` or the usd-core wheel's `pxr/pluginfo`.
    #[must_use]
    pub fn with_plugin_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.search.push(Search::PluginRoot(root.into()));
        self
    }

    /// Builds `plugInfo.json` on `plug_info`, an existing one (comment
    /// lines included), instead of OpenUSD's template: its generated types
    /// (`autoGenerated`) are replaced and everything else is kept.
    #[must_use]
    pub fn with_existing_plug_info(mut self, plug_info: impl Into<String>) -> Self {
        self.existing_plug_info = Some(plug_info.into());
        self
    }

    /// The directories sublayers resolve in, in order.
    ///
    /// # Errors
    ///
    /// [`CompileError::Read`] when a plugin root cannot be listed.
    pub fn search_paths(&self) -> Result<Vec<PathBuf>, CompileError> {
        let mut paths = Vec::new();
        for search in &self.search {
            match search {
                Search::Directory(directory) => paths.push(directory.clone()),
                Search::PluginRoot(root) => {
                    let read = |e: std::io::Error| CompileError::Read {
                        layer: root.display().to_string(),
                        message: e.to_string(),
                    };
                    let mut plugins: Vec<PathBuf> = std::fs::read_dir(root)
                        .map_err(read)?
                        .filter_map(Result::ok)
                        .map(|entry| entry.path().join("resources"))
                        .filter(|resources| resources.is_dir())
                        .collect();
                    plugins.sort();
                    paths.extend(plugins);
                }
            }
        }
        Ok(paths)
    }
}

/// A compiled schema library: its generated files and its classes.
#[derive(Clone, Debug)]
pub struct CompiledSchema {
    library: SchemaLibrary,
    classes: Vec<SchemaClass>,
    generated_schema: String,
    plug_info: Option<String>,
    rust: rust::Inputs,
}

impl CompiledSchema {
    /// `generatedSchema.usda`.
    #[must_use]
    pub fn generated_schema_usda(&self) -> &str {
        &self.generated_schema
    }

    /// `plugInfo.json`, or `None` for a schema file without classes, for
    /// which usdGenSchema writes none.
    #[must_use]
    pub fn plug_info_json(&self) -> Option<&str> {
        self.plug_info.as_deref()
    }

    /// `plugInfo.json` as installed in a plugin directory that holds it and
    /// `generatedSchema.usda` in `resources/`, as OpenUSD installs a schema
    /// plugin: [`Self::plug_info_json`] with the template's placeholders
    /// filled in for that layout (`Root` `..`, `ResourcePath` `resources`,
    /// an empty `LibraryPath`) and without its comment lines, so it is
    /// strict JSON. Values an existing file set are kept. Register the
    /// `resources` directory (`PXR_PLUGINPATH_NAME`, `Plug.Registry
    /// .RegisterPlugins`). `None` when there are no classes.
    ///
    /// # Errors
    ///
    /// [`CompileError::PlugInfo`] for a library with code whose
    /// `LibraryPath` is still the template's: only its build knows it.
    pub fn installed_plug_info_json(&self) -> Result<Option<String>, CompileError> {
        self.plug_info
            .as_deref()
            .map(|text| plug_info::install(text, self.library.skip_code_generation))
            .transpose()
    }

    /// The library, from the schema file's `/GLOBAL` prim.
    #[must_use]
    pub fn library(&self) -> &SchemaLibrary {
        &self.library
    }

    /// The schema classes, in the schema file's order.
    #[must_use]
    pub fn classes(&self) -> &[SchemaClass] {
        &self.classes
    }

    /// The library as a Rust module, for a crate that depends on
    /// `layerstack` and `layerstack_schemas`: a typed view, edit handle
    /// and token enums for each schema that has a prim definition, in the
    /// shape `layerstack_schemas` gives OpenUSD's own, and `register` and
    /// `registry` functions that register the schemas from static tables.
    /// See [Rust views](self#rust-views).
    ///
    /// The text is not formatted, so a build script needs no `rustfmt`;
    /// `compile --rust <file>` formats it for a checked-in file.
    ///
    /// # Errors
    ///
    /// [`CompileError::Rust`]: an accessor name is a Rust keyword or
    /// collides, or a schema builds on a library other than OpenUSD's
    /// domains.
    pub fn rust_module(&self) -> Result<String, CompileError> {
        rust::module_text(
            &self.library.name,
            &self.classes,
            &self.generated_schema,
            self.plug_info.as_deref(),
            &self.rust,
        )
    }
}

/// A schema library, as the schema file's `/GLOBAL` prim declares it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SchemaLibrary {
    /// `libraryName`: the plugin's name.
    pub name: String,
    /// `libraryPath`, empty for a codeless library.
    pub path: String,
    /// `libraryPrefix`, or `libraryName` in proper case: the C++ class
    /// names' prefix.
    pub prefix: String,
    /// `tokensPrefix`, or the prefix.
    pub tokens_prefix: String,
    /// `useLiteralIdentifier`: whether token identifiers keep their
    /// spelling rather than being camel-cased.
    pub use_literal_identifier: bool,
    /// Whether a layer of the schema stage sets `skipCodeGeneration`: the
    /// library is codeless, its plugin a `resource` plugin.
    pub skip_code_generation: bool,
}

/// One schema of the library, as `plugInfo.json` and `generatedSchema.usda`
/// describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SchemaClass {
    /// The schema identifier: the class prim's name (`Mesh`,
    /// `CollectionAPI`).
    pub name: String,
    /// The C++ type name the plugin registers it as: the library prefix
    /// and `className` (`UsdGeomMesh`).
    pub cpp_class_name: String,
    /// What kind of schema it is.
    pub kind: SchemaKind,
    /// The schema it inherits from, unless it is a base (`Typed`,
    /// `APISchemaBase`).
    pub base: Option<String>,
    /// The C++ type name of its base (`bases` in `plugInfo.json`).
    pub base_cpp_class_name: String,
    /// Its family: the identifier without a version suffix.
    pub family: String,
    /// Its version (`_1` suffix), 0 without one.
    pub version: u32,
    /// The applied schemas it includes as built-ins, as
    /// `generatedSchema.usda` lists them (templated with
    /// `__INSTANCE_NAME__` for a multiple-apply schema).
    pub built_ins: Vec<String>,
    /// `apiSchemaAutoApplyTo`: the schemas a single-apply schema applies
    /// itself to.
    pub auto_apply_to: Vec<String>,
    /// `apiSchemaCanOnlyApplyTo`.
    pub can_only_apply_to: Vec<String>,
    /// `apiSchemaAllowedInstanceNames`.
    pub allowed_instance_names: Vec<String>,
    /// The `apiSchemaCanOnlyApplyTo` of each `apiSchemaInstances` entry.
    pub instance_can_only_apply_to: Vec<(String, Vec<String>)>,
    /// A multiple-apply schema's `propertyNamespacePrefix`.
    pub property_namespace_prefix: Option<String>,
    /// `fallbackTypes`: the types older readers use instead.
    pub fallback_prim_types: Vec<String>,
}

/// What kind of schema a class is: usdGenSchema's `schemaKind`.
///
/// Spec: AOUSD Core §13.3 (typed and API schemas).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SchemaKind {
    /// A base no prim is typed with (`Typed`, `APISchemaBase`).
    AbstractBase,
    /// A typed schema only other typed schemas inherit from.
    AbstractTyped,
    /// A typed schema a prim's type name may name.
    ConcreteTyped,
    /// An API schema that is never applied.
    NonAppliedApi,
    /// An API schema applied at most once.
    SingleApplyApi,
    /// An API schema applied once per instance name.
    MultipleApplyApi,
}

impl SchemaKind {
    /// The name `plugInfo.json` gives it (`concreteTyped`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AbstractBase => "abstractBase",
            Self::AbstractTyped => "abstractTyped",
            Self::ConcreteTyped => "concreteTyped",
            Self::NonAppliedApi => "nonAppliedAPI",
            Self::SingleApplyApi => "singleApplyAPI",
            Self::MultipleApplyApi => "multipleApplyAPI",
        }
    }

    /// The kind as the schema registry has it, for a schema that has a
    /// prim definition: `None` for a base or a non-applied API schema.
    #[must_use]
    pub fn registry_kind(self) -> Option<layerstack::SchemaKind> {
        match self {
            Self::AbstractTyped => Some(layerstack::SchemaKind::AbstractTyped),
            Self::ConcreteTyped => Some(layerstack::SchemaKind::ConcreteTyped),
            Self::SingleApplyApi => Some(layerstack::SchemaKind::SingleApplyApi),
            Self::MultipleApplyApi => Some(layerstack::SchemaKind::MultipleApplyApi),
            Self::AbstractBase | Self::NonAppliedApi => None,
        }
    }

    /// Whether it is an API schema.
    #[must_use]
    pub fn is_api(self) -> bool {
        matches!(
            self,
            Self::NonAppliedApi | Self::SingleApplyApi | Self::MultipleApplyApi
        )
    }
}

/// Compiles the schema file at `source` into the `generatedSchema.usda`
/// and `plugInfo.json` OpenUSD's usdGenSchema writes for it. See the
/// [module docs](self).
///
/// # Errors
///
/// A [`CompileError`]: a layer that cannot be read or resolved, or the
/// first usdGenSchema rule a class breaks.
pub fn compile(source: &Path, options: &CompileOptions) -> Result<CompiledSchema, CompileError> {
    let search = options.search_paths()?;
    let mut stage = source::SchemaStage::open(source, &search)?;
    let parsed = classes::parse(&stage)?;
    let errors = stage.stage().composition_errors();
    if !errors.is_empty() {
        return Err(CompileError::Composition {
            errors: errors.iter().map(|e| format!("{e:?}")).collect(),
        });
    }
    if !parsed.library.skip_code_generation {
        codegen::check(&parsed, &stage.store().tokens)?;
    }
    let plug_info = if parsed.classes.is_empty() {
        None
    } else {
        Some(plug_info::generate(
            &parsed,
            options.existing_plug_info.as_deref(),
            &stage.store().tokens,
        )?)
    };
    let generated_schema = registry::generate(&mut stage, &parsed, source, &search)?;
    let rust = rust::Inputs::new(&parsed);
    let classes = parsed
        .classes
        .into_iter()
        .map(|class| SchemaClass {
            name: class.name,
            cpp_class_name: class.cpp_class_name,
            kind: class.kind,
            base: class.parent_name,
            base_cpp_class_name: class.parent_cpp_class_name,
            family: class.family,
            version: class.version,
            built_ins: class.all_applied_api_schemas,
            auto_apply_to: class.auto_apply_to,
            can_only_apply_to: class.can_only_apply_to,
            allowed_instance_names: class.allowed_instance_names,
            instance_can_only_apply_to: class.instance_can_only_apply_to,
            property_namespace_prefix: class.property_namespace,
            fallback_prim_types: class.fallback_prim_types,
        })
        .collect();
    Ok(CompiledSchema {
        library: parsed.library,
        classes,
        generated_schema,
        plug_info,
        rust,
    })
}

/// The `compile` subcommand: `compile <schema.usda> [--out <dir> |
/// --install <dir>] [--rust <file.rs>] [--search <dir>]...
/// [--openusd-source <dir>]... [--plugin-root <dir>]... [--check]`.
///
/// `--out` writes `generatedSchema.usda` and `plugInfo.json` into `<dir>`
/// as usdGenSchema does, `plugInfo.json` as OpenUSD's template;
/// `--install` writes them into `<dir>/resources` in their installed form
/// ([`CompiledSchema::installed_plug_info_json`]), ready to register.
/// Either builds `plugInfo.json` on the one already there. `--rust`
/// writes the library's Rust module ([`CompiledSchema::rust_module`]),
/// formatted with `rustfmt`. At least one of them is required. With
/// `--check` it writes nothing and fails when any file differs.
pub(crate) fn run(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut source = None;
    let mut out = None;
    let mut install = None;
    let mut rust = None;
    let mut check = false;
    let mut options = CompileOptions::new();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| format!("{flag} requires a directory"))
        };
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--install" => install = Some(PathBuf::from(value("--install")?)),
            "--rust" => {
                rust = Some(PathBuf::from(
                    args.next().ok_or("--rust requires a Rust file")?,
                ));
            }
            "--search" => options = options.with_search_path(value("--search")?),
            "--openusd-source" => options = options.with_openusd_source(value("--openusd-source")?),
            "--plugin-root" => options = options.with_plugin_root(value("--plugin-root")?),
            "--check" => check = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown argument {other}; see the crate docs"));
            }
            other if source.is_none() => source = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let source = source.ok_or("compile requires a schema.usda")?;
    let out = match (out, &install) {
        (Some(out), None) => Some(out),
        (None, Some(plugin)) => Some(plugin.join("resources")),
        (Some(_), Some(_)) => return Err("compile takes --out or --install, not both".into()),
        (None, None) if rust.is_some() => None,
        (None, None) => {
            return Err(
                "compile requires --out <directory>, --install <directory> or --rust <file>".into(),
            );
        }
    };
    let plug_info_path = out.as_ref().map(|out| out.join("plugInfo.json"));
    if let Some(Ok(existing)) = plug_info_path.as_ref().map(std::fs::read_to_string) {
        options = options.with_existing_plug_info(existing);
    }
    let compiled = compile(&source, &options).map_err(|e| e.to_string())?;
    let installed = if install.is_some() {
        compiled
            .installed_plug_info_json()
            .map_err(|e| e.to_string())?
    } else {
        None
    };
    let module = rust
        .as_ref()
        .map(|_| {
            compiled
                .rust_module()
                .map_err(|e| e.to_string())
                .and_then(|text| crate::rustfmt(&text))
        })
        .transpose()?;
    let mut files: Vec<(PathBuf, &str)> = Vec::new();
    if let (Some(out), Some(plug_info_path)) = (&out, plug_info_path) {
        files.push((
            out.join("generatedSchema.usda"),
            compiled.generated_schema_usda(),
        ));
        if let Some(plug_info) = installed.as_deref().or(compiled.plug_info_json()) {
            files.push((plug_info_path, plug_info));
        }
    }
    if let (Some(path), Some(module)) = (&rust, &module) {
        files.push((path.clone(), module));
    }
    if check {
        let stale: Vec<String> = files
            .iter()
            .filter(|(path, text)| std::fs::read_to_string(path).ok().as_deref() != Some(*text))
            .map(|(path, _)| path.display().to_string())
            .collect();
        if !stale.is_empty() {
            return Err(format!(
                "stale against {}: {}; compile without --check",
                source.display(),
                stale.join(", ")
            ));
        }
        return Ok(format!(
            "{} {} schemas are up to date",
            compiled.classes().len(),
            compiled.library().name
        ));
    }
    if let Some(out) = &out {
        std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;
    }
    for (path, text) in &files {
        crate::output::write_if_changed(path, text)?;
    }
    Ok(format!(
        "compiled {} {} schemas into {}",
        compiled.classes().len(),
        compiled.library().name,
        files
            .iter()
            .map(|(path, _)| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
mod tests;
