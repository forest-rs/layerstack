// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset resolution: mapping asset path URIs to layers.
//!
//! Asset paths appear in references, payloads, and sublayer includes. Before
//! composition can use them, they must be resolved to concrete [`LayerId`]s
//! with their layer data loaded into a [`LayerStore`].
//!
//! The [`AssetResolver`] trait defines this mapping. Implementations handle
//! the full resolution pipeline described in AOUSD Core §9:
//!
//! - Protocol handling (§9.3): interpreting URI schemes
//! - Relative path resolution (§9.4): resolving paths relative to an anchor
//! - Search path resolution (§9.5): searching configured directories
//! - Extension resolution (§9.6): probing `.usda` / `.usdc` / `.usd`
//! - Package resolution (§9.7): locating assets within USDZ archives
//!
//! # Usage pattern
//!
//! ```ignore
//! // 1. Build a resolver (implementation-specific).
//! let mut resolver = MyResolver::new(&["assets/", "shared/"]);
//!
//! // 2. Resolve asset paths — the resolver returns the layer.
//! let resolved = resolver.resolve(
//!     "props/robot.usda",
//!     Some(root_layer),
//!     &mut store.tokens,
//!     &mut store.paths,
//! )?;
//!
//! // 3. Insert the layer into your store (if it's new).
//! if let Some(layer) = resolved.layer {
//!     store.insert_layer(layer);
//! }
//!
//! // 4. Build the reference with the resolved LayerId.
//! let reference = Reference {
//!     layer: resolved.layer_id,
//!     prim_path: target_path,
//!     asset: Some("props/robot.usda".into()),
//! };
//! ```

use alloc::{string::String, sync::Arc, vec::Vec};

use crate::{
    doc::{Layer, LayerId, LayerStore},
    interner::TokenInterner,
    path::PathInterner,
};

/// An asset path a variable expression evaluates to, and the layer that
/// authors the expression, which a relative path is anchored to (see
/// [`expression_asset_paths`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExpressionAssetPath {
    /// The layer that authors the expression.
    pub anchor: LayerId,
    /// The evaluated asset path.
    pub asset_path: String,
}

/// Returns the asset paths that composing the layer stack rooted at `root`
/// evaluates variable expressions to and that `store` does not resolve
/// ([`LayerStore::asset_layer`]), sorted.
///
/// The asset path of a sublayer, reference or payload may be a variable
/// expression (see [`crate::variable_expression`]), evaluated with the
/// expression variables of the layer stack that authors it, so an importer
/// cannot resolve it on its own. A host loads each path this lists,
/// anchored to its layer, records it for [`LayerStore::asset_layer`] (for
/// example [`InMemoryStore::insert_asset_layer`]) and asks again, until
/// nothing new resolves: a newly loaded layer may author expressions of its
/// own.
///
/// Every reference and payload the reachable layers author is visited, in
/// every variant branch, with the variables of each layer stack that
/// reaches it, so this may list paths composition does not follow.
///
/// OpenUSD resolves these paths during composition
/// (`_PcpComposeSiteReferencesOrPayloads` in `pxr/usd/pcp/composeSite.cpp`).
///
/// [`InMemoryStore::insert_asset_layer`]: crate::InMemoryStore::insert_asset_layer
#[must_use]
pub fn expression_asset_paths(store: &dyn LayerStore, root: LayerId) -> Vec<ExpressionAssetPath> {
    crate::expression_variables::walk(store, root).unresolved
}

/// The result of successfully resolving an asset path.
///
/// On the first resolution of a given path, `layer` is `Some` and the caller
/// should insert it into their [`LayerStore`]. On
/// subsequent resolutions of the same path (deduplication), `layer` is `None`.
#[derive(Clone, Debug)]
pub struct ResolvedAsset {
    /// The layer ID assigned to the resolved asset.
    pub layer_id: LayerId,
    /// The canonical resolved path, after search path and extension probing.
    ///
    /// This may differ from the input path (e.g., `"robot.usd"` might resolve
    /// to `"/assets/props/robot.usdc"`).
    pub resolved_path: Arc<str>,
    /// The resolved layer data, or `None` if this was a cache hit.
    ///
    /// When `Some`, the caller must insert this layer into their store before
    /// composition. When `None`, the layer was already returned by a previous
    /// call to [`AssetResolver::resolve`] for the same asset path.
    pub layer: Option<Layer>,
}

/// Reasons an asset path could not be resolved.
///
/// Spec: AOUSD Core §9.2 (asset identifiers), §9.5 (search path failure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetResolveError {
    /// The asset path could not be found after exhausting all search paths
    /// and extension probing.
    NotFound,
    /// The asset was located but could not be loaded (parse error, I/O
    /// failure, unsupported format, etc.).
    LoadError(Arc<str>),
}

/// Resolves asset path strings to layers.
///
/// This trait is the standard interface for integrating external asset sources
/// (filesystems, databases, network services, USDZ packages) with
/// layerstack's composition pipeline. The resolver is responsible for:
///
/// 1. Locating the asset (applying search paths, extension probing, etc.)
/// 2. Loading and parsing the layer data
/// 3. Assigning a [`LayerId`] and returning the [`Layer`]; the resolver
///    owns the ID space, and also allocates IDs for layers loaded on its
///    behalf ([`AssetResolver::allocate_layer_id`])
/// 4. Deduplicating: repeated resolution of the same path returns the same
///    [`LayerId`] with `layer: None`
///
/// The caller is responsible for inserting returned layers into their
/// [`LayerStore`].
///
/// Spec: AOUSD Core §9 (asset resolution).
pub trait AssetResolver {
    /// Resolves an asset path, returning the layer data.
    ///
    /// The resolver uses the provided interners to build paths and tokens
    /// within the resolved layer, ensuring they share the same interning
    /// namespace as the rest of the scene.
    ///
    /// # Parameters
    ///
    /// - `asset_path`: the raw URI string from a reference, payload, or
    ///   sublayer include (e.g., `"props/robot.usda"`, `"./local.usda"`).
    /// - `anchor`: the [`LayerId`] of the layer containing the arc that
    ///   references this asset. Used for relative path resolution (§9.4).
    ///   `None` for top-level / root resolution.
    /// - `tokens`: shared token interner for the scene.
    /// - `paths`: shared path interner for the scene.
    ///
    /// # Returns
    ///
    /// A [`ResolvedAsset`] on success. If `resolved.layer` is `Some`, the
    /// caller must insert it into their store. If `None`, the layer was
    /// already returned by a previous call (deduplication hit).
    fn resolve(
        &mut self,
        asset_path: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError>;

    /// Returns the canonical resolved path for a previously resolved layer.
    ///
    /// Returns `None` if `id` was not produced by this resolver.
    fn resolved_path(&self, id: LayerId) -> Option<&str>;

    /// Allocates a [`LayerId`] for a layer loaded on this resolver's behalf
    /// by someone else: a layer inside a package that
    /// [`AssetResolver::resolve`] never sees, such as a member of a USDZ
    /// package read by `layerstack_usdz::read_usdz`.
    ///
    /// The layers this resolver returns and the layers it allocates IDs for
    /// end up in one store, so the resolver owns the one ID space for both:
    /// an allocated ID is never one it has returned or will return from
    /// [`AssetResolver::resolve`], and never allocated twice.
    ///
    /// `None`, the default, when the resolver does not allocate IDs for
    /// others; a package reader then fails rather than guess at free IDs.
    fn allocate_layer_id(&mut self) -> Option<LayerId> {
        None
    }

    /// Returns `asset_path`, authored in the layer `anchor`, anchored to that
    /// layer: an identifier that names the same asset from any layer.
    /// `None` when it cannot be anchored, because the layer's location is
    /// not known or the path is an expression (`` `...` ``).
    ///
    /// The default anchors a path relative to its layer (`./bark.png`,
    /// `../shared/leaf.png`) against [`AssetResolver::resolved_path`] with
    /// [`anchor_asset_path`]; a search path (`textures/bark.png`) and an
    /// absolute path are normalized and kept, and a URI is kept as
    /// authored. OpenUSD's default resolver anchors a search path too when
    /// the anchored asset exists (`ArDefaultResolver::_CreateIdentifier`); a
    /// resolver that can look overrides this to do the same.
    ///
    /// Spec: AOUSD Core §9.4 (relative asset paths).
    fn anchor_asset_path(&self, asset_path: &str, anchor: LayerId) -> Option<String> {
        if asset_path.starts_with('`') {
            return None;
        }
        if !is_file_relative(&asset_path.replace('\\', "/")) {
            return anchor_asset_path(asset_path, "");
        }
        anchor_asset_path(asset_path, self.resolved_path(anchor)?)
    }
}

/// Whether `asset_path` is relative to the layer that authors it or to a
/// search path: not empty, not from the root (`/`), and not a URI or a
/// drive (`https:`, `C:`).
fn is_relative(asset_path: &str) -> bool {
    if asset_path.is_empty() || asset_path.starts_with('/') {
        return false;
    }
    !asset_path.split_once(':').is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// Whether `asset_path` is relative to the layer that authors it: `./` or
/// `../` (OpenUSD's `_IsFileRelative` in `pxr/usd/ar/defaultResolver.cpp`).
/// Any other relative path is a search path.
fn is_file_relative(asset_path: &str) -> bool {
    asset_path.starts_with("./") || asset_path.starts_with("../")
}

/// Normalizes the asset path `path` as OpenUSD identifies an asset it has
/// not anchored (`TfNormPath`): empty and `.` segments are dropped and each
/// `..` removes the segment before it. A path relative to its layer (`./`
/// or `../`) stays marked as such, since it anchors to the layer; a search
/// path (`granite.usda`, `sub/../granite.usda`) does not; an absolute path
/// stays absolute, from `/` or from a drive root (`C:/`). A URI and an
/// expression are kept as authored.
///
/// Composition compares unresolved reference and payload arcs by this
/// identity, and flattening writes search and absolute paths with it
/// ([`AssetResolver::anchor_asset_path`]), so both agree.
///
/// OpenUSD: `_IsFileRelative`, `_IsSearchPath` and `_CreateIdentifier` in
/// `pxr/usd/ar/defaultResolver.cpp`.
#[must_use]
pub fn normalize_asset_path(path: &str) -> String {
    if let Some((drive, rest)) = drive_root(path) {
        return alloc::format!("{drive}{}", normalize_asset_path(rest));
    }
    let absolute = path.starts_with('/');
    if !absolute && (!is_relative(path) || path.starts_with('`')) {
        return String::from(path);
    }
    let file_relative = is_file_relative(path);
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." if segments.last().is_some_and(|last| *last != "..") => {
                segments.pop();
            }
            ".." if absolute => {}
            other => segments.push(other),
        }
    }
    let joined = segments.join("/");
    if absolute {
        alloc::format!("/{joined}")
    } else if file_relative && segments.first() != Some(&"..") {
        alloc::format!("./{joined}")
    } else {
        joined
    }
}

/// Splits a Windows drive root off `path`: `C:/a` is `("C:", "/a")`.
fn drive_root(path: &str) -> Option<(&str, &str)> {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/')
        .then(|| path.split_at(2))
}

/// Anchors `asset_path` to the layer at `layer_location` (its resolved
/// path), as `ArDefaultResolver::_CreateIdentifier` anchors a path it
/// finds: a path relative to the layer (`./bark.png`, `../leaf.png`) is
/// joined to the layer's directory and normalized
/// ([`normalize_asset_path`]); any other path is normalized and kept, a
/// search path staying a search path. `None` for an expression
/// (`` `...` ``), or for a layer relative path in a layer inside a package
/// (`a.usdz[b.usda]`) or of unknown location, which only a package-aware
/// resolver can anchor.
///
/// The location and the path may use `\` separators and a drive root
/// (`C:\assets\oak.usda`); the anchored path uses `/`, as OpenUSD's
/// `TfNormPath` writes it on Windows.
///
/// Spec: AOUSD Core §9.4 (relative asset paths).
#[must_use]
pub fn anchor_asset_path(asset_path: &str, layer_location: &str) -> Option<String> {
    if asset_path.starts_with('`') {
        return None;
    }
    let asset_path = asset_path.replace('\\', "/");
    if !is_file_relative(&asset_path) {
        return Some(normalize_asset_path(&asset_path));
    }
    if layer_location.contains('[') || layer_location.is_empty() {
        return None;
    }
    let location = layer_location.replace('\\', "/");
    let directory = location.rfind('/').map_or("", |end| &location[..end]);
    Some(normalize_asset_path(&alloc::format!(
        "{directory}/{asset_path}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_paths_normalize_as_openusd_identifies_them() {
        for (path, normalized) in [
            ("./granite.usda", "./granite.usda"),
            ("./deep/../granite.usda", "./granite.usda"),
            ("../rock/./granite.usda", "../rock/granite.usda"),
            ("./../granite.usda", "../granite.usda"),
            ("granite.usda", "granite.usda"),
            ("deep/../granite.usda", "granite.usda"),
            ("/assets/./deep/../granite.usda", "/assets/granite.usda"),
            (
                "https://example.com/a/../b.usda",
                "https://example.com/a/../b.usda",
            ),
            ("", ""),
            ("C:/assets/./deep/../granite.usda", "C:/assets/granite.usda"),
        ] {
            assert_eq!(normalize_asset_path(path), normalized, "{path}");
        }
    }

    #[test]
    fn layer_relative_paths_are_anchored_to_their_layer() {
        let layer = "/assets/trees/oak.usda";
        for (path, anchored) in [
            ("./bark.png", "/assets/trees/bark.png"),
            ("../shared//leaf.png", "/assets/shared/leaf.png"),
            ("textures/./bark.png", "textures/bark.png"),
            ("/abs/./bark.png", "/abs/bark.png"),
            (
                "https://example.com/bark.png",
                "https://example.com/bark.png",
            ),
            ("C:/bark.png", "C:/bark.png"),
            ("", ""),
        ] {
            assert_eq!(
                anchor_asset_path(path, layer).as_deref(),
                Some(anchored),
                "{path}"
            );
        }
        // A Windows location: `\` separators and a drive root.
        let windows = "C:\\assets\\trees\\oak.usda";
        for (path, anchored) in [
            ("./bark.png", "C:/assets/trees/bark.png"),
            ("../../../shared/leaf.png", "C:/shared/leaf.png"),
            (".\\textures\\bark.png", "C:/assets/trees/textures/bark.png"),
        ] {
            assert_eq!(
                anchor_asset_path(path, windows).as_deref(),
                Some(anchored),
                "{path}"
            );
        }
        assert_eq!(
            anchor_asset_path("./bark.png", "C:\\assets/mixed\\oak.usda").as_deref(),
            Some("C:/assets/mixed/bark.png")
        );
        assert_eq!(anchor_asset_path("`${X}.png`", layer), None);
        assert_eq!(anchor_asset_path("./a.png", "/p/a.usdz[b.usda]"), None);
    }

    #[test]
    fn the_default_keeps_search_paths() {
        struct Located;
        impl AssetResolver for Located {
            fn resolve(
                &mut self,
                _: &str,
                _: Option<LayerId>,
                _: &mut TokenInterner,
                _: &mut PathInterner,
            ) -> Result<ResolvedAsset, AssetResolveError> {
                Err(AssetResolveError::NotFound)
            }
            fn resolved_path(&self, id: LayerId) -> Option<&str> {
                (id == LayerId(1)).then_some("/assets/oak.usda")
            }
        }
        let anchor = |path| Located.anchor_asset_path(path, LayerId(1));
        assert_eq!(anchor("./bark.png").as_deref(), Some("/assets/bark.png"));
        assert_eq!(
            anchor("textures/../textures/bark.png").as_deref(),
            Some("textures/bark.png")
        );
        assert_eq!(anchor("/abs.png").as_deref(), Some("/abs.png"));
        assert_eq!(Located.anchor_asset_path("./bark.png", LayerId(2)), None);
    }
}
