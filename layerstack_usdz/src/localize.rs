// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dependency-aware asset localization with explicit host resolution.
//!
//! [`localize_asset`] builds inspectable copied layers and asset entries before
//! any output is written. The host supplies loaded layers, asset bytes and
//! package member names; this module owns graph traversal, collision checking,
//! relative-path rewriting and deterministic packaging. Patterns and variable
//! expressions are resolved explicitly by the host, without filesystem policy.
//!
//! Spec: AOUSD Core §9.4 (authoring-layer anchoring), §9.7 (packages), §16.4
//! (USDZ). OpenUSD: `UsdUtilsLocalizeAsset`, `UsdUtilsCreateNewUsdzPackage`.

use crate::{PackageFile, UsdzWriteError, write_usdz};
use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    string::String,
    sync::Arc,
    vec,
    vec::Vec,
};
use core::fmt;
use layerstack::asset_dependencies::{AssetDependency, rewrite_layer_assets};
use layerstack::{Layer, LayerId, LayerStore, PathInterner, TokenInterner};

/// A resource returned by a host after resolving an authored asset use.
#[derive(Clone, Debug)]
pub enum LocalizationResource {
    /// Loaded layer to rewrite and serialize, including anonymous layers.
    Layer {
        /// Source layer ID in the supplied store.
        source: LayerId,
        /// Destination path relative to the localization root.
        member: Arc<str>,
    },
    /// Non-layer asset, copied verbatim.
    Asset {
        /// Destination path relative to the localization root.
        member: Arc<str>,
        /// Complete immutable asset bytes.
        data: Arc<[u8]>,
    },
}

/// A host resolution, including expanded pattern files when applicable.
#[derive(Clone, Debug)]
pub struct LocalizationTarget {
    /// Root-relative identifier to author (a member or a valid host pattern).
    pub member: Arc<str>,
    /// Concrete resources required by that identifier.
    pub resources: Vec<LocalizationResource>,
}
impl LocalizationTarget {
    /// Resolves a use to one loaded layer.
    #[must_use]
    pub fn layer(source: LayerId, member: impl Into<Arc<str>>) -> Self {
        let member = member.into();
        Self {
            member: member.clone(),
            resources: vec![LocalizationResource::Layer { source, member }],
        }
    }
    /// Resolves a use to one asset's bytes.
    #[must_use]
    pub fn asset(member: impl Into<Arc<str>>, data: impl Into<Arc<[u8]>>) -> Self {
        let member = member.into();
        Self {
            member: member.clone(),
            resources: vec![LocalizationResource::Asset {
                member,
                data: data.into(),
            }],
        }
    }
}

/// Copied scene description and its destination member.
#[derive(Clone, Debug)]
pub struct LocalizedLayer {
    /// Original source ID, also retained by the copied layer.
    pub source: LayerId,
    /// Root-relative destination path.
    pub member: Arc<str>,
    /// Copied authored layer with localized relative asset paths.
    pub layer: Layer,
}
/// Copied non-layer asset.
#[derive(Clone, Debug)]
pub struct LocalizedAsset {
    /// Root-relative destination path.
    pub member: Arc<str>,
    /// Complete immutable bytes.
    pub data: Arc<[u8]>,
}
/// A complete inspectable localization, whose source layers remain unchanged.
#[derive(Clone, Debug)]
pub struct LocalizationPlan {
    /// Root first, then other layers in lexical member order.
    pub layers: Vec<LocalizedLayer>,
    /// Non-layer assets in lexical member order.
    pub assets: Vec<LocalizedAsset>,
    /// Number of distinct authored asset uses resolved by the host.
    pub resolved_uses: usize,
}

/// Why localization could not produce a complete output.
#[derive(Debug)]
pub enum LocalizationError {
    /// A source layer is absent from the store.
    MissingLayer {
        /// Missing source ID.
        source: LayerId,
    },
    /// Destination is absolute, empty, or has unsafe/ambiguous segments.
    InvalidMember {
        /// Rejected path.
        member: Arc<str>,
    },
    /// Distinct resources were assigned the same destination path.
    MemberCollision {
        /// Conflicting destination.
        member: Arc<str>,
    },
    /// One layer was assigned different member names by the host.
    LayerCollision {
        /// Source ID needing one canonical destination.
        source: LayerId,
    },
    /// The host could not resolve an authored dependency.
    Resolution {
        /// Exact authoring source and identifier.
        dependency: alloc::boxed::Box<AssetDependency>,
        /// Host explanation, suitable for diagnosis.
        reason: Arc<str>,
    },
    /// Replacement names no supplied file, or supplies no resources.
    MissingReplacement {
        /// Unrepresented destination identifier.
        member: Arc<str>,
    },
    /// Destination layer extension has no serializer here.
    UnsupportedLayerFormat {
        /// Rejected member.
        member: Arc<str>,
    },
    /// USDA lowering/serialization rejected a copied layer.
    Usda(layerstack_usda::save::SaveError),
    /// USDC lowering/serialization rejected a copied layer.
    Usdc(layerstack_usdc::writer::UsdcWriteError),
    /// USDZ layout/member validation failed.
    Package(UsdzWriteError),
}
impl fmt::Display for LocalizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLayer { source } => {
                write!(f, "localization source layer {source:?} is missing")
            }
            Self::InvalidMember { member } => write!(f, "invalid localization member {member:?}"),
            Self::MemberCollision { member } => write!(
                f,
                "localization member {member:?} has conflicting resources"
            ),
            Self::LayerCollision { source } => {
                write!(f, "layer {source:?} has multiple localization destinations")
            }
            Self::Resolution { dependency, reason } => write!(
                f,
                "cannot localize {:?} from layer {:?}: {reason}",
                dependency.identifier, dependency.layer
            ),
            Self::MissingReplacement { member } => write!(
                f,
                "replacement {member:?} names no supplied localization resource"
            ),
            Self::UnsupportedLayerFormat { member } => {
                write!(f, "no layer serializer for {member:?}")
            }
            Self::Usda(error) => error.fmt(f),
            Self::Usdc(error) => error.fmt(f),
            Self::Package(error) => error.fmt(f),
        }
    }
}
impl core::error::Error for LocalizationError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Usda(e) => Some(e),
            Self::Usdc(e) => Some(e),
            Self::Package(e) => Some(e),
            _ => None,
        }
    }
}

/// Builds a complete localization without mutating source layers or writing I/O.
/// `root_member` and host-returned member paths are relative to one destination
/// root. Layer references are rewritten relative to each copied layer's member.
/// Every variant and authored asset use is visited, even when unselected.
///
/// For a pattern, the host supplies its destination pattern and all concrete
/// files. Pattern expansion correctness is host-owned; missing expansion is an
/// error. No partial plan is returned after a resolution or naming failure.
pub fn localize_asset(
    store: &dyn LayerStore,
    root: LayerId,
    root_member: impl Into<Arc<str>>,
    mut resolve: impl FnMut(&AssetDependency) -> Result<LocalizationTarget, LocalizationError>,
) -> Result<LocalizationPlan, LocalizationError> {
    let root_member = root_member.into();
    valid_member(&root_member)?;
    valid_layer_member(&root_member)?;
    let mut names = BTreeMap::from([(root, root_member.clone())]);
    let mut occupied = BTreeMap::from([(root_member.clone(), Some(root))]);
    let mut assets = BTreeMap::<Arc<str>, Arc<[u8]>>::new();
    let mut queue = VecDeque::from([root]);
    let mut visited = BTreeSet::new();
    let mut layers = Vec::new();
    let mut resolved_uses = 0;
    while let Some(source) = queue.pop_front() {
        if !visited.insert(source) {
            continue;
        }
        let layer = store
            .layer(source)
            .ok_or(LocalizationError::MissingLayer { source })?;
        let member = names[&source].clone();
        let copy = rewrite_layer_assets(layer, store.paths(), store.tokens(), |dependency| {
            let target = resolve(dependency)?;
            valid_member(&target.member)?;
            if target.resources.is_empty()
                || (!is_pattern(&target.member)
                    && !target
                        .resources
                        .iter()
                        .any(|r| resource_member(r) == &target.member))
            {
                return Err(LocalizationError::MissingReplacement {
                    member: target.member,
                });
            }
            for resource in target.resources {
                match resource {
                    LocalizationResource::Layer { source, member } => {
                        valid_member(&member)?;
                        valid_layer_member(&member)?;
                        if names.get(&source).is_some_and(|name| name != &member) {
                            return Err(LocalizationError::LayerCollision { source });
                        }
                        if occupied
                            .get(&member)
                            .is_some_and(|old| *old != Some(source))
                        {
                            return Err(LocalizationError::MemberCollision { member });
                        }
                        occupied.insert(member.clone(), Some(source));
                        names.insert(source, member);
                        queue.push_back(source);
                    }
                    LocalizationResource::Asset { member, data } => {
                        valid_member(&member)?;
                        if occupied.get(&member).is_some_and(Option::is_some)
                            || assets.get(&member).is_some_and(|old| old != &data)
                        {
                            return Err(LocalizationError::MemberCollision { member });
                        }
                        occupied.insert(member.clone(), None);
                        assets.insert(member, data);
                    }
                }
            }
            resolved_uses += 1;
            Ok(relative_member(&member, &target.member).into())
        })?;
        layers.push(LocalizedLayer {
            source,
            member,
            layer: copy,
        });
    }
    layers.sort_by(|a, b| (a.source != root, &a.member).cmp(&(b.source != root, &b.member)));
    Ok(LocalizationPlan {
        layers,
        assets: assets
            .into_iter()
            .map(|(member, data)| LocalizedAsset { member, data })
            .collect(),
        resolved_uses,
    })
}
impl LocalizationPlan {
    /// Serializes copied layers and packages the plan deterministically.
    /// `.usda` members are text; `.usd` and `.usdc` members are binary crates.
    /// The supplied interners must be those used to build the source layers.
    /// Unsupported authored content or USDZ member types return typed errors.
    pub fn write_usdz(
        &self,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<Vec<u8>, LocalizationError> {
        let mut serialized = Vec::with_capacity(self.layers.len());
        for layer in &self.layers {
            valid_layer_member(&layer.member)?;
            let bytes = if layer.member.ends_with(".usda") {
                layerstack_usda::save::save_usda(&layer.layer, tokens, paths)
                    .map_err(LocalizationError::Usda)?
                    .into_bytes()
            } else {
                layerstack_usdc::writer::save_layer(&layer.layer, tokens, paths)
                    .map_err(LocalizationError::Usdc)?
            };
            serialized.push(bytes);
        }
        let files: Vec<_> = self
            .layers
            .iter()
            .zip(&serialized)
            .map(|(layer, data)| PackageFile::new(&layer.member, data))
            .chain(
                self.assets
                    .iter()
                    .map(|asset| PackageFile::new(&asset.member, &asset.data)),
            )
            .collect();
        write_usdz(&files).map_err(LocalizationError::Package)
    }
}
fn resource_member(resource: &LocalizationResource) -> &Arc<str> {
    match resource {
        LocalizationResource::Layer { member, .. } | LocalizationResource::Asset { member, .. } => {
            member
        }
    }
}
fn is_pattern(member: &str) -> bool {
    member.contains("<UDIM>") || member.contains('#')
}
fn valid_member(member: &Arc<str>) -> Result<(), LocalizationError> {
    if member.is_empty()
        || member.contains(['\\', '\0', ':', '[', ']'])
        || member
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        Err(LocalizationError::InvalidMember {
            member: member.clone(),
        })
    } else {
        Ok(())
    }
}
fn valid_layer_member(member: &Arc<str>) -> Result<(), LocalizationError> {
    if [".usd", ".usda", ".usdc"]
        .iter()
        .any(|s| member.ends_with(s))
    {
        Ok(())
    } else {
        Err(LocalizationError::UnsupportedLayerFormat {
            member: member.clone(),
        })
    }
}
fn relative_member(from: &str, to: &str) -> String {
    let mut parent: Vec<_> = from.split('/').collect();
    parent.pop();
    let target: Vec<_> = to.split('/').collect();
    let common = parent
        .iter()
        .zip(&target)
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = "../".repeat(parent.len() - common);
    result.push_str(&target[common..].join("/"));
    result
}

#[cfg(test)]
mod tests;
