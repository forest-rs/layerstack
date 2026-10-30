// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Host-registered file formats: layers a document reads through code the
//! host supplies, such as procedural generators, from a source file's bytes
//! and the format arguments of the asset path naming it.
//!
//! A [`FileFormat`] claims file extensions and builds a [`Layer`] from a
//! [`FormatSource`]. Formats are registered explicitly in a [`FileFormats`]
//! set passed through [`LoadOptions::formats`](crate::LoadOptions::formats);
//! there is no global registry or plugin discovery. USDA, USDC and USDZ are
//! built in and cannot be claimed.
//!
//! An asset path may carry format arguments after `:SDF_FORMAT_ARGS:`, as
//! `terrain.grid:SDF_FORMAT_ARGS:rows=4&seed=7`. They are part of the layer's
//! identity: the same file read with different arguments is a different
//! layer, read once per distinct set. The identity drops `target` and every
//! argument equal to the format's default, so `rows=4` and no `rows` name one
//! layer when the default is `4`.
//!
//! ```
//! use layerstack::{Layer, PrimSpec};
//! use layerstack_io::{FileFormat, FileFormats, FormatSource, IoError};
//!
//! /// One prim per row named by the `rows` argument.
//! #[derive(Debug)]
//! struct Grid;
//! impl FileFormat for Grid {
//!     fn name(&self) -> &str {
//!         "grid"
//!     }
//!     fn extensions(&self) -> &[&str] {
//!         &["grid"]
//!     }
//!     fn read(&self, source: &FormatSource<'_>) -> Result<Layer, IoError> {
//!         let rows: usize = source.args.get("rows").unwrap_or("1").parse().map_err(|_| {
//!             source.rejected("`rows` must be a count")
//!         })?;
//!         let mut layer = Layer::new(source.layer);
//!         for row in 0..rows {
//!             let path = source.path(&format!("/Row{row}"))?;
//!             layer.define_prim(path, PrimSpec::def(), source.paths);
//!         }
//!         Ok(layer)
//!     }
//! }
//! let formats = FileFormats::default().with(std::sync::Arc::new(Grid)).unwrap();
//! assert_eq!(formats.for_identifier("fields/a.grid").map(|f| f.name()), Some("grid"));
//! ```
//!
//! OpenUSD: `SdfFileFormat` (`pxr/usd/sdf/fileFormat.h`), format arguments
//! and identifiers (`Sdf_SplitIdentifier`, `Sdf_CreateIdentifier` in
//! `pxr/usd/sdf/assetPathResolver.cpp`; `_CanonicalizeFileFormatArguments` in
//! `pxr/usd/sdf/layer.cpp`). AOUSD Core §9 (asset resolution).

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::fmt;

use layerstack::{
    DynamicArgs, DynamicFileFormat, DynamicFormatContext, Layer, LayerId, Path, PathId,
    PathInterner, TokenId, TokenInterner, Value,
};

use crate::{IoError, IoErrorKind};

pub use layerstack::asset::{FORMAT_ARGS_DELIMITER, FormatArgs, FormatArgsError};

/// The `target` argument, which selects a format among those claiming an
/// extension. Every format here is the only one for its extensions, so the
/// argument never takes part in a layer's identity.
const TARGET: &str = "target";

/// Splits `asset` into its path and format arguments ([`FormatArgs::split`]).
///
/// # Errors
///
/// [`IoErrorKind::Rejected`] when an argument has no `=`.
pub(crate) fn split_args(asset: &str) -> Result<(&str, FormatArgs), IoError> {
    FormatArgs::split(asset).map_err(|error| IoError::new(IoErrorKind::Rejected, error.to_string()))
}

/// `args` without `target` and without those equal to `format`'s defaults:
/// the arguments that name a distinct layer.
///
/// OpenUSD: `_CanonicalizeFileFormatArguments` (`pxr/usd/sdf/layer.cpp`).
#[must_use]
pub(crate) fn canonical_args(mut args: FormatArgs, format: Option<&dyn FileFormat>) -> FormatArgs {
    args.remove(TARGET);
    if let Some(format) = format {
        let defaults = format.default_args();
        args.retain(|key, value| defaults.get(key) != Some(value));
    }
    args
}

/// What a [`FileFormat`] reads a layer from.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct FormatSource<'a> {
    /// The layer's identifier: the source's storage identifier, followed by
    /// its arguments when it has any ([`FormatArgs::identifier`]).
    pub identifier: &'a str,
    /// The id the layer must have ([`Layer::new`]).
    pub layer: LayerId,
    /// The source's bytes; `None` when the format does not read them
    /// ([`FileFormat::reads_source`]).
    pub bytes: Option<&'a [u8]>,
    /// The layer's arguments, without `target` or defaults.
    pub args: &'a FormatArgs,
    /// The document's token interner, for the layer's names.
    pub tokens: &'a TokenInterner,
    /// The document's path interner, for the layer's paths.
    pub paths: &'a PathInterner,
}

impl fmt::Debug for FormatSource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FormatSource")
            .field("identifier", &self.identifier)
            .field("layer", &self.layer)
            .field("bytes", &self.bytes.map(<[u8]>::len))
            .field("args", &self.args)
            .finish_non_exhaustive()
    }
}

impl FormatSource<'_> {
    /// The id of the absolute prim path `path`, as `/Grid/Row0`.
    ///
    /// # Errors
    ///
    /// [`FormatSource::rejected`] when `path` is not an absolute path.
    pub fn path(&self, path: &str) -> Result<PathId, IoError> {
        let parsed = Path::parse_absolute(path, self.tokens)
            .map_err(|error| self.rejected(alloc::format!("`{path}`: {error}")))?;
        Ok(self.paths.intern(parsed))
    }

    /// The id of the token `name`.
    #[must_use]
    pub fn token(&self, name: &str) -> TokenId {
        self.tokens.intern(name)
    }

    /// An error rejecting this source, naming it: for a read that cannot
    /// build a layer from what it was given.
    #[must_use]
    pub fn rejected(&self, reason: impl fmt::Display) -> IoError {
        IoError::new(IoErrorKind::Rejected, alloc::format!("{reason}")).at(self.identifier)
    }
}

/// A file format a host supplies: code that builds layers from sources with
/// the extensions it claims.
///
/// Build the layer's prims with [`Layer::define_prim`], which lists each
/// among its parent's children, so the layer can be saved or exported.
/// The layer a read returns may author arcs by asset path, anchored to it:
/// sublayers as [`SublayerEntry::unresolved`](layerstack::SublayerEntry::unresolved),
/// which the document resolves as it reads the layer, and references and
/// payloads as [`Reference::deferred`](layerstack::Reference::deferred) with
/// [`FormatSource::layer`], which composition asks for and the document loads
/// ([`layerstack::Stage::asset_requests`]). Paths and tokens must come from
/// the source's interners.
///
/// Reads may run on several threads at once when a stage opens on several
/// ([`layerstack::StageOptions::threads`]), and must be deterministic: the
/// document reads a source again when it reloads it.
///
/// OpenUSD: `SdfFileFormat` (`pxr/usd/sdf/fileFormat.h`).
pub trait FileFormat: fmt::Debug + Send + Sync {
    /// The format's name, for diagnostics.
    fn name(&self) -> &str;

    /// The file extensions it reads, without the dot, in lowercase.
    fn extensions(&self) -> &[&str];

    /// Arguments a source is read with when its asset path does not name
    /// them. An argument equal to its default does not distinguish a layer.
    fn default_args(&self) -> FormatArgs {
        FormatArgs::new()
    }

    /// Whether reading needs the source's bytes. A format that builds its
    /// layers from their arguments alone returns `false`, and the document
    /// identifies the source but does not read it.
    fn reads_source(&self) -> bool {
        true
    }

    /// Builds the layer `source` names.
    ///
    /// # Errors
    ///
    /// An error for a source it cannot read, typically
    /// [`FormatSource::rejected`]; the document reports it as it reports a
    /// malformed USDA layer.
    fn read(&self, source: &FormatSource<'_>) -> Result<Layer, IoError>;

    /// The format's dynamic side, when the payloads to it compose the
    /// arguments its layers are read with (see [`layerstack::dynamic_format`]);
    /// `None`, the default, for a format read with its asset paths'
    /// arguments alone.
    ///
    /// Registering the format registers its dynamic side too: the document
    /// composes its stage with it ([`layerstack::StageOptions::dynamic_formats`]),
    /// under this format's name and extensions, and reads the layers it
    /// authors leaving payloads to the format for composition
    /// ([`layerstack::AssetResolver::defers_payload`]). A format implementing
    /// both traits returns `Some(self)`.
    ///
    /// OpenUSD: an `SdfFileFormat` that is also a
    /// `PcpDynamicFileFormatInterface`.
    fn dynamic(&self) -> Option<&dyn DynamicFileFormat> {
        None
    }

    /// Serializes `layer` for saving or exporting to this format.
    ///
    /// # Errors
    ///
    /// The default rejects with [`IoErrorKind::Unsupported`]: formats are
    /// read-only unless they write.
    fn write(
        &self,
        layer: &Layer,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<Vec<u8>, IoError> {
        let _ = (layer, tokens, paths);
        Err(IoError::new(
            IoErrorKind::Unsupported,
            alloc::format!("the {} format does not write layers", self.name()),
        ))
    }
}

/// Why a format could not be registered.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FormatRegistrationError {
    /// The format claims no extension.
    NoExtensions {
        /// The format's name.
        format: String,
    },
    /// A built-in format (`usda`, `usdc`, `usd`, `usdz`) already reads the
    /// extension.
    BuiltinExtension {
        /// The format's name.
        format: String,
        /// The extension.
        extension: String,
    },
    /// Another registered format already reads the extension.
    DuplicateExtension {
        /// The format's name.
        format: String,
        /// The extension.
        extension: String,
        /// The format registered for it first.
        registered: String,
    },
    /// An extension is empty, has a dot, or has uppercase letters.
    InvalidExtension {
        /// The format's name.
        format: String,
        /// The extension.
        extension: String,
    },
}

impl fmt::Display for FormatRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoExtensions { format } => write!(f, "format {format} claims no extension"),
            Self::BuiltinExtension { format, extension } => {
                write!(f, "format {format} claims built-in extension .{extension}")
            }
            Self::DuplicateExtension {
                format,
                extension,
                registered,
            } => write!(
                f,
                "format {format} claims .{extension}, which {registered} reads"
            ),
            Self::InvalidExtension { format, extension } => write!(
                f,
                "format {format} claims `{extension}`, which is not a lowercase extension"
            ),
        }
    }
}

impl core::error::Error for FormatRegistrationError {}

/// The file formats a document reads beyond USDA, USDC and USDZ, by
/// extension. Cloning shares the formats.
#[derive(Clone, Debug, Default)]
pub struct FileFormats {
    formats: Vec<Arc<dyn FileFormat>>,
    /// The dynamic sides of the formats that have one, in registration
    /// order.
    dynamic: Vec<Arc<dyn DynamicFileFormat>>,
}

/// The dynamic side of a registered [`FileFormat`], under its name and
/// extensions.
#[derive(Debug)]
struct DynamicSide(Arc<dyn FileFormat>);

impl DynamicSide {
    fn side(&self) -> &dyn DynamicFileFormat {
        self.0.dynamic().expect("registered for its dynamic side")
    }
}

impl DynamicFileFormat for DynamicSide {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn extensions(&self) -> &[&str] {
        self.0.extensions()
    }
    fn compose_args(
        &self,
        asset_path: &str,
        context: &mut DynamicFormatContext<'_>,
    ) -> DynamicArgs {
        self.side().compose_args(asset_path, context)
    }
    fn can_field_change_affect_args(
        &self,
        field: &str,
        old: Option<&Value>,
        new: Option<&Value>,
        dependency: &Value,
    ) -> bool {
        self.side()
            .can_field_change_affect_args(field, old, new, dependency)
    }
    fn can_attribute_default_change_affect_args(
        &self,
        attribute: &str,
        old: Option<&Value>,
        new: Option<&Value>,
        dependency: &Value,
    ) -> bool {
        self.side()
            .can_attribute_default_change_affect_args(attribute, old, new, dependency)
    }
}

impl FileFormats {
    /// Registers `format` for its extensions.
    ///
    /// # Errors
    ///
    /// When it claims no extension, an invalid one, a built-in one, or one
    /// another registered format reads; nothing is registered then.
    pub fn add(&mut self, format: Arc<dyn FileFormat>) -> Result<(), FormatRegistrationError> {
        let name = || format.name().to_string();
        if format.extensions().is_empty() {
            return Err(FormatRegistrationError::NoExtensions { format: name() });
        }
        for &extension in format.extensions() {
            if extension.is_empty()
                || extension.contains('.')
                || extension.chars().any(char::is_uppercase)
            {
                return Err(FormatRegistrationError::InvalidExtension {
                    format: name(),
                    extension: extension.into(),
                });
            }
            if matches!(extension, "usda" | "usdc" | "usd" | "usdz") {
                return Err(FormatRegistrationError::BuiltinExtension {
                    format: name(),
                    extension: extension.into(),
                });
            }
            if let Some(registered) = self.for_extension(extension) {
                return Err(FormatRegistrationError::DuplicateExtension {
                    format: name(),
                    extension: extension.into(),
                    registered: registered.name().into(),
                });
            }
        }
        if format.dynamic().is_some() {
            self.dynamic
                .push(Arc::new(DynamicSide(Arc::clone(&format))));
        }
        self.formats.push(format);
        Ok(())
    }

    /// The dynamic sides of the registered formats that have one
    /// ([`FileFormat::dynamic`]), in registration order, as the document
    /// adds them to [`layerstack::StageOptions::dynamic_formats`].
    pub fn dynamic_formats(&self) -> impl ExactSizeIterator<Item = &Arc<dyn DynamicFileFormat>> {
        self.dynamic.iter()
    }

    /// Whether the asset `asset_path` names is read by a format with a
    /// dynamic side, whose payloads composition resolves.
    pub(crate) fn is_dynamic(&self, asset_path: &str) -> bool {
        !self.dynamic.is_empty()
            && self
                .for_identifier(asset_path)
                .is_some_and(|format| format.dynamic().is_some())
    }

    /// These formats and `format` (see [`Self::add`]).
    ///
    /// # Errors
    ///
    /// As [`Self::add`].
    pub fn with(mut self, format: Arc<dyn FileFormat>) -> Result<Self, FormatRegistrationError> {
        self.add(format)?;
        Ok(self)
    }

    /// The registered formats, in registration order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &dyn FileFormat> + '_ {
        self.formats.iter().map(|format| &**format)
    }

    /// The format reading `extension` (without the dot, any case).
    #[must_use]
    pub fn for_extension(&self, extension: &str) -> Option<&dyn FileFormat> {
        self.position(extension).map(|at| &*self.formats[at])
    }

    /// The format reading the layer `identifier` names, by the extension of
    /// its path without format arguments; `None` for a built-in format, a
    /// package member, or an extension no format claims.
    #[must_use]
    pub fn for_identifier(&self, identifier: &str) -> Option<&dyn FileFormat> {
        self.index_for(identifier).map(|at| &*self.formats[at])
    }

    pub(crate) fn index_for(&self, identifier: &str) -> Option<usize> {
        if self.formats.is_empty() {
            return None;
        }
        let path = FormatArgs::strip(identifier);
        if path.ends_with(']') {
            return None;
        }
        let leaf = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let (_, extension) = leaf.rsplit_once('.')?;
        self.position(extension)
    }

    pub(crate) fn get(&self, index: usize) -> &dyn FileFormat {
        &*self.formats[index]
    }

    fn position(&self, extension: &str) -> Option<usize> {
        self.formats.iter().position(|format| {
            format
                .extensions()
                .iter()
                .any(|claimed| claimed.eq_ignore_ascii_case(extension))
        })
    }
}

impl PartialEq for FileFormats {
    /// The same formats, by identity, in the same order.
    fn eq(&self, other: &Self) -> bool {
        self.formats.len() == other.formats.len()
            && self
                .formats
                .iter()
                .zip(&other.formats)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

impl Eq for FileFormats {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Named(&'static str, &'static [&'static str]);
    impl FileFormat for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn extensions(&self) -> &[&str] {
            self.1
        }
        fn default_args(&self) -> FormatArgs {
            FormatArgs::new().with("rows", "4")
        }
        fn read(&self, source: &FormatSource<'_>) -> Result<Layer, IoError> {
            Ok(Layer::new(source.layer))
        }
    }

    #[test]
    fn malformed_arguments_reject_the_asset() {
        assert_eq!(
            split_args("a.grid:SDF_FORMAT_ARGS:k").unwrap_err().kind,
            IoErrorKind::Rejected
        );
    }

    #[test]
    fn identity_drops_target_and_defaults() {
        let format = Named("grid", &["grid"]);
        let args = FormatArgs::new()
            .with("target", "usd")
            .with("rows", "4")
            .with("seed", "7");
        let canonical = canonical_args(args, Some(&format));
        assert_eq!(canonical.iter().collect::<Vec<_>>(), [("seed", "7")]);
        let builtin = canonical_args(FormatArgs::new().with("target", "usd"), None);
        assert!(builtin.is_empty());
    }

    #[test]
    fn registration_is_explicit_and_exclusive() {
        let mut formats = FileFormats::default();
        formats
            .add(Arc::new(Named("grid", &["grid", "grd"])))
            .unwrap();
        assert!(matches!(
            formats.add(Arc::new(Named("other", &["grd"]))),
            Err(FormatRegistrationError::DuplicateExtension { registered, .. }) if registered == "grid"
        ));
        assert!(matches!(
            formats.add(Arc::new(Named("text", &["usda"]))),
            Err(FormatRegistrationError::BuiltinExtension { .. })
        ));
        assert!(matches!(
            formats.add(Arc::new(Named("dotted", &[".x"]))),
            Err(FormatRegistrationError::InvalidExtension { .. })
        ));
        assert!(matches!(
            formats.add(Arc::new(Named("none", &[]))),
            Err(FormatRegistrationError::NoExtensions { .. })
        ));
        assert_eq!(formats.iter().len(), 1);
        let name = |id: &str| formats.for_identifier(id).map(FileFormat::name);
        assert_eq!(name("/x/y.GRD:SDF_FORMAT_ARGS:rows=2"), Some("grid"));
        assert_eq!(name("/x/y.usda"), None);
        assert_eq!(name("/x/p.usdz[y.grid]"), None);
        assert_eq!(name("/x.grid/y"), None);
    }
}
