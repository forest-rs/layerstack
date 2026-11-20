// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Opt-in checks for identifier-based `MaterialX` Shader output-to-input edits.

use super::{MaterialXNode, MaterialXPort};
use crate::diagnostic::{Describe, Names, with_store};
use crate::shading::{ConnectionModification, ConnectionSource, PortEdit, PortError, PortKind};
use crate::usd_shade::{NodeDefApiImplementationSource, Shader};
use crate::{CannotEdit, SchemaEdit, value};
use alloc::string::String;
use core::fmt;
use layerstack::{LayerStore, PathId, PropertyType};

/// Why a checked `MaterialX` connection could not be authored.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaterialXConnectionError {
    /// The destination must be an input and the source must be an output.
    WrongDirection {
        /// Port identified by its prim and full USD attribute name.
        port: ConnectionSource,
        /// Direction required at this endpoint.
        expected: PortKind,
    },
    /// The endpoint requires a `Shader` prim.
    NotShader {
        /// Owning prim.
        prim: PathId,
    },
    /// The shader's implementation uses source code or a source asset.
    NotIdentifierBased {
        /// Owning prim.
        prim: PathId,
        /// Effective implementation source.
        source: NodeDefApiImplementationSource,
    },
    /// The shader has no readable identifier or the library has no such definition.
    UnknownDefinition {
        /// Owning prim.
        prim: PathId,
        /// The shader's `info:id`, when present.
        id: Option<String>,
    },
    /// The known definition does not declare this port in the required direction.
    UnknownPort {
        /// Port identified by its prim and full USD attribute name.
        port: ConnectionSource,
        /// Identifier of the node definition.
        definition: &'static str,
    },
    /// The two ports have different `MaterialX` types.
    TypeMismatch {
        /// The destination input.
        input: ConnectionSource,
        /// The source output.
        output: ConnectionSource,
        /// The destination input's `MaterialX` type.
        input_type: &'static str,
        /// The source output's `MaterialX` type.
        output_type: &'static str,
    },
    /// A USD declaration disagrees with the definition's storage mapping.
    UsdTypeMismatch {
        /// Port identified by its prim and full USD attribute name.
        port: ConnectionSource,
        /// USD type required by the node definition.
        expected: &'static str,
        /// USD type the edit would use for this port.
        actual: String,
    },
    /// The edit target cannot author a required port.
    CannotEdit {
        /// Owning prim.
        prim: PathId,
        /// Reason authoring is prohibited.
        error: CannotEdit,
    },
    /// A shading attribute is invalid or its owning prim does not exist.
    Port(PortError),
}

impl MaterialXConnectionError {
    /// Writes the error with prim paths resolved through the stage's store.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl fmt::Display + 'a {
        with_store(self, store)
    }
}

impl Describe for MaterialXConnectionError {
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result {
        f.write_str("cannot connect MaterialX ports: ")?;
        let show =
            |port: &ConnectionSource| alloc::format!("{}.{}", names.path(port.prim()), port.name());
        match self {
            Self::WrongDirection { port, expected } => {
                let direction = match expected {
                    PortKind::Input => "an input",
                    PortKind::Output => "an output",
                };
                write!(f, "{} must be {direction}", show(port))
            }
            Self::NotShader { prim } => write!(f, "{} must be a Shader", names.path(*prim)),
            Self::NotIdentifierBased { prim, source } => write!(
                f,
                "{} requires `info:implementationSource = id` for node definition lookup. The effective source is `{}`",
                names.path(*prim),
                source.as_str()
            ),
            Self::UnknownDefinition { prim, id: Some(id) } => write!(
                f,
                "no definition for `{id}` on {} in the supplied library",
                names.path(*prim)
            ),
            Self::UnknownDefinition { prim, id: None } => {
                write!(f, "{} has no readable `info:id`", names.path(*prim))
            }
            Self::UnknownPort { port, definition } => {
                write!(
                    f,
                    "definition `{definition}` has no port for {}",
                    show(port)
                )
            }
            Self::TypeMismatch {
                input,
                output,
                input_type,
                output_type,
            } => write!(
                f,
                "{} supplies `{output_type}` to {}, which requires `{input_type}`",
                show(output),
                show(input),
            ),
            Self::UsdTypeMismatch {
                port,
                expected,
                actual,
            } => write!(
                f,
                "{} requires USD type `{expected}`, but the connection would use `{actual}`",
                show(port)
            ),
            Self::CannotEdit { prim, error } => {
                write!(f, "{}: ", names.path(*prim))?;
                error.describe(f, names)
            }
            Self::Port(error) => error.describe(f, names),
        }
    }
}

impl fmt::Display for MaterialXConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.describe(f, Names(None))
    }
}

impl core::error::Error for MaterialXConnectionError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::CannotEdit { error, .. } => Some(error),
            Self::Port(error) => Some(error),
            _ => None,
        }
    }
}

impl PortEdit {
    /// Checks a `MaterialX` Shader output-to-input connection without adding edits.
    ///
    /// Identifiers and port declarations are taken from the stage and edits
    /// already queued in `edit`. Definitions are selected by each Shader's
    /// `info:id` in `definitions` (the first matching entry). The existing
    /// input and any existing output must have the USD types declared by those
    /// definitions. The `MaterialX` types must match exactly.
    ///
    /// This checks node interfaces. Use [`crate::shading::Port::can_connect`]
    /// for USD connectability on a composed scene.
    ///
    /// # Errors
    /// Returns the same validation errors as [`Self::connect_to_materialx_source`].
    pub fn check_materialx_source(
        &self,
        edit: &mut SchemaEdit<'_>,
        source: impl Into<ConnectionSource>,
        definitions: &[MaterialXNode],
    ) -> Result<(), MaterialXConnectionError> {
        check_connection(self, edit, &source.into(), definitions)
    }

    /// Connects a `MaterialX` Shader output after checking both node interfaces.
    ///
    /// Accepts a generated output `PortEdit` or a generic [`ConnectionSource`].
    /// A missing output is created as in [`Self::connect_to_source`]. Pass the
    /// generated definitions the two shaders use, such as a slice of custom
    /// definitions or `materialx::STANDARD_NODES` with the `usd-mtlx` feature.
    /// The resulting connection uses ordinary USD list opinions and edit-target
    /// mapping (AOUSD Core §7.6.4.2.3 and §12.4).
    ///
    /// # Errors
    /// Rejects incompatible `MaterialX` directions, definitions or types, and
    /// invalid USD declarations before changing the transaction. Existing
    /// authored and queued connections are preserved on error.
    pub fn connect_to_materialx_source(
        &self,
        edit: &mut SchemaEdit<'_>,
        source: impl Into<ConnectionSource>,
        definitions: &[MaterialXNode],
        modification: ConnectionModification,
    ) -> Result<Self, MaterialXConnectionError> {
        let source = source.into();
        check_connection(self, edit, &source, definitions)?;
        self.connect_to_source(edit, source, modification)
            .map_err(MaterialXConnectionError::Port)
    }
}

fn check_connection(
    input: &PortEdit,
    edit: &mut SchemaEdit<'_>,
    output: &ConnectionSource,
    definitions: &[MaterialXNode],
) -> Result<(), MaterialXConnectionError> {
    use PortKind::{Input, Output};

    let input_definition = port_definition(edit, definitions, input.prim(), input.name(), Input)?;
    let output_definition =
        port_definition(edit, definitions, output.prim(), output.name(), Output)?;
    if input_definition.type_name != output_definition.type_name {
        return Err(MaterialXConnectionError::TypeMismatch {
            input: ConnectionSource::from(input),
            output: output.clone(),
            input_type: input_definition.type_name,
            output_type: output_definition.type_name,
        });
    }

    check_editable(edit, input.prim())?;
    let input_type = edit
        .attribute_type(input.prim(), input.name())
        .ok_or_else(|| invalid_port(input.prim(), input.name()))?;
    check_usd_type(input.prim(), input.name(), input_definition, &input_type)?;
    let output_type = match edit.attribute_type(output.prim(), output.name()) {
        Some(ty) => ty,
        None => {
            if edit.property_kind(output.prim(), output.name()).is_some() {
                return Err(invalid_port(output.prim(), output.name()));
            }
            check_editable(edit, output.prim())?;
            // ConnectToSource declares a missing source with its consumer's type.
            input_type
        }
    };
    check_usd_type(
        output.prim(),
        output.name(),
        output_definition,
        &output_type,
    )
}

fn port_definition(
    edit: &mut SchemaEdit<'_>,
    definitions: &[MaterialXNode],
    prim: PathId,
    name: &str,
    direction: PortKind,
) -> Result<&'static MaterialXPort, MaterialXConnectionError> {
    let Some(base) = name.strip_prefix(direction.prefix()) else {
        return Err(MaterialXConnectionError::WrongDirection {
            port: ConnectionSource::new(prim, name),
            expected: direction,
        });
    };
    if !base.split(':').all(layerstack::ident::is_identifier) {
        return Err(MaterialXConnectionError::Port(PortError::InvalidName(
            name.into(),
        )));
    }
    if !edit.exists(prim) {
        return Err(MaterialXConnectionError::Port(PortError::MissingPrim(prim)));
    }
    if !edit.is_a(prim, Shader::SCHEMA) {
        return Err(MaterialXConnectionError::NotShader { prim });
    }

    let authored_source = edit
        .default_value(prim, "info:implementationSource")
        .as_ref()
        .and_then(|value| NodeDefApiImplementationSource::read(value, edit.tokens()));
    let source = NodeDefApiImplementationSource::effective(authored_source.as_ref());
    if source != NodeDefApiImplementationSource::Id {
        return Err(MaterialXConnectionError::NotIdentifierBased { prim, source });
    }

    let value = edit.default_value(prim, "info:id");
    let id = value
        .as_ref()
        .and_then(|value| value::read_token(value, edit.tokens()));
    let node = definitions
        .iter()
        .find(|definition| Some(definition.id) == id)
        .ok_or_else(|| MaterialXConnectionError::UnknownDefinition {
            prim,
            id: id.map(String::from),
        })?;
    node.ports
        .iter()
        .find(|port| port.direction == direction && port.usd_name == name)
        .ok_or_else(|| MaterialXConnectionError::UnknownPort {
            port: ConnectionSource::new(prim, name),
            definition: node.id,
        })
}

fn check_editable(edit: &SchemaEdit<'_>, prim: PathId) -> Result<(), MaterialXConnectionError> {
    edit.can_edit(prim)
        .map_err(|error| MaterialXConnectionError::CannotEdit { prim, error })
}

fn invalid_port(prim: PathId, name: &str) -> MaterialXConnectionError {
    MaterialXConnectionError::Port(PortError::InvalidAttribute {
        prim,
        name: name.into(),
    })
}

fn check_usd_type(
    prim: PathId,
    name: &str,
    definition: &MaterialXPort,
    ty: &PropertyType,
) -> Result<(), MaterialXConnectionError> {
    if ty.type_name() == definition.usd_type_name && ty.value_type().is_some() {
        Ok(())
    } else {
        Err(MaterialXConnectionError::UsdTypeMismatch {
            port: ConnectionSource::new(prim, name),
            expected: definition.usd_type_name,
            actual: ty.type_name().into(),
        })
    }
}

#[cfg(all(test, feature = "usd-mtlx"))]
mod tests;
