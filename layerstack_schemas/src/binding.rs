// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Material binding resolution: the material a prim is bound to, and the
//! binding relationship that decides it, as OpenUSD's
//! `UsdShadeMaterialBindingAPI::ComputeBoundMaterial` resolves it.
//!
//! The computation is pure steps over explicit reads. [`BindingInputs`]
//! gathers one prim's bindings (its one place of stage reads);
//! [`BoundMaterial::resolve`] folds them from the prim up to the root,
//! asking a membership function whether each bound collection includes the
//! prim. [`PrimView::compute_bound_material`] is the one-shot caller and
//! [`BindingCache`] the caller for many prims; an incremental graph can be
//! another.

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use layerstack::{
    HashMap, HashSet, PathId, PropertyKind, PropertyPath, ResolvedValue, TargetPath, Value,
};

use crate::collection::{Membership, MembershipQuery, collection_name};
use crate::usd::CollectionApi;
use crate::usd_shade::{Material, MaterialBindingApi};
use crate::view::{PrimView, Scene};

/// `material:binding`.
const BINDING: &str = "material:binding";
/// `material:binding:collection`.
const COLLECTION_BINDING: &str = "material:binding:collection";
/// The binding strength metadata of a binding relationship.
const BIND_MATERIAL_AS: &str = "bindMaterialAs";

/// A material purpose: what the bound material is for.
///
/// OpenUSD: `UsdShadeTokens->allPurpose` (the empty token), `preview` and
/// `full`, or any other token.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MaterialPurpose {
    /// Every purpose: the fallback of the others.
    All,
    /// `preview`: for fast, interactive rendering.
    Preview,
    /// `full`: for final quality rendering.
    Full,
    /// Another purpose.
    Other(Arc<str>),
}

impl MaterialPurpose {
    /// The purpose `token` names; the empty token is [`MaterialPurpose::All`].
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "" => Self::All,
            "preview" => Self::Preview,
            "full" => Self::Full,
            other => Self::Other(Arc::from(other)),
        }
    }

    /// Its token (empty for [`MaterialPurpose::All`]).
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::All => "",
            Self::Preview => "preview",
            Self::Full => "full",
            Self::Other(token) => token,
        }
    }

    /// The name of its direct binding relationship (`material:binding`,
    /// `material:binding:preview`).
    fn direct_name(&self) -> String {
        match self {
            Self::All => BINDING.to_string(),
            other => format!("{BINDING}:{}", other.as_str()),
        }
    }

    /// The prefix of its collection binding relationships.
    fn collection_prefix(&self) -> String {
        match self {
            Self::All => COLLECTION_BINDING.to_string(),
            other => format!("{COLLECTION_BINDING}:{}", other.as_str()),
        }
    }

    /// The purpose a binding relationship's name gives
    /// (`material:binding:preview`, `material:binding:collection:full:a`).
    ///
    /// OpenUSD: `_GetMaterialPurpose` in `materialBindingAPI.cpp`.
    fn of_relationship(name: &str) -> Self {
        let parts: Vec<&str> = name.split(':').collect();
        match parts.len() {
            5 => Self::from_token(parts[3]),
            3 => Self::from_token(parts[2]),
            _ => Self::All,
        }
    }
}

/// Whether a binding holds against its prim's descendants' bindings.
///
/// OpenUSD: the `bindMaterialAs` metadata; unauthored is weaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingStrength {
    /// A descendant's own binding wins (the default).
    WeakerThanDescendants,
    /// This binding wins over its descendants'.
    StrongerThanDescendants,
}

/// How bindings are resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BindingOptions {
    /// Whether bindings on prims without `MaterialBindingAPI` applied count,
    /// as they do by default in OpenUSD 26.08 (`supportLegacyBindings`,
    /// which OpenUSD means to turn off in a future release). Such a binding
    /// is reported ([`Binding::without_binding_api`]) where OpenUSD warns.
    pub support_legacy_bindings: bool,
}

impl Default for BindingOptions {
    fn default() -> Self {
        Self {
            support_legacy_bindings: true,
        }
    }
}

/// A direct binding: `material:binding[:<purpose>]` targeting one material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectBinding {
    /// The binding relationship.
    pub relationship: PropertyPath,
    /// The purpose its name gives.
    pub purpose: MaterialPurpose,
    /// The bound material path: its one forwarded target, a prim.
    pub material: PathId,
    /// Its strength.
    pub strength: BindingStrength,
}

/// A collection binding: `material:binding:collection[:<purpose>]:<name>`
/// targeting a collection and a material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionBinding {
    /// The binding relationship.
    pub relationship: PropertyPath,
    /// The bound collection's path (`/Prim.collection:name`).
    pub collection: PropertyPath,
    /// The bound material path.
    pub material: PathId,
    /// Its strength.
    pub strength: BindingStrength,
}

/// What one prim contributes to binding resolution for a purpose: every
/// stage read the resolution makes for it.
///
/// It reads whether the prim has `MaterialBindingAPI` applied, the names of
/// its authored properties starting with `material:binding` in the prim's
/// property order (dictionary order, then `reorder properties`), and for each
/// binding relationship the purpose needs its forwarded targets (following
/// targets that are relationships) and `bindMaterialAs`.
///
/// OpenUSD: `UsdShadeMaterialBindingAPI::BindingsAtPrim`, which lists
/// collection bindings with `UsdPrim::GetAuthoredPropertiesInNamespace`
/// (property order applied).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BindingInputs {
    /// Whether the prim has `MaterialBindingAPI` applied.
    pub binding_api: bool,
    /// Its direct binding for the purpose, else its all-purpose direct
    /// binding.
    pub direct: Option<DirectBinding>,
    /// Its collection bindings for the (restricted) purpose, in property
    /// order: the first that includes a prim decides.
    pub restricted: Vec<CollectionBinding>,
    /// Its all-purpose collection bindings, in property order.
    pub all_purpose: Vec<CollectionBinding>,
}

/// Whether the property `path` is a relationship: one an opinion declares
/// or a schema defines.
fn is_relationship(scene: &Scene<'_>, path: PropertyPath) -> bool {
    let stage = scene.stage();
    if !stage.has_prim(path.prim_path()) {
        return false;
    }
    if let Some(declared) = stage.resolve_property_declaration(path.prim_path(), path.property()) {
        return declared.kind == PropertyKind::Relationship;
    }
    stage
        .property_definition(path.prim_path(), path.property(), scene.store())
        .is_some_and(|defined| defined.kind == PropertyKind::Relationship)
}

/// The targets of the relationship `path`, following each target that is
/// itself a relationship (each once).
///
/// OpenUSD: `UsdRelationship::GetForwardedTargets`.
fn forwarded_targets(scene: &Scene<'_>, path: PropertyPath) -> Vec<TargetPath> {
    fn walk(
        scene: &Scene<'_>,
        path: PropertyPath,
        visited: &mut HashSet<PropertyPath>,
        unique: &mut HashSet<TargetPath>,
        out: &mut Vec<TargetPath>,
    ) {
        let targets = scene
            .stage()
            .resolve_target_list_path(path)
            .map(|resolved| resolved.value)
            .unwrap_or_default();
        for target in targets {
            if let TargetPath::Property(property) = target
                && is_relationship(scene, property)
            {
                if visited.insert(property) {
                    walk(scene, property, visited, unique, out);
                }
                continue;
            }
            if unique.insert(target) {
                out.push(target);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        scene,
        path,
        &mut HashSet::new(),
        &mut HashSet::new(),
        &mut out,
    );
    out
}

fn strength(scene: &Scene<'_>, relationship: PropertyPath) -> BindingStrength {
    let tokens = scene.store().tokens();
    let stronger = tokens.lookup(BIND_MATERIAL_AS).is_some_and(|key| {
        scene
            .stage()
            .resolve_property_metadata(relationship.prim_path(), relationship.property(), key)
            .is_some_and(|resolved| match resolved.value {
                ResolvedValue::Scalar(Value::Token(token)) => {
                    tokens.resolve(token) == "strongerThanDescendants"
                }
                _ => false,
            })
    });
    if stronger {
        BindingStrength::StrongerThanDescendants
    } else {
        BindingStrength::WeakerThanDescendants
    }
}

/// The direct binding `relationship`, if it binds one prim.
///
/// OpenUSD: `UsdShadeMaterialBindingAPI::DirectBinding`.
fn direct(scene: &Scene<'_>, relationship: PropertyPath) -> Option<DirectBinding> {
    if !is_relationship(scene, relationship) {
        return None;
    }
    let targets = forwarded_targets(scene, relationship);
    let [TargetPath::Prim(material)] = targets.as_slice() else {
        return None;
    };
    let name = scene.store().tokens().resolve(relationship.property());
    Some(DirectBinding {
        relationship,
        purpose: MaterialPurpose::of_relationship(name),
        material: *material,
        strength: strength(scene, relationship),
    })
}

/// The collection binding `relationship`, if it binds a collection (a
/// property target) and a material (a prim target).
///
/// OpenUSD: `UsdShadeMaterialBindingAPI::CollectionBinding`.
fn collection_binding(scene: &Scene<'_>, relationship: PropertyPath) -> Option<CollectionBinding> {
    if !is_relationship(scene, relationship) {
        return None;
    }
    let targets = forwarded_targets(scene, relationship);
    let (material, collection) = match targets.as_slice() {
        [TargetPath::Prim(m), TargetPath::Property(c)]
        | [TargetPath::Property(c), TargetPath::Prim(m)] => (*m, *c),
        _ => return None,
    };
    Some(CollectionBinding {
        relationship,
        collection,
        material,
        strength: strength(scene, relationship),
    })
}

impl BindingInputs {
    /// Reads the bindings of the prim at `path` for `purpose`.
    #[must_use]
    pub fn read(
        scene: &Scene<'_>,
        path: PathId,
        purpose: &MaterialPurpose,
        options: BindingOptions,
    ) -> Self {
        let binding_api = scene.has_api(path, MaterialBindingApi::SCHEMA, None);
        let mut out = Self {
            binding_api,
            ..Self::default()
        };
        if !options.support_legacy_bindings && !binding_api {
            return out;
        }
        let tokens = scene.store().tokens();
        let names: Vec<(&str, layerstack::TokenId)> = scene
            .stage()
            .authored_property_names(path, scene.store())
            .into_iter()
            .map(|token| (tokens.resolve(token), token))
            .filter(|(name, _)| name.starts_with(BINDING))
            .collect();
        if names.is_empty() {
            return out;
        }
        let find = |wanted: &str| {
            names
                .iter()
                .find(|(name, _)| *name == wanted)
                .map(|(_, token)| PropertyPath::new(path, *token))
        };
        let mut chosen = find(&purpose.direct_name()).map(|rel| direct(scene, rel));
        if *purpose != MaterialPurpose::All
            && !matches!(chosen, Some(Some(_)))
            && let Some(rel) = find(BINDING)
        {
            chosen = Some(direct(scene, rel));
        }
        out.direct = chosen.flatten();
        let collections = |purpose: &MaterialPurpose| -> Vec<CollectionBinding> {
            let prefix = purpose.collection_prefix();
            names
                .iter()
                .filter(|(name, _)| {
                    name.len() > prefix.len()
                        && name.as_bytes()[prefix.len()] == b':'
                        && name.starts_with(&prefix)
                        && (*purpose != MaterialPurpose::All
                            || !name[prefix.len() + 1..].contains(':'))
                })
                .filter_map(|(_, token)| collection_binding(scene, PropertyPath::new(path, *token)))
                .collect()
        };
        if *purpose != MaterialPurpose::All {
            out.restricted = collections(purpose);
        }
        out.all_purpose = collections(&MaterialPurpose::All);
        out
    }
}

/// How a winning binding binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingKind {
    /// A direct binding.
    Direct,
    /// A collection binding, through the collection at this path.
    Collection {
        /// The collection path.
        collection: PropertyPath,
    },
}

/// The binding relationship that decides a prim's material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The binding relationship.
    pub relationship: PropertyPath,
    /// Direct or through a collection.
    pub kind: BindingKind,
    /// Its strength.
    pub strength: BindingStrength,
    /// The purpose it was found for: the requested one, or all-purpose
    /// when the requested purpose found nothing.
    pub purpose: MaterialPurpose,
    /// The path it targets as the material, whatever is there.
    pub target: PathId,
    /// Whether the prim authoring it lacks `MaterialBindingAPI`: a legacy
    /// binding, which OpenUSD warns about.
    pub without_binding_api: bool,
}

/// A prim's bound material.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundMaterial {
    /// The bound material: the winning binding's target, when a `Material`
    /// is there.
    pub material: Option<PathId>,
    /// The binding that decided it; `None` when nothing binds the prim.
    pub binding: Option<Binding>,
}

/// A binding resolution that depends on a collection membership layerstack
/// cannot decide (a `membershipExpression`): no material is guessed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undecided {
    /// The prim whose material was asked for.
    pub prim: PathId,
    /// The collection binding whose membership decides the outcome.
    pub relationship: PropertyPath,
    /// Its collection.
    pub collection: PropertyPath,
}

struct Winner {
    prim: PathId,
    binding: Binding,
}

impl BoundMaterial {
    /// Resolves the material of the prim `target` for `purpose` from the
    /// inputs of the prim and its ancestors, nearest first (the pseudo-root
    /// excluded). `membership(collection)` says whether a bound collection
    /// includes `target`, `None` when there is no such collection;
    /// `is_material(path)` whether a `Material` is at `path`. Pure: it reads
    /// nothing else.
    ///
    /// For the purpose, then all-purpose, the walk from the prim up keeps a
    /// winner: an ancestor's binding replaces it only when nothing is bound
    /// yet or it is `strongerThanDescendants`, and a collection binding
    /// including the prim also replaces a direct binding of its own prim.
    /// The first collection binding of a prim that applies wins. A purpose
    /// with a winner ends the search.
    ///
    /// OpenUSD: `UsdShadeMaterialBindingAPI::ComputeBoundMaterial`.
    ///
    /// # Errors
    ///
    /// [`Undecided`] when a membership the outcome depends on is
    /// [`Membership::ExpressionUnsupported`].
    pub fn resolve(
        target: PathId,
        purpose: &MaterialPurpose,
        ancestry: &[(PathId, &BindingInputs)],
        mut membership: impl FnMut(PropertyPath) -> Option<Membership>,
        is_material: impl Fn(PathId) -> bool,
    ) -> Result<Self, Undecided> {
        let mut purposes = alloc::vec![purpose.clone()];
        if *purpose != MaterialPurpose::All {
            purposes.push(MaterialPurpose::All);
        }
        for pass in purposes {
            let mut winner: Option<Winner> = None;
            for (prim, inputs) in ancestry {
                if let Some(direct) = &inputs.direct
                    && direct.purpose == pass
                    && (winner.is_none()
                        || direct.strength == BindingStrength::StrongerThanDescendants)
                {
                    winner = Some(Winner {
                        prim: *prim,
                        binding: Binding {
                            relationship: direct.relationship,
                            kind: BindingKind::Direct,
                            strength: direct.strength,
                            purpose: pass.clone(),
                            target: direct.material,
                            without_binding_api: !inputs.binding_api,
                        },
                    });
                }
                let collections = if pass == MaterialPurpose::All {
                    &inputs.all_purpose
                } else {
                    &inputs.restricted
                };
                for binding in collections {
                    let Some(member) = membership(binding.collection) else {
                        continue;
                    };
                    let takes = winner.as_ref().is_none_or(|w| w.prim == *prim)
                        || binding.strength == BindingStrength::StrongerThanDescendants;
                    match member {
                        Membership::Excluded => continue,
                        Membership::ExpressionUnsupported => {
                            if takes {
                                return Err(Undecided {
                                    prim: target,
                                    relationship: binding.relationship,
                                    collection: binding.collection,
                                });
                            }
                            continue;
                        }
                        Membership::Included(_) => {}
                    }
                    if takes {
                        winner = Some(Winner {
                            prim: *prim,
                            binding: Binding {
                                relationship: binding.relationship,
                                kind: BindingKind::Collection {
                                    collection: binding.collection,
                                },
                                strength: binding.strength,
                                purpose: pass.clone(),
                                target: binding.material,
                                without_binding_api: !inputs.binding_api,
                            },
                        });
                        break;
                    }
                }
            }
            if let Some(winner) = winner {
                let target = winner.binding.target;
                return Ok(Self {
                    material: is_material(target).then_some(target),
                    binding: Some(winner.binding),
                });
            }
        }
        Ok(Self::default())
    }
}

/// What a [`BindingCache`] has done since it was made or cleared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BindingCacheStats {
    /// Prims' binding inputs read.
    pub inputs_read: usize,
    /// Collection membership queries computed.
    pub queries_computed: usize,
    /// Reads and queries answered from the cache.
    pub hits: usize,
}

/// Bound materials of many prims for one purpose, sharing each ancestor's
/// bindings and each collection's membership query.
///
/// The caller owns it; nothing is global. It holds results for the
/// [`Scene`] it is queried with; after an edit, [`BindingCache::invalidate`]
/// drops a prim's bindings, [`BindingCache::invalidate_collection`] a
/// collection's membership, and [`BindingCache::clear`] everything.
///
/// OpenUSD: `UsdShadeMaterialBindingAPI::BindingsCache` and
/// `CollectionQueryCache`, as `ComputeBoundMaterials` uses them.
#[derive(Clone, Debug)]
pub struct BindingCache {
    purpose: MaterialPurpose,
    options: BindingOptions,
    inputs: HashMap<PathId, BindingInputs>,
    queries: HashMap<PropertyPath, Option<MembershipQuery>>,
    stats: BindingCacheStats,
}

impl BindingCache {
    /// An empty cache for `purpose`, resolved with `options`.
    #[must_use]
    pub fn new(purpose: MaterialPurpose, options: BindingOptions) -> Self {
        Self {
            purpose,
            options,
            inputs: HashMap::new(),
            queries: HashMap::new(),
            stats: BindingCacheStats::default(),
        }
    }

    /// Drops everything held, and the statistics.
    pub fn clear(&mut self) {
        self.inputs.clear();
        self.queries.clear();
        self.stats = BindingCacheStats::default();
    }

    /// Drops the bindings read for the prim at `path`.
    ///
    /// Call it after an edit to anything [`BindingInputs::read`] reads for
    /// that prim: its binding relationships' targets or `bindMaterialAs`,
    /// its `reorder properties`, whether it has `MaterialBindingAPI`, or a
    /// relationship one of its bindings forwards through (which may be on
    /// another prim).
    pub fn invalidate(&mut self, path: PathId) {
        self.inputs.remove(&path);
    }

    /// Drops the membership query of the collection at `collection`, and
    /// every held query that reads it ([`MembershipQuery::depends_on`]):
    /// those of the collections that include it, directly or not.
    ///
    /// Call it after an edit to anything the collection's membership reads:
    /// its `includes`, `excludes`, `expansionRule`, `includeRoot`, `mode` or
    /// `membershipExpression`, whether its prim has that `CollectionAPI`
    /// instance, or whether the prim of a collection it includes exists.
    pub fn invalidate_collection(&mut self, collection: PropertyPath) {
        self.queries.remove(&collection);
        self.queries
            .retain(|_, query| !query.as_ref().is_some_and(|q| q.depends_on(collection)));
    }

    /// What it has computed and reused.
    #[must_use]
    pub fn stats(&self) -> BindingCacheStats {
        self.stats
    }

    /// The membership query of the collection at `collection`, `None` when
    /// no prim there has that `CollectionAPI` instance applied.
    pub fn membership_query(
        &mut self,
        scene: &Scene<'_>,
        collection: PropertyPath,
    ) -> Option<&MembershipQuery> {
        if self.queries.contains_key(&collection) {
            self.stats.hits += 1;
        } else {
            let prim = collection.prim_path();
            let name = collection_name(scene.store().tokens().resolve(collection.property()));
            let query = name
                .filter(|name| scene.has_api(prim, CollectionApi::SCHEMA, Some(name)))
                .map(|name| MembershipQuery::compute(scene, prim, name));
            self.queries.insert(collection, query);
            self.stats.queries_computed += 1;
        }
        self.queries.get(&collection).and_then(Option::as_ref)
    }

    /// The bound material of the prim at `path`.
    ///
    /// OpenUSD: `UsdShadeMaterialBindingAPI::ComputeBoundMaterial`, and
    /// `ComputeBoundMaterials` for many prims.
    ///
    /// # Errors
    ///
    /// [`Undecided`], as [`BoundMaterial::resolve`] reports it.
    pub fn compute_bound_material(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<BoundMaterial, Undecided> {
        let mut ancestry = Vec::new();
        let mut at = Some(path);
        while let Some(prim) = at {
            if scene.parent(prim).is_none() {
                break;
            }
            if self.inputs.contains_key(&prim) {
                self.stats.hits += 1;
            } else {
                let inputs = BindingInputs::read(scene, prim, &self.purpose, self.options);
                self.inputs.insert(prim, inputs);
                self.stats.inputs_read += 1;
            }
            ancestry.push(prim);
            at = scene.parent(prim);
        }
        // Every collection the ancestry binds, queried up front.
        let collections: Vec<PropertyPath> = ancestry
            .iter()
            .flat_map(|prim| {
                let inputs = &self.inputs[prim];
                inputs
                    .restricted
                    .iter()
                    .chain(&inputs.all_purpose)
                    .map(|binding| binding.collection)
                    .collect::<Vec<_>>()
            })
            .collect();
        for collection in &collections {
            self.membership_query(scene, *collection);
        }
        let chain: Vec<(PathId, &BindingInputs)> = ancestry
            .iter()
            .map(|prim| (*prim, &self.inputs[prim]))
            .collect();
        let queries = &self.queries;
        BoundMaterial::resolve(
            path,
            &self.purpose,
            &chain,
            |collection| {
                queries
                    .get(&collection)
                    .and_then(Option::as_ref)
                    .map(|query| query.is_included(scene, TargetPath::Prim(path)))
            },
            |material| scene.is_a(material, Material::SCHEMA),
        )
    }

    /// The bound materials of the prims at `paths`, in order.
    ///
    /// OpenUSD: `UsdShadeMaterialBindingAPI::ComputeBoundMaterials`.
    pub fn compute_bound_materials(
        &mut self,
        scene: &Scene<'_>,
        paths: &[PathId],
    ) -> Vec<Result<BoundMaterial, Undecided>> {
        paths
            .iter()
            .map(|path| self.compute_bound_material(scene, *path))
            .collect()
    }
}

impl<'a> PrimView<'a> {
    /// The material the prim is bound to for `purpose`, with the binding
    /// that decides it ([`BindingCache`] for many prims).
    ///
    /// OpenUSD: `UsdShadeMaterialBindingAPI(prim).ComputeBoundMaterial`.
    ///
    /// # Errors
    ///
    /// [`Undecided`] when the outcome depends on a collection's
    /// `membershipExpression`, which layerstack does not evaluate yet.
    pub fn compute_bound_material(
        &self,
        purpose: &MaterialPurpose,
        options: BindingOptions,
    ) -> Result<BoundMaterial, Undecided> {
        BindingCache::new(purpose.clone(), options)
            .compute_bound_material(&self.scene(), self.path())
    }
}
