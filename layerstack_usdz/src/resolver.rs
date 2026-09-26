// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Package-scoped asset resolver for USDZ archives.
//!
//! Internal asset paths (e.g., sublayer references, texture paths) resolve
//! within the package before delegating to an outer resolver. This
//! implements packaged resource resolution per AOUSD Core §9.7.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::doc::{Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};

use crate::error::UsdzError;
use crate::zip::ZipArchive;

/// USDC magic bytes used for format sniffing.
const USDC_MAGIC: &[u8; 8] = b"PXR-USDC";

/// Resolves asset paths within a USDZ package.
///
/// Paths that match archive entries are loaded from the package; paths
/// that don't match are delegated to the outer resolver.
///
/// Every layer it loads reaches the caller of [`read_usdz`] exactly once. A
/// layer that [`AssetResolver::resolve`] loads goes back to the parser that
/// asked for it, which keeps it among its own resolved layers. The layers
/// that layer's own parser resolved in turn come back to this resolver,
/// which keeps them in `descendants` until [`read_usdz`] returns them
/// alongside the ones the root layer resolved.
///
/// [`read_usdz`]: crate::read_usdz
pub(crate) struct UsdzResolver<'a> {
    archive: &'a ZipArchive<'a>,
    by_name: BTreeMap<Arc<str>, LayerId>,
    layer_names: BTreeMap<LayerId, Arc<str>>,
    /// The layers resolved while parsing a layer this resolver loaded.
    descendants: Vec<Layer>,
    /// Why the package cannot be read, which a failed resolution cannot
    /// report itself: parsers keep an unresolvable arc and carry on.
    failure: Option<UsdzError>,
    /// Resolves paths outside the package, and allocates the layer IDs of
    /// members, so that they share one ID space with the layers it loads.
    outer: &'a mut dyn AssetResolver,
}

impl<'a> UsdzResolver<'a> {
    /// Creates a new resolver scoped to the given archive.
    pub(crate) fn new(archive: &'a ZipArchive<'a>, outer: &'a mut dyn AssetResolver) -> Self {
        Self {
            archive,
            by_name: BTreeMap::new(),
            layer_names: BTreeMap::new(),
            descendants: Vec::new(),
            failure: None,
            outer,
        }
    }

    /// Returns what this resolver loaded, or why the package cannot be
    /// read.
    pub(crate) fn finish(self) -> Result<Loaded, UsdzError> {
        match self.failure {
            Some(failure) => Err(failure),
            None => Ok(Loaded {
                descendants: self.descendants,
                member_paths: self.layer_names,
            }),
        }
    }
}

/// What a [`UsdzResolver`] loaded, beyond the layers it handed to parsers.
pub(crate) struct Loaded {
    /// The layers resolved while parsing the layers the resolver loaded,
    /// which the caller must keep.
    pub(crate) descendants: Vec<Layer>,
    /// The path in the package of each member the resolver loaded.
    pub(crate) member_paths: BTreeMap<LayerId, Arc<str>>,
}

impl AssetResolver for UsdzResolver<'_> {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        // Normalize: strip leading "./"
        let normalized = asset_path.trim_start_matches("./");

        // Deduplication check.
        if let Some(&id) = self.by_name.get(normalized) {
            return Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: Arc::from(normalized),
                layer: None,
            });
        }

        // Look up in archive.
        let Some(entry) = self.archive.find(normalized) else {
            // Not in package — delegate to outer resolver.
            return self.outer.resolve(asset_path, _anchor, tokens, paths);
        };

        // The outer resolver owns the ID space the members share with the
        // layers it loads.
        let name: Arc<str> = Arc::from(normalized);
        let Some(layer_id) = self.outer.allocate_layer_id() else {
            let failure = UsdzError::LayerIdUnavailable {
                member: name.clone(),
            };
            let error = AssetResolveError::LoadError(Arc::from(alloc::format!("{failure}")));
            self.failure.get_or_insert(failure);
            return Err(error);
        };
        self.by_name.insert(name.clone(), layer_id);
        self.layer_names.insert(layer_id, name.clone());

        // Get entry data.
        let data = self.archive.entry_data(entry);

        // Format dispatch based on extension + magic.
        let parsed = parse_layer_data(data, normalized, layer_id, tokens, paths, self)
            .map_err(|e| AssetResolveError::LoadError(Arc::from(alloc::format!("{e}"))))?;

        // The layer goes back to the parser that asked for it; the layers
        // its own parser resolved have no other way back to the caller.
        self.descendants.extend(parsed.resolved_layers);

        Ok(ResolvedAsset {
            layer_id,
            resolved_path: name,
            layer: Some(parsed.layer),
        })
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.layer_names.get(&id).map(|s| &**s)
    }

    fn allocate_layer_id(&mut self) -> Option<LayerId> {
        self.outer.allocate_layer_id()
    }
}

/// A layer parsed from a package member.
pub(crate) struct ParsedLayer {
    /// The assembled layer.
    pub(crate) layer: Layer,
    /// The layers its parser was handed as [`ResolvedAsset::layer`] while
    /// resolving its sublayers, references and payloads. Nothing else keeps
    /// them: the caller must.
    pub(crate) resolved_layers: Vec<Layer>,
}

/// Parses a USD layer from raw bytes, dispatching by extension and magic.
pub(crate) fn parse_layer_data(
    data: &[u8],
    name: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<ParsedLayer, UsdzError> {
    let ext = name.rsplit('.').next().unwrap_or("");

    match ext {
        "usdc" => parse_usdc(data, name, layer_id, tokens, paths, resolver),
        "usda" => parse_usda(data, name, layer_id, tokens, paths, resolver),
        "usd" => {
            // Probe magic bytes to determine format.
            if data.len() >= 8 && &data[..8] == USDC_MAGIC {
                parse_usdc(data, name, layer_id, tokens, paths, resolver)
            } else {
                parse_usda(data, name, layer_id, tokens, paths, resolver)
            }
        }
        _ => Err(UsdzError::LayerParseError {
            message: Arc::from(alloc::format!("unsupported file type: {name}")),
        }),
    }
}

/// Parses a USDC binary layer.
fn parse_usdc(
    data: &[u8],
    name: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<ParsedLayer, UsdzError> {
    let result =
        layerstack_usdc::read_usdc(data, layer_id, tokens, paths, resolver).map_err(|e| {
            UsdzError::LayerParseError {
                message: Arc::from(alloc::format!("USDC parse error in {name}: {e}")),
            }
        })?;
    Ok(ParsedLayer {
        layer: result.layer,
        resolved_layers: result.resolved_layers,
    })
}

/// Parses a USDA text layer.
fn parse_usda(
    data: &[u8],
    name: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<ParsedLayer, UsdzError> {
    let source = core::str::from_utf8(data).map_err(|_| UsdzError::LayerParseError {
        message: Arc::from(alloc::format!("USDA file {name} is not valid UTF-8")),
    })?;

    let cst = layerstack_usda::parser::parse_cst(source);
    let ast_result = layerstack_usda::lower::lower(&cst.tree, source);
    let emit_result =
        layerstack_usda::emit::emit(&ast_result.layer, layer_id, tokens, paths, resolver);
    Ok(ParsedLayer {
        layer: emit_result.layer,
        resolved_layers: emit_result.resolved_layers,
    })
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;

    use super::*;
    use crate::{PackageFile, UsdzResult, read_usdz, write_usdz};

    /// An outer resolver that finds the empty layers named in `files`, and
    /// records what it was asked. It hands out the IDs from `next_id` on,
    /// both for the layers it finds and for package members; with
    /// `next_id` at `None` it allocates none for members, and with
    /// `reuse` it hands out the ID of every layer it finds twice.
    struct Outside {
        files: &'static [&'static str],
        next_id: Option<u64>,
        reuse: bool,
        found: BTreeMap<String, LayerId>,
        asked: Vec<String>,
    }

    impl Outside {
        fn new(files: &'static [&'static str]) -> Self {
            Self {
                files,
                // The package's root layer is `LayerId(1)`.
                next_id: Some(2),
                reuse: false,
                found: BTreeMap::new(),
                asked: Vec::new(),
            }
        }

        fn next(&mut self) -> Option<LayerId> {
            let id = self.next_id?;
            self.next_id = Some(id + 1);
            Some(LayerId(id))
        }
    }

    impl AssetResolver for Outside {
        fn resolve(
            &mut self,
            asset_path: &str,
            _anchor: Option<LayerId>,
            _tokens: &mut TokenInterner,
            _paths: &mut PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            self.asked.push(String::from(asset_path));
            if !self.files.contains(&asset_path) {
                return Err(AssetResolveError::NotFound);
            }
            if let Some(&layer_id) = self.found.get(asset_path) {
                return Ok(ResolvedAsset {
                    layer_id,
                    resolved_path: Arc::from(asset_path),
                    layer: None,
                });
            }
            let layer_id = self.next().unwrap_or(LayerId(2));
            if self.reuse {
                self.next_id = Some(layer_id.0);
            }
            self.found.insert(String::from(asset_path), layer_id);
            Ok(ResolvedAsset {
                layer_id,
                resolved_path: Arc::from(asset_path),
                layer: Some(Layer::new(layer_id)),
            })
        }

        fn resolved_path(&self, id: LayerId) -> Option<&str> {
            self.found
                .iter()
                .find(|(_, found)| **found == id)
                .map(|(path, _)| path.as_str())
        }

        fn allocate_layer_id(&mut self) -> Option<LayerId> {
            self.next()
        }
    }

    fn try_read(files: &[(&str, &str)], outside: &mut Outside) -> Result<UsdzResult, UsdzError> {
        let files: Vec<PackageFile<'_>> = files
            .iter()
            .map(|(path, text)| PackageFile::new(path, text.as_bytes()))
            .collect();
        let package = write_usdz(&files).expect("package");
        read_usdz(
            &package,
            LayerId(1),
            &mut TokenInterner::default(),
            &mut PathInterner::default(),
            outside,
        )
    }

    fn read(files: &[(&str, &str)]) -> (UsdzResult, Outside) {
        let mut outside = Outside::new(&[]);
        let result = try_read(files, &mut outside).expect("read");
        (result, outside)
    }

    /// The layer IDs of `result`, the root's first, which must all differ.
    fn layer_ids(result: &UsdzResult) -> Vec<LayerId> {
        let mut ids = vec![result.layer.id];
        ids.extend(result.resolved_layers.iter().map(|layer| layer.id));
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "layer IDs repeat: {ids:?}");
        ids
    }

    const MEMBER_AND_OUTSIDE: &[(&str, &str)] = &[
        (
            "root.usda",
            "#usda 1.0\ndef \"P\" (references = [@member.usda@</M>, \
             @outside.usda@</O>]) {}\n",
        ),
        (
            "member.usda",
            "#usda 1.0\ndef \"M\" (references = @further.usda@</F>) {}\n",
        ),
    ];

    #[test]
    fn members_and_outside_layers_share_one_id_space() {
        let mut outside = Outside::new(&["outside.usda", "further.usda"]);
        let result = try_read(MEMBER_AND_OUTSIDE, &mut outside).expect("read");
        let mut ids = layer_ids(&result);
        ids.sort_unstable();
        assert_eq!(ids, [LayerId(1), LayerId(2), LayerId(3), LayerId(4)]);
        assert_eq!(result.member_paths.len(), 2);
        assert_eq!(outside.asked, ["further.usda", "outside.usda"]);
    }

    #[test]
    fn a_resolver_that_allocates_no_ids_fails_the_read() {
        let mut outside = Outside::new(&[]);
        outside.next_id = None;
        assert_eq!(
            try_read(MEMBER_AND_OUTSIDE, &mut outside).unwrap_err(),
            UsdzError::LayerIdUnavailable {
                member: Arc::from("member.usda")
            }
        );
        // A package that loads no other member needs no IDs.
        let mut outside = Outside::new(&[]);
        outside.next_id = None;
        let root_only = [("root.usda", "#usda 1.0\ndef \"P\" {}\n")];
        assert!(try_read(&root_only, &mut outside).is_ok());
    }

    #[test]
    fn an_id_handed_out_twice_fails_the_read() {
        // The layer the resolver finds and the member it then allocates for
        // share an ID.
        let mut outside = Outside::new(&["outside.usda"]);
        outside.reuse = true;
        let files = [
            (
                "root.usda",
                "#usda 1.0\ndef \"P\" (references = [@outside.usda@</O>, \
                 @member.usda@</M>]) {}\n",
            ),
            ("member.usda", "#usda 1.0\ndef \"M\" {}\n"),
        ];
        assert_eq!(
            try_read(&files, &mut outside).unwrap_err(),
            UsdzError::DuplicateLayerId { id: LayerId(2) }
        );
    }

    /// The package path of every layer returned, the root's first and the
    /// rest sorted, after checking that each is returned once.
    fn returned(result: &UsdzResult) -> Vec<&str> {
        let mut ids: Vec<LayerId> = result.resolved_layers.iter().map(|l| l.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "a layer returned twice");
        assert!(!ids.contains(&result.layer.id), "root layer returned again");
        let mut paths = vec![&*result.member_paths[&result.layer.id]];
        let mut others: Vec<&str> = ids.iter().map(|id| &*result.member_paths[id]).collect();
        others.sort_unstable();
        paths.extend(others);
        assert_eq!(paths.len(), result.member_paths.len(), "unreturned member");
        paths
    }

    #[test]
    fn layers_loaded_beneath_members_are_returned_once() {
        let (result, outside) = read(&[
            (
                "root.usda",
                "#usda 1.0\n(\n subLayers = [@over.usda@]\n)\n\
                 def \"A\" (references = @a.usda@</A>) {}\n\
                 def \"B\" (payload = @a.usda@</A>) {}\n",
            ),
            (
                "over.usda",
                "#usda 1.0\ndef \"C\" (references = @c.usda@</C>) {}\n",
            ),
            (
                "a.usda",
                "#usda 1.0\ndef \"A\" (references = [@b.usda@</B>, @c.usda@</C>]) {}\n",
            ),
            (
                "b.usda",
                "#usda 1.0\n(\n subLayers = [@c.usda@]\n)\n\
                 def \"B\" (references = @a.usda@</Z>) {}\n",
            ),
            ("c.usda", "#usda 1.0\ndef \"C\" {}\n"),
        ]);
        assert_eq!(
            returned(&result),
            ["root.usda", "a.usda", "b.usda", "c.usda", "over.usda"]
        );
        assert!(outside.asked.is_empty(), "{:?}", outside.asked);
    }
}
