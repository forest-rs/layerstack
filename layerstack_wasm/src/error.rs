// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The error the bindings throw.

use alloc::string::{String, ToString};
use core::fmt;

use layerstack_io::IoError;
use wasm_bindgen::JsValue;

use crate::wire::Wire;

/// Why a call into the bindings failed: a stable code, the failure in
/// words, and the source and path it is about.
///
/// The TypeScript wrapper turns it into a `LayerstackError` with the same
/// fields. Codes are dotted lowercase, a family and a case:
///
/// - `io.*`: [`layerstack_io::IoErrorKind::code`], such as `io.unavailable`
///   or `io.rejected`.
/// - `path.invalid`: a path argument is not an absolute prim path (or a
///   property path, where one is asked for).
/// - `prim.not_found`: no composed prim is at the path.
/// - `layer.unknown`: no layer of the stage has the id.
/// - `value.decode`: a retained array failed to decode when read.
/// - `value.unknown_type`, `value.kind`, `value.shape`, `value.range`,
///   `value.unsupported`: a value sent for an edit does not make a value
///   of its type (see `value::decode`).
/// - `argument.invalid`: an argument the wrapper checks is malformed
///   (an unknown specifier or list position, a value slot never filled).
/// - `layer.read_only`: an edit targets a layer that cannot be saved, such
///   as a package member; open with a session layer to edit it. A move or
///   delete that would change such a layer fails with it too
///   (`NamespaceError::ReadOnlyLayer`), naming the layer.
/// - `layer.outside_stack`: an edit targets a layer outside the stage's
///   layer stack, such as a referenced asset's.
/// - `layer.muted`: an edit targets a muted layer, which the stage does
///   not compose until it is unmuted.
/// - `mute.root_layer`: a request to mute the root layer, by id or by any
///   spelling of its identifier, which cannot be muted
///   ([`layerstack::LayerMuteError`]).
/// - `mute.refused`: a mute request the live stage refuses for a reason
///   this module does not know ([`layerstack::LayerMuteError`] is
///   non-exhaustive).
/// - `edit.*`: [`layerstack::EditError::code`], such as `edit.stale` for
///   an undo whose guard no longer holds, or
///   `edit.rejected.type_mismatch`; [`layerstack_io::ApplyError::code`].
/// - `namespace.*`: a move or delete that cannot be prepared, from
///   [`layerstack::NamespaceError`]: `namespace.invalid_paths`,
///   `namespace.not_found`, `namespace.collision`,
///   `namespace.missing_parent`, `namespace.cycle`,
///   `namespace.instance_proxy`, `namespace.unmappable`,
///   `namespace.split_opinions`, `namespace.unsupported`.
/// - `subset.*`: `subset.geometry`, a subset read or created on a prim
///   that is not geometry of the subsets' element type (the material
///   subsets of a prim that is not a mesh), from
///   `layerstack_schemas::subset::SubsetError` and
///   `layerstack_schemas::MaterialSubsetError`; and the refusals of
///   `layerstack_schemas::subset::SubsetAuthoringError`:
///   `subset.indices`, `subset.invalid_name`, `subset.name_taken`,
///   `subset.family_declaration`, `subset.unrestricted_material_bind`.
/// - `material.not_found`: a binding names a material where no prim is,
///   on the stage or defined earlier in the batch
///   (`layerstack_schemas::MaterialBindingError::MissingMaterial`), as
///   OpenUSD's `Bind` refuses it; `material.not_material`: a base material
///   set on a prim that is not a `Material`;
///   `material.binding_name`: a binding relationship name that is not a
///   property name (`layerstack_schemas::MaterialBindingError`).
/// - `shading.*`: `shading.no_port`, `shading.not_shader`, and the
///   refusals of `layerstack_schemas::shading::PortError`:
///   `shading.type_mismatch`, `shading.invalid_name`,
///   `shading.invalid_attribute`.
/// - `schema.cannot_edit`: a schema edit of a prim beneath an instance or
///   in a prototype (`layerstack_schemas::CannotEdit`);
///   `schema.cannot_apply`: an API schema the prim may not have.
/// - `coord_sys.no_binding`: clearing a coordinate-system binding that
///   does not exist (`layerstack_schemas::CoordSysError`).
/// - `metadata.key_path`, `metadata.field`: an `sdrMetadata` edit
///   `layerstack_schemas::MetadataError` refuses.
/// - `flatten.failed`, `package.failed`: the stage has no root layer to
///   flatten or package.
/// - `receipt.affinity`: a receipt of another stage was passed in.
/// - `receipt.released`: the receipt was already undone or released.
///
/// Codes are part of the public contract: a released code keeps its
/// spelling and meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// The stable code naming the kind of failure.
    pub code: &'static str,
    /// The failure in words.
    pub message: String,
    /// The source identifier the failure is about, if any.
    pub identifier: Option<String>,
    /// The prim or property path the failure is about, if any.
    pub path: Option<String>,
    /// The index of the edit the failure is about, in the edits sent to
    /// `apply`, if any.
    pub edit: Option<u32>,
}

impl Error {
    /// An error with `code` and `message`, naming nothing.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            identifier: None,
            path: None,
            edit: None,
        }
    }

    /// This error, naming the path it is about.
    #[must_use]
    pub fn at_path(mut self, path: &str) -> Self {
        self.path = Some(path.to_string());
        self
    }

    /// This error, naming the edit it is about.
    #[must_use]
    pub fn at_edit(mut self, edit: u32) -> Self {
        self.edit = Some(edit);
        self
    }

    /// The positional record the wrapper decodes:
    /// `[code, message, identifier, path, edit]`.
    pub(crate) fn to_wire(&self) -> Wire {
        Wire::List(alloc::vec![
            Wire::text(self.code),
            Wire::text(self.message.clone()),
            Wire::text_or_null(self.identifier.clone()),
            Wire::text_or_null(self.path.clone()),
            self.edit
                .map_or(Wire::Null, |edit| Wire::Number(f64::from(edit))),
        ])
    }
}

impl From<IoError> for Error {
    fn from(error: IoError) -> Self {
        Self {
            code: error.code(),
            message: error.message,
            identifier: error.identifier,
            path: None,
            edit: None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl core::error::Error for Error {}

impl From<Error> for JsValue {
    fn from(error: Error) -> Self {
        error.to_wire().into()
    }
}
