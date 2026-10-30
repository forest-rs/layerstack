// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Named shading attributes and source-opinion authoring.

use super::{ConnectedSources, ShaderSources, ValueSources, shading_kind};
use crate::diagnostic::{Describe, Names, with_store};
use crate::{CannotEdit, PrimEdit, PrimView, Scene, SchemaEdit, Time};
use alloc::{format, string::String, vec::Vec};
use layerstack::{
    LayerStore, ListOp, ListPosition, PathId, PropertyPath, PropertyType, TargetPath, Value,
};

/// A shading attribute's namespace, independent of its value type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortKind {
    /// `inputs:`: a shader parameter or a container interface input.
    Input,
    /// `outputs:`: a shader result or a container passthrough.
    Output,
}
impl PortKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Input => "inputs:",
            Self::Output => "outputs:",
        }
    }
    fn of(name: &str) -> Option<Self> {
        shading_kind(name).map(|input| if input { Self::Input } else { Self::Output })
    }
}

/// An existing input or output attribute. Its prim need not be a Shader.
///
/// The view inspects composed state; [`PortEdit::get`] authors source opinions.
/// It does not validate renderer support or plugin-defined connectability.
#[derive(Clone, Copy, Debug)]
pub struct Port<'a> {
    scene: Scene<'a>,
    path: PropertyPath,
    kind: PortKind,
}
impl<'a> Port<'a> {
    /// Gets an existing attribute with an `inputs:` or `outputs:` prefix.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: PropertyPath) -> Option<Self> {
        let kind = PortKind::of(scene.store().tokens().resolve(path.property()))?;
        scene.shading_attribute(path).then_some(Self {
            scene: *scene,
            path,
            kind,
        })
    }
    /// Its composed property path.
    #[must_use]
    pub fn path(&self) -> PropertyPath {
        self.path
    }
    /// Whether it is an input or an output.
    #[must_use]
    pub fn kind(&self) -> PortKind {
        self.kind
    }
    /// The attribute name after its `inputs:` or `outputs:` prefix.
    #[must_use]
    pub fn name(&self) -> &'a str {
        &self.scene.store().tokens().resolve(self.path.property())[self.kind.prefix().len()..]
    }
    /// Its composed declared type, including array and role information.
    #[must_use]
    pub fn property_type(&self) -> Option<PropertyType> {
        let property = self.scene.property(self.path)?;
        property
            .declaration()
            .and_then(|d| d.type_name)
            .or_else(|| property.definition().and_then(|d| d.type_name.clone()))
    }
    /// Reads this attribute's own value at `time`, without following connections.
    /// To find upstream value providers first, use [`Self::value_sources`].
    #[must_use]
    pub fn value(&self, time: Time) -> Option<Value> {
        PrimView::new(self.scene, self.path.prim_path()).raw_value(
            self.scene.store().tokens().resolve(self.path.property()),
            time,
        )
    }
    /// Direct valid sources and invalid targets in composed order.
    #[must_use]
    pub fn connected_sources(&self) -> ConnectedSources {
        self.scene.connected_sources(self.path)
    }
    /// Shader-output providers, excluding authored constants.
    #[must_use]
    pub fn shader_sources(&self) -> ShaderSources {
        self.scene.shader_sources(self.path)
    }
    /// Shader outputs and authored-value providers, with branch evidence.
    #[must_use]
    pub fn value_sources(&self) -> ValueSources {
        self.scene.value_sources(self.path)
    }
    /// Validates a proposed source using built-in connectability policies.
    /// Does not author the connection or check renderer/type support.
    pub fn can_connect(&self, source: PropertyPath) -> Result<(), super::ConnectionError> {
        self.scene.validate_shading_connection(self.path, source)
    }
}

impl<'a> PrimView<'a> {
    /// An existing named shading input, including namespaced base names.
    #[must_use]
    pub fn input(&self, name: &str) -> Option<Port<'a>> {
        Port::get(
            &self.scene(),
            self.property_path(&format!("inputs:{name}"))?,
        )
    }
    /// An existing named shading output, including render-context namespaces.
    #[must_use]
    pub fn output(&self, name: &str) -> Option<Port<'a>> {
        Port::get(
            &self.scene(),
            self.property_path(&format!("outputs:{name}"))?,
        )
    }
    /// Composed ports of `kind`, in property order, including schema properties.
    #[must_use]
    pub fn ports(&self, kind: PortKind) -> Vec<Port<'a>> {
        let scene = self.scene();
        let tokens = scene.store().tokens();
        let Some(prim) = self.prim() else {
            return Vec::new();
        };
        prim.property_names()
            .into_iter()
            .filter_map(|name| {
                Port::get(&scene, PropertyPath::new(self.path(), tokens.lookup(name)?))
            })
            .filter(|port| port.kind == kind)
            .collect()
    }
}

/// A port edit rejected before adding operations to the transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PortError {
    /// The owning prim does not exist in this edit's scene.
    MissingPrim(PathId),
    /// The base name is not a nonempty colon-separated sequence of identifiers.
    InvalidName(String),
    /// An existing property is not a typed shading attribute.
    InvalidAttribute {
        /// Owning prim.
        prim: PathId,
        /// Full property name.
        name: String,
    },
    /// Creating a port would change an existing attribute's declared type.
    TypeMismatch {
        /// Full property name.
        name: String,
    },
}
impl PortError {
    /// The error in words, its prim written as a path with `store`, the
    /// store the stage was composed from. `Display` writes the same with
    /// ids.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl core::fmt::Display + 'a {
        with_store(self, store)
    }
}
impl Describe for PortError {
    fn describe(&self, f: &mut core::fmt::Formatter<'_>, names: Names<'_>) -> core::fmt::Result {
        f.write_str("cannot edit the shading port: ")?;
        match self {
            Self::MissingPrim(path) => write!(f, "no prim is at {}", names.path(*path)),
            Self::InvalidName(name) => write!(
                f,
                "`{name}` is not a nonempty, colon-separated sequence of identifiers"
            ),
            Self::InvalidAttribute { prim, name } => write!(
                f,
                "{}.{name} exists but is not a typed shading attribute",
                names.path(*prim)
            ),
            Self::TypeMismatch { name } => write!(
                f,
                "`{name}` is already declared with another type; the port would change it"
            ),
        }
    }
}
impl core::fmt::Display for PortError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.describe(f, Names(None))
    }
}
impl core::error::Error for PortError {}

/// Where [`PortEdit::connect_to_source`] puts the source among the
/// connections the edit target's spec authors.
///
/// OpenUSD: `UsdShadeConnectionModification`.
#[doc(alias = "UsdShadeConnectionModification")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectionModification {
    /// The source becomes the spec's only connection, an explicit list that
    /// hides weaker layers' connections.
    #[default]
    Replace,
    /// The source goes to the front of the spec's prepended connections, or
    /// of its explicit list when it authors one; weaker layers' connections
    /// stay behind it.
    Prepend,
    /// The source goes to the back of the spec's appended connections, or
    /// of its explicit list when it authors one.
    Append,
}

/// The source of a connection: a prim and the full name of one of its
/// shading attributes (`outputs:rgb`, `inputs:roughness`).
///
/// Build it from the base name with [`Self::output`] or [`Self::input`],
/// as [`PrimEdit::create_output`] and [`PrimEdit::create_input`] take it,
/// from the full name with [`Self::new`], or from a [`PortEdit`] handle
/// (`From`). [`PortEdit::connect_to_source`],
/// [`PortEdit::disconnect_source`] and [`PortEdit::set_sources`] all take
/// it.
///
/// OpenUSD: `UsdShadeConnectionSourceInfo` (source prim, name and type).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionSource {
    prim: PathId,
    name: String,
}
impl ConnectionSource {
    /// The attribute `name` of `prim`, by its full name with its namespace
    /// (`outputs:rgb`). A connection refuses a name outside the `outputs:`
    /// and `inputs:` namespaces ([`PortError::InvalidName`]).
    #[must_use]
    pub fn new(prim: PathId, name: &str) -> Self {
        Self {
            prim,
            name: name.into(),
        }
    }
    /// The output `name` (its base name, `rgb` for `outputs:rgb`) of `prim`.
    #[must_use]
    pub fn output(prim: PathId, name: &str) -> Self {
        Self {
            prim,
            name: format!("{}{name}", PortKind::Output.prefix()),
        }
    }
    /// The input `name` (its base name, `file` for `inputs:file`) of
    /// `prim`, such as a node graph's interface input.
    #[must_use]
    pub fn input(prim: PathId, name: &str) -> Self {
        Self {
            prim,
            name: format!("{}{name}", PortKind::Input.prefix()),
        }
    }
    /// The prim the source attribute is on.
    #[must_use]
    pub fn prim(&self) -> PathId {
        self.prim
    }
    /// The source attribute's full name, with its `outputs:` or `inputs:`
    /// namespace.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}
impl From<&PortEdit> for ConnectionSource {
    fn from(port: &PortEdit) -> Self {
        Self {
            prim: port.prim,
            name: port.name.clone(),
        }
    }
}
impl From<PortEdit> for ConnectionSource {
    fn from(port: PortEdit) -> Self {
        Self {
            prim: port.prim,
            name: port.name,
        }
    }
}

/// A named input or output authored through a [`SchemaEdit`].
///
/// Connections are source opinions: every target is mapped through the edit
/// target when committed, and undo restores authored absence as well as values.
/// Semantic `CanConnect` validation is not performed. Every connection
/// method takes its source as a [`ConnectionSource`] (a prim and a full
/// attribute name) or a `PortEdit`. Sources passed to [`Self::set_sources`]
/// must already exist in the scene or have been created in this edit;
/// [`Self::connect_to_source`] creates a missing one, as C++
/// `ConnectToSource` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortEdit {
    prim: PathId,
    name: String,
}
impl PortEdit {
    /// Gets an existing typed shading attribute, including one created earlier
    /// in this edit. Returns `Ok(None)` for absent prims, relationships or bad
    /// names.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] when the edit may not author the prim, beneath an
    /// instance or in a prototype ([`SchemaEdit::can_edit`]).
    pub fn get(edit: &mut SchemaEdit<'_>, path: PropertyPath) -> Result<Option<Self>, CannotEdit> {
        edit.can_edit(path.prim_path())?;
        let name = String::from(edit.tokens().resolve(path.property()));
        let result = Self {
            prim: path.prim_path(),
            name,
        };
        Ok(result.validate(edit).is_ok().then_some(result))
    }
    /// The owning prim.
    #[must_use]
    pub fn prim(&self) -> PathId {
        self.prim
    }
    /// Full property name, including the shading namespace.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    fn validate(&self, edit: &mut SchemaEdit<'_>) -> Result<(), PortError> {
        if !edit.exists(self.prim) {
            return Err(PortError::MissingPrim(self.prim));
        }
        if PortKind::of(&self.name).is_none()
            || edit.attribute_type(self.prim, &self.name).is_none()
        {
            return Err(PortError::InvalidAttribute {
                prim: self.prim,
                name: self.name.clone(),
            });
        }
        Ok(())
    }
    /// Authors this attribute's default value. Type validation is performed by
    /// the core transaction at commit, as with generated attribute setters.
    pub fn set(&self, edit: &mut SchemaEdit<'_>, value: Value) -> Result<&Self, PortError> {
        self.validate(edit)?;
        edit.set_value(self.prim, &self.name, None, value);
        Ok(self)
    }
    /// Authors a sample in stage time, mapped through the edit target.
    pub fn set_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: Value,
    ) -> Result<&Self, PortError> {
        self.validate(edit)?;
        edit.set_value(self.prim, &self.name, Some(time), value);
        Ok(self)
    }
    /// Replaces this spec's connections with an explicit ordered source list
    /// (each a [`ConnectionSource`], or a [`PortEdit`] handle). Every
    /// source must exist already, on the stage or created in this edit; all
    /// are checked before any operations are appended. An empty list blocks
    /// weaker connections; it does not erase the authored opinion.
    ///
    /// OpenUSD: `UsdShadeConnectableAPI::SetConnectedSources`, for existing
    /// attributes. AOUSD Core §7.6.4.2.3 and §12.4 (connection list composition).
    ///
    /// # Errors
    ///
    /// Before authoring anything: this port or a source does not exist.
    pub fn set_sources(
        &self,
        edit: &mut SchemaEdit<'_>,
        sources: impl IntoIterator<Item = impl Into<ConnectionSource>>,
    ) -> Result<&Self, PortError> {
        self.validate(edit)?;
        let sources: Vec<ConnectionSource> = sources.into_iter().map(Into::into).collect();
        for source in &sources {
            Self {
                prim: source.prim,
                name: source.name.clone(),
            }
            .validate(edit)?;
        }
        let targets = sources
            .iter()
            .map(|source| {
                TargetPath::Property(PropertyPath::new(
                    source.prim,
                    edit.tokens().intern(&source.name),
                ))
            })
            .collect();
        edit.set_connection_op(self.prim, &self.name, Some(ListOp::explicit(targets)));
        Ok(self)
    }
    /// Connects `source` (a [`ConnectionSource`], or a [`PortEdit`]
    /// handle), placing it as `modification` says, and returns the source.
    /// A source attribute that does not exist yet is created with this
    /// port's type, not `custom` and with no value, as C++ creates it;
    /// an existing one keeps its own type, which is not checked against
    /// this port's.
    ///
    /// OpenUSD: `UsdShadeConnectableAPI::ConnectToSource` with a
    /// `UsdShadeConnectionSourceInfo`. AOUSD Core §7.6.4.2.3 and §12.4
    /// (connection list composition).
    ///
    /// # Errors
    ///
    /// Before authoring anything: this port or the source prim does not
    /// exist, the source's name is not a shading attribute name, or a
    /// relationship already has it.
    #[doc(alias = "ConnectToSource")]
    pub fn connect_to_source(
        &self,
        edit: &mut SchemaEdit<'_>,
        source: impl Into<ConnectionSource>,
        modification: ConnectionModification,
    ) -> Result<Self, PortError> {
        self.validate(edit)?;
        let ConnectionSource { prim, name } = source.into();
        let source = self.source_port(edit, prim, &name)?;
        let target = TargetPath::Property(PropertyPath::new(prim, edit.tokens().intern(&name)));
        match modification {
            ConnectionModification::Replace => edit.set_connection_op(
                self.prim,
                &self.name,
                Some(ListOp::explicit(alloc::vec![target])),
            ),
            ConnectionModification::Prepend => {
                edit.add_connection(self.prim, &self.name, target, ListPosition::FrontOfPrepend);
            }
            ConnectionModification::Append => {
                edit.add_connection(self.prim, &self.name, target, ListPosition::BackOfAppend);
            }
        }
        Ok(source)
    }
    /// The existing source `name` of `prim`, or one created with this port's type.
    fn source_port(
        &self,
        edit: &mut SchemaEdit<'_>,
        prim: PathId,
        name: &str,
    ) -> Result<Self, PortError> {
        if !edit.exists(prim) {
            return Err(PortError::MissingPrim(prim));
        }
        let Some(kind) = PortKind::of(name) else {
            return Err(PortError::InvalidName(name.into()));
        };
        let base = &name[kind.prefix().len()..];
        if !base.split(':').all(layerstack::ident::is_identifier) {
            return Err(PortError::InvalidName(name.into()));
        }
        if edit.attribute_type(prim, name).is_none() {
            if edit.property_kind(prim, name).is_some() {
                return Err(PortError::InvalidAttribute {
                    prim,
                    name: name.into(),
                });
            }
            let ty = edit
                .attribute_type(self.prim, &self.name)
                .expect("a validated port has a type");
            edit.create_attribute(prim, name, ty);
        }
        Ok(Self {
            prim,
            name: name.into(),
        })
    }
    /// Removes the one source `source` (a [`ConnectionSource`], or a
    /// [`PortEdit`] handle) from this port's connections: from
    /// the edit target's explicit list when it authors one, else as a
    /// deletion that also removes it from weaker layers' connections. Other
    /// sources stay connected.
    ///
    /// OpenUSD: `UsdShadeConnectableAPI::DisconnectSource` with a source
    /// attribute (`UsdAttribute::RemoveConnection`). AOUSD Core §12.4.
    ///
    /// # Errors
    ///
    /// When this port no longer exists.
    #[doc(alias = "DisconnectSource")]
    pub fn disconnect_source(
        &self,
        edit: &mut SchemaEdit<'_>,
        source: impl Into<ConnectionSource>,
    ) -> Result<&Self, PortError> {
        self.validate(edit)?;
        let source = source.into();
        let target = TargetPath::Property(PropertyPath::new(
            source.prim,
            edit.tokens().intern(&source.name),
        ));
        edit.remove_connection(self.prim, &self.name, target);
        Ok(self)
    }
    /// Disconnects all sources by authoring an explicit empty list.
    /// OpenUSD: `DisconnectSource()` with no source argument.
    pub fn disconnect_sources(&self, edit: &mut SchemaEdit<'_>) -> Result<&Self, PortError> {
        self.set_sources(edit, core::iter::empty::<ConnectionSource>())
    }
    /// Erases this spec's connection opinion, revealing weaker connections.
    /// OpenUSD: `ClearSources`; this differs from [`Self::disconnect_sources`].
    pub fn clear_sources(&self, edit: &mut SchemaEdit<'_>) -> Result<&Self, PortError> {
        self.validate(edit)?;
        edit.set_connection_op(self.prim, &self.name, None);
        Ok(self)
    }
}
impl PrimEdit {
    /// Creates a typed shading input with no authored value, or returns an
    /// existing input of the same type. Invalid names, prims, relationships and
    /// conflicting types are rejected before authoring any operation.
    pub fn create_input(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        ty: PropertyType,
    ) -> Result<PortEdit, PortError> {
        self.create_port(edit, PortKind::Input, name, ty)
    }
    /// As [`Self::create_input`], for a shading output.
    pub fn create_output(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        ty: PropertyType,
    ) -> Result<PortEdit, PortError> {
        self.create_port(edit, PortKind::Output, name, ty)
    }
    fn create_port(
        &self,
        edit: &mut SchemaEdit<'_>,
        kind: PortKind,
        name: &str,
        ty: PropertyType,
    ) -> Result<PortEdit, PortError> {
        if !edit.exists(self.path()) {
            return Err(PortError::MissingPrim(self.path()));
        }
        if !name.split(':').all(layerstack::ident::is_identifier) {
            return Err(PortError::InvalidName(name.into()));
        }
        let name = format!("{}{name}", kind.prefix());
        if let Some(existing) = edit.attribute_type(self.path(), &name) {
            if existing.type_name() != ty.type_name() || existing.is_array() != ty.is_array() {
                return Err(PortError::TypeMismatch { name });
            }
        } else if edit.property_kind(self.path(), &name).is_some() {
            return Err(PortError::InvalidAttribute {
                prim: self.path(),
                name,
            });
        } else {
            edit.create_attribute(self.path(), &name, ty);
        }
        Ok(PortEdit {
            prim: self.path(),
            name,
        })
    }
}
