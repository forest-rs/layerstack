// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Package-scoped asset resolver for USDZ archives.
//!
//! Internal asset paths (e.g., sublayer references, texture paths) resolve
//! within the package before delegating to an outer resolver. This
//! implements packaged resource resolution per AOUSD Core §9.7.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::asset::anchor_asset_path;
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
/// A relative asset path authored in a package member names another member,
/// found as OpenUSD's `SdfComputeAssetPathRelativeToLayer` finds it
/// (`pxr/usd/sdf/layerUtils.cpp`):
///
/// - a path relative to its layer (`./asset.usda`, `../asset.usda`, any
///   path starting with `.`) is anchored to the directory of the member
///   that authors it, and nowhere else;
/// - any other relative path (`asset.usda`, `models/asset.usda`) is a search
///   path: it is anchored to the authoring member's directory, then to the
///   directory of the package's root layer, and if neither names a member
///   it goes to the outer resolver as authored.
///
/// An absolute path goes to the outer resolver. A member is identified by
/// its normalized path inside the package, so a member reached by
/// different asset paths (`asset.usda` from the root, `../asset.usda` from
/// `models/`) is the same layer, and loads once.
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
    /// The package's root layer, which stands for the package itself when
    /// a path goes to the outer resolver.
    root: LayerId,
    /// The layer of each member loaded so far, by its path in the package.
    by_member: BTreeMap<Arc<str>, LayerId>,
    /// The path in the package of each member loaded so far.
    member_paths: BTreeMap<LayerId, Arc<str>>,
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
    /// Creates a resolver scoped to `archive`, whose root layer is the
    /// member `root_member`, loaded as `root`. Members it loads get layer
    /// IDs from `outer`.
    pub(crate) fn new(
        archive: &'a ZipArchive<'a>,
        root_member: Arc<str>,
        root: LayerId,
        outer: &'a mut dyn AssetResolver,
    ) -> Self {
        Self {
            archive,
            root,
            by_member: BTreeMap::from([(root_member.clone(), root)]),
            member_paths: BTreeMap::from([(root, root_member)]),
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
                member_paths: self.member_paths,
            }),
        }
    }

    /// Loads the member at `member`, a normalized path in the package, or
    /// returns the layer it was already loaded as; `None` when the package
    /// has no such member.
    fn load_member(
        &mut self,
        member: &str,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Option<Result<ResolvedAsset, AssetResolveError>> {
        if let Some((name, &id)) = self.by_member.get_key_value(member) {
            return Some(Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: name.clone(),
                layer: None,
            }));
        }
        let entry = self.archive.find(member)?;
        let data = self.archive.entry_data(entry);
        let name = entry.name.clone();

        // The outer resolver owns the ID space the members share with the
        // layers it loads.
        let Some(layer_id) = self.outer.allocate_layer_id() else {
            let failure = UsdzError::LayerIdUnavailable {
                member: name.clone(),
            };
            let error = AssetResolveError::LoadError(Arc::from(alloc::format!("{failure}")));
            self.failure.get_or_insert(failure);
            return Some(Err(error));
        };
        // Registered before parsing, so a member that it loads in turn and
        // that references it back finds it.
        self.by_member.insert(name.clone(), layer_id);
        self.member_paths.insert(layer_id, name.clone());

        let parsed = match parse_layer_data(data, &name, layer_id, tokens, paths, self) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Some(Err(AssetResolveError::LoadError(Arc::from(
                    alloc::format!("{e}"),
                ))));
            }
        };

        // The layer goes back to the parser that asked for it; the layers
        // its own parser resolved have no other way back to the caller.
        self.descendants.extend(parsed.resolved_layers);

        Some(Ok(ResolvedAsset {
            layer_id,
            resolved_path: name,
            layer: Some(parsed.layer),
        }))
    }
}

/// What a [`UsdzResolver`] loaded, beyond the layers it handed to parsers.
pub(crate) struct Loaded {
    /// The layers resolved while parsing the layers the resolver loaded,
    /// which the caller must keep.
    pub(crate) descendants: Vec<Layer>,
    /// The path in the package of each member loaded, the root layer's
    /// included.
    pub(crate) member_paths: BTreeMap<LayerId, Arc<str>>,
}

impl AssetResolver for UsdzResolver<'_> {
    /// Resolves `asset_path`, authored in the layer `anchor` (the root layer
    /// when `None`), as described on [`UsdzResolver`].
    ///
    /// Spec: AOUSD Core §9.4 (relative asset paths), §9.7 (packaged
    /// resource resolution). OpenUSD: `SdfComputeAssetPathRelativeToLayer`
    /// in `pxr/usd/sdf/layerUtils.cpp`.
    fn resolve(
        &mut self,
        asset_path: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        // A layer the outer resolver loaded resolves its paths there.
        let Some(authoring) = self.member_paths.get(&anchor.unwrap_or(self.root)).cloned() else {
            return self.outer.resolve(asset_path, anchor, tokens, paths);
        };
        let path = asset_path.replace('\\', "/");
        if is_absolute(&path) {
            return self
                .outer
                .resolve(asset_path, Some(self.root), tokens, paths);
        }

        // OpenUSD takes any path starting with `.` as relative to its layer.
        let layer_relative = path.starts_with('.');
        if let Some(member) = member_path(&path, &authoring)
            && let Some(resolved) = self.load_member(&member, tokens, paths)
        {
            return resolved;
        }
        if layer_relative {
            return Err(AssetResolveError::NotFound);
        }
        let root_member = self.member_paths[&self.root].clone();
        if let Some(member) = member_path(&path, &root_member)
            && let Some(resolved) = self.load_member(&member, tokens, paths)
        {
            return resolved;
        }
        self.outer
            .resolve(asset_path, Some(self.root), tokens, paths)
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.member_paths.get(&id).map(|s| &**s)
    }

    fn allocate_layer_id(&mut self) -> Option<LayerId> {
        self.outer.allocate_layer_id()
    }
}

/// Whether `path` (with `/` separators) is absolute: from the root, or from
/// a drive (`C:/`).
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":/")
}

/// The member `path`, a relative asset path with `/` separators, names when
/// anchored to the directory of the member `anchor`: the two joined and
/// normalized (`models/./a/../asset.usda` is `models/asset.usda`). `None`
/// when it leaves the package (`../asset.usda` authored in a member at the
/// package's top).
///
/// OpenUSD: `_AnchorRelativePath` in `pxr/usd/sdf/layerUtils.cpp`.
fn member_path(path: &str, anchor: &str) -> Option<String> {
    // Both are marked relative to the package's top, so that anchoring
    // joins them even for a search path and a member at the top.
    let anchored = anchor_asset_path(&alloc::format!("./{path}"), &alloc::format!("./{anchor}"))?;
    anchored.strip_prefix("./").map(String::from)
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

    #[test]
    fn member_paths_anchor_to_the_authoring_members_directory() {
        for (path, anchor, member) in [
            ("asset.usda", "root.usda", Some("asset.usda")),
            ("./asset.usda", "root.usda", Some("asset.usda")),
            (
                "./asset.usda",
                "models/parent.usda",
                Some("models/asset.usda"),
            ),
            (
                "asset.usda",
                "models/parent.usda",
                Some("models/asset.usda"),
            ),
            ("../asset.usda", "models/parent.usda", Some("asset.usda")),
            (
                "./sub/../deep/./asset.usda",
                "models/parent.usda",
                Some("models/deep/asset.usda"),
            ),
            ("../../shared/a.usda", "a/b/c.usda", Some("shared/a.usda")),
            ("../asset.usda", "root.usda", None),
            ("sub/../../asset.usda", "root.usda", None),
        ] {
            assert_eq!(
                member_path(path, anchor).as_deref(),
                member,
                "{path} in {anchor}"
            );
        }
    }

    #[test]
    fn a_member_reached_by_different_paths_loads_once() {
        let (result, outside) = read(&[
            (
                "root.usda",
                "#usda 1.0\ndef \"A\" (references = [@shared/asset.usda@</A>, \
                 @models/parent.usda@</P>]) {}\n",
            ),
            (
                "models/parent.usda",
                "#usda 1.0\ndef \"P\" (references = [@../shared/asset.usda@</A>, \
                 @./sub/../../shared/asset.usda@</A>, @..\\shared\\asset.usda@</A>, \
                 @../root.usda@</A>]) {}\n",
            ),
            ("shared/asset.usda", "#usda 1.0\ndef \"A\" {}\n"),
        ]);
        assert_eq!(
            returned(&result),
            ["root.usda", "models/parent.usda", "shared/asset.usda"]
        );
        assert!(outside.asked.is_empty(), "{:?}", outside.asked);
    }

    #[test]
    fn a_layer_relative_path_never_searches() {
        // `./asset.usda` in `models/` is `models/asset.usda`, which is not
        // in the package: neither the root's `asset.usda` nor the outer
        // resolver stands in for it.
        let (result, outside) = read(&[
            (
                "root.usda",
                "#usda 1.0\ndef \"A\" (references = @models/parent.usda@</P>) {}\n",
            ),
            (
                "models/parent.usda",
                "#usda 1.0\ndef \"P\" (references = @./asset.usda@</A>) {}\n",
            ),
            ("asset.usda", "#usda 1.0\ndef \"A\" {}\n"),
        ]);
        assert_eq!(returned(&result), ["root.usda", "models/parent.usda"]);
        assert!(outside.asked.is_empty(), "{:?}", outside.asked);
    }

    #[test]
    fn a_search_path_falls_back_to_the_root_layer_then_the_outer_resolver() {
        let (result, outside) = read(&[
            (
                "scene/root.usda",
                "#usda 1.0\ndef \"A\" (references = @models/parent.usda@</P>) {}\n",
            ),
            (
                "scene/models/parent.usda",
                "#usda 1.0\ndef \"P\" (references = [@common.usda@</C>, \
                 @elsewhere.usda@</E>, @/abs/outside.usda@</O>]) {}\n",
            ),
            ("scene/common.usda", "#usda 1.0\ndef \"C\" {}\n"),
        ]);
        assert_eq!(
            returned(&result),
            [
                "scene/root.usda",
                "scene/common.usda",
                "scene/models/parent.usda"
            ]
        );
        assert_eq!(outside.asked, ["elsewhere.usda", "/abs/outside.usda"]);
    }
}
