// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Path arithmetic on sites: prim paths with variant selections.
//!
//! Variant selections are not namespace: map functions never see them, so
//! a site's path is mapped with its selections stripped and they are put
//! back where they belong. OpenUSD: the `SdfPath` operations of
//! `pxr/usd/pcp/primIndex.cpp` on prim variant selection paths.

use crate::{
    path::{PathId, PathInterner},
    spec_path::{SpecPath, VariantSelectionSite},
};

/// The number of path elements of `site`, its variant selections counted:
/// `/A{v=x}B` has three. OpenUSD: `SdfPath::GetPathElementCount`.
pub(crate) fn element_count(paths: &PathInterner, site: &SpecPath) -> usize {
    paths.resolve(site.prim_path()).depth() + site.selection_entries().len()
}

/// Whether `site` ends in a variant selection (`/A{v=x}`).
/// OpenUSD: `SdfPath::IsPrimVariantSelectionPath`.
pub(crate) fn is_variant_selection_path(site: &SpecPath) -> bool {
    site.trailing_selection().is_some()
}

/// The parent of `site`: without its trailing variant selection, or
/// without its last prim name. `None` for the root.
/// OpenUSD: `SdfPath::GetParentPath`.
pub(crate) fn parent(paths: &PathInterner, site: &SpecPath) -> Option<SpecPath> {
    let prim = site.prim_path();
    let depth = paths.resolve(prim).depth();
    let entries = site.selection_entries();
    if let Some((host_depth, _)) = entries.last()
        && *host_depth as usize == depth
    {
        let kept = &entries[..entries.len() - 1];
        let sites: alloc::vec::Vec<VariantSelectionSite> =
            kept.iter().map(|(_, site)| *site).collect();
        return Some(SpecPath::from_variant_selection_sites(prim, &sites, paths));
    }
    if depth == 0 {
        return None;
    }
    let parent = paths
        .parent(prim)
        .expect("a prim below the root has a parent");
    Some(site.truncated_to(parent, depth - 1))
}

/// `site` with the variant selection `{set=variant}` appended after its
/// last element. OpenUSD: `SdfPath::AppendVariantSelection`.
pub(crate) fn append_selection(
    paths: &PathInterner,
    site: &SpecPath,
    set: crate::interner::TokenId,
    variant: crate::interner::TokenId,
) -> SpecPath {
    let prim = site.prim_path();
    let depth = u32::try_from(paths.resolve(prim).depth()).expect("namespace depth");
    site.with_trailing_selection(
        VariantSelectionSite {
            host_path: prim,
            set,
            variant,
        },
        depth,
    )
}

/// `site`, whose stripped path has the stripped path of `prefix` as a
/// prefix, with `prefix`'s variant selections in place of its own up to
/// that depth. OpenUSD: `path.ReplacePrefix(prefix.StripAllVariantSelections(),
/// prefix)`.
pub(crate) fn apply_selections_of(
    paths: &PathInterner,
    path: PathId,
    prefix: &SpecPath,
) -> SpecPath {
    if !prefix.has_variant_selections() {
        return SpecPath::from_prim_path(path);
    }
    let prefix_prim = paths.resolve(prefix.prim_path());
    if !paths
        .resolve(path)
        .segments()
        .starts_with(prefix_prim.segments())
    {
        return SpecPath::from_prim_path(path);
    }
    SpecPath::with_prim_and_selections(path, prefix)
}

/// `site` with its prefix `from` (a spec path) replaced by `to`, keeping the
/// rest of its prim names and variant selections.
/// OpenUSD: `SdfPath::ReplacePrefix` on variant-qualified paths.
pub(crate) fn replace_prefix(
    paths: &PathInterner,
    site: &SpecPath,
    from: &SpecPath,
    to: &SpecPath,
) -> SpecPath {
    let from_depth = paths.resolve(from.prim_path()).depth();
    let rest = &paths.resolve(site.prim_path()).segments()[from_depth..];
    let prim = paths.intern_joined(to.prim_path(), rest);
    replace_prefix_to(paths, site, from, from_depth, to, prim)
}

/// [`replace_prefix`] with the prim path of the result, `prim`, known, and
/// the namespace depth of `from`'s, `from_depth`.
pub(crate) fn replace_prefix_to(
    paths: &PathInterner,
    site: &SpecPath,
    from: &SpecPath,
    from_depth: usize,
    to: &SpecPath,
    prim: PathId,
) -> SpecPath {
    // The selections of `to`, then those of `site` beyond `from`'s, their
    // hosts moved beneath `to`.
    let skip = from.selection_entries().len();
    if site.selection_entries().len() <= skip {
        // None beyond `from`'s: the selections of `to`, as they are.
        return SpecPath::with_prim_and_selections(prim, to);
    }
    let mut sites: alloc::vec::Vec<VariantSelectionSite> = to.variant_selection_sites().collect();
    for (host_depth, selection) in
        &site.selection_entries()[skip.min(site.selection_entries().len())..]
    {
        let host_rest: crate::path::Segments = paths.resolve(selection.host_path).segments()
            [from_depth.min(*host_depth as usize)..]
            .into();
        let host = paths.intern_joined(to.prim_path(), &host_rest);
        sites.push(VariantSelectionSite {
            host_path: host,
            ..*selection
        });
    }
    SpecPath::from_variant_selection_sites(prim, &sites, paths)
}

/// The prim path of `site` without its trailing variant selections,
/// keeping enclosing ones: `/A{v=x}B{w=y}` becomes `/A{v=x}B`.
/// OpenUSD: `SdfPath::GetPrimPath`.
pub(crate) fn without_trailing_selections(paths: &PathInterner, site: &SpecPath) -> SpecPath {
    let depth = paths.resolve(site.prim_path()).depth();
    if depth == 0 {
        return site.clone();
    }
    site.truncated_to(site.prim_path(), depth - 1)
}
