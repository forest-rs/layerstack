// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Edit targets: where stage-level edits are written.

use crate::{
    doc::{LayerId, LayerOffset, LayerStore},
    interner::TokenId,
    layer_stack::LayerStack,
    path::{PathId, PathInterner, PropertyPath},
    prim_index::ArcKind,
    prim_index_graph::{NodeId, PrimNode},
    spec_path::SpecPath,
    stage::Stage,
};

/// How an [`EditTarget`] maps stage namespace to spec paths in its layer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NamespaceMap {
    /// Every stage path is the spec path of the same name.
    Identity,
    /// `stage_root` and its namespace descendants map to `spec_root` and
    /// the same descendants beneath it; nothing else maps.
    Prefix {
        stage_root: PathId,
        spec_root: SpecPath,
    },
}

impl NamespaceMap {
    /// The spec path of the stage prim `path`, if the map covers it.
    pub(crate) fn map_prim(&self, path: PathId, paths: &mut PathInterner) -> Option<SpecPath> {
        match self {
            Self::Identity => Some(SpecPath::from_prim_path(path, paths)),
            Self::Prefix {
                stage_root,
                spec_root,
            } => {
                let names = paths
                    .resolve(path)
                    .strip_prefix(paths.resolve(*stage_root))?
                    .to_vec();
                Some(spec_root.join_prims(&names, paths))
            }
        }
    }
}

/// Where stage-level edits are written: a layer, a map from stage
/// namespace to spec paths in that layer, and the layer offset between the
/// layer's times and stage times.
///
/// An edit target is how a host authors *through* composition. Targeting
/// the node of a reference makes an edit of `/World/RockB.size` write
/// `/Rock.size` in the referenced layer; targeting a variant node writes
/// inside the variant (`/Rock{shape=jagged}.roughness`); and a time sample
/// set at stage time 14 through a reference with `(offset = 10; scale = 2)`
/// is written at layer time 2.
///
/// Edits name their target through an [`Address`] ([`EditTarget::prim`],
/// [`EditTarget::property`]), which is mapped when the edit is applied: a
/// path the target does not map is rejected then.
///
/// OpenUSD: `UsdEditTarget` (`pxr/usd/usd/editTarget.h`), constructed from
/// a layer, a layer and a `PcpNodeRef`, or `ForLocalDirectVariant`; its
/// `MapToSpecPath` and the time offset of its `PcpMapFunction`.
///
/// Spec: AOUSD Core §8 (spec paths, including variant selections),
/// §10.3.1.1 and §12.3.2.1 (layer offsets).
#[derive(Clone, Debug, PartialEq)]
pub struct EditTarget {
    layer: LayerId,
    map: NamespaceMap,
    offset: LayerOffset,
}

impl EditTarget {
    /// Targets `layer` directly: stage paths are spec paths and stage times
    /// are layer times.
    ///
    /// OpenUSD: `UsdEditTarget(layer)`.
    #[must_use]
    pub fn for_layer(layer: LayerId) -> Self {
        Self {
            layer,
            map: NamespaceMap::Identity,
            offset: LayerOffset::IDENTITY,
        }
    }

    /// Targets the variant branch `variant` of `layer`, such as
    /// `/Rock{shape=jagged}`: the prim hosting the branch and its namespace
    /// descendants map inside the branch; no other path maps. Any property
    /// suffix of `variant` is ignored.
    ///
    /// The branch does not have to be selected, or to exist: edits through
    /// the target create it.
    ///
    /// OpenUSD: `UsdEditTarget::ForLocalDirectVariant`.
    #[must_use]
    pub fn for_local_variant(layer: LayerId, variant: &SpecPath) -> Self {
        Self {
            layer,
            map: NamespaceMap::Prefix {
                stage_root: variant.prim_path(),
                spec_root: variant.prim_spec(),
            },
            offset: LayerOffset::IDENTITY,
        }
    }

    /// Targets the node `node` of the composed prim `prim`'s index graph
    /// ([`Stage::explain_prim_graph`]), in the root layer of the node's
    /// layer stack.
    ///
    /// `prim` and its namespace descendants map to the node's site and the
    /// same descendants beneath it, including the variant selections of a
    /// variant node; other paths do not map, except through the prim's root
    /// node, which maps every path to itself. Times map through the offsets
    /// of every arc from the root to the node.
    ///
    /// Returns `None` if `prim` is not on the stage, `node` is not a node
    /// of its graph, or `node` is a relocate node: its site is the
    /// relocation source, whose own specs never contribute, so an edit
    /// there would change nothing (AOUSD Core §10.3.2.6). A relocated prim
    /// is edited through its other nodes, which map it to the specs it
    /// reads, or through its root node at the relocation target. Opinions
    /// authored at a relocation source through the root node are not
    /// rejected; composition ignores them.
    ///
    /// OpenUSD: `UsdEditTarget(layer, node)` with the node's layer stack's
    /// root layer.
    #[must_use]
    pub fn for_node(stage: &Stage, prim: PathId, node: NodeId) -> Option<Self> {
        let node_ref = editable_node(stage, prim, node)?;
        Some(Self::from_node(
            prim,
            node_ref,
            node_ref.layer_stack(),
            node_ref.layer_offset(),
        ))
    }

    /// Targets the node `node` of `prim`'s index graph, like
    /// [`EditTarget::for_node`], in `layer`, one of the layers of the node's
    /// layer stack: times also map through that layer's sublayer offset.
    ///
    /// Returns `None` if `prim` is not on the stage, `node` is not a node of
    /// its graph or is a relocate node, or `layer` is not in the node's
    /// layer stack.
    ///
    /// OpenUSD: `UsdEditTarget(layer, node)` with a sublayer of the node's
    /// layer stack.
    #[must_use]
    pub fn for_node_layer(
        stage: &Stage,
        store: &dyn LayerStore,
        prim: PathId,
        node: NodeId,
        layer: LayerId,
    ) -> Option<Self> {
        let node_ref = editable_node(stage, prim, node)?;
        let stack = LayerStack::gather(store, node_ref.layer_stack());
        let index = stack.layers.iter().position(|id| *id == layer)?;
        let offset = node_ref.layer_offset().compose(stack.offset_at(index));
        Some(Self::from_node(prim, node_ref, layer, offset))
    }

    fn from_node(prim: PathId, node: &PrimNode, layer: LayerId, offset: LayerOffset) -> Self {
        let map = if node.parent().is_none() && node.arc_kind() == ArcKind::Local {
            NamespaceMap::Identity
        } else {
            NamespaceMap::Prefix {
                stage_root: prim,
                spec_root: node.site().clone(),
            }
        };
        Self { layer, map, offset }
    }

    /// The layer edits are written to.
    #[must_use]
    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The offset from the target layer's times to stage times.
    #[must_use]
    pub fn layer_offset(&self) -> LayerOffset {
        self.offset
    }

    /// Maps the stage prim path `path` to its spec path in the target
    /// layer; `None` if the target does not map it.
    ///
    /// OpenUSD: `UsdEditTarget::MapToSpecPath`.
    #[must_use]
    pub fn map_to_spec_path(&self, path: PathId, paths: &mut PathInterner) -> Option<SpecPath> {
        self.map.map_prim(path, paths)
    }

    /// Maps the stage property path `path` to its spec path in the target
    /// layer; `None` if the target does not map its prim.
    #[must_use]
    pub fn map_property_to_spec_path(
        &self,
        path: PropertyPath,
        paths: &mut PathInterner,
    ) -> Option<SpecPath> {
        self.map
            .map_prim(path.prim_path(), paths)
            .map(|prim| prim.with_property(path.property()))
    }

    /// Maps the stage time `time` to a time in the target layer.
    ///
    /// OpenUSD: `UsdEditTarget::GetMapFunction().GetTimeOffset()`, inverted,
    /// applied to the time (`UsdStage::_SetValueImpl`).
    #[must_use]
    pub fn map_to_spec_time(&self, time: f64) -> f64 {
        self.offset.map_time(time)
    }

    /// Addresses the stage prim `path` through this target.
    #[must_use]
    pub fn prim(&self, path: PathId) -> Address {
        Address {
            layer: self.layer,
            offset: self.offset,
            path: AddressPath::Stage {
                map: self.map.clone(),
                prim: path,
                property: None,
            },
        }
    }

    /// Addresses the stage property `path` through this target.
    #[must_use]
    pub fn property(&self, path: PropertyPath) -> Address {
        Address {
            layer: self.layer,
            offset: self.offset,
            path: AddressPath::Stage {
                map: self.map.clone(),
                prim: path.prim_path(),
                property: Some(path.property()),
            },
        }
    }
}

/// The node `node` of `prim`'s graph, unless it is a relocate node, whose
/// own specs never contribute (see [`EditTarget::for_node`]).
fn editable_node(stage: &Stage, prim: PathId, node: NodeId) -> Option<&PrimNode> {
    let node = stage.explain_prim_graph(prim)?.node(node)?;
    (node.arc_kind() != ArcKind::Relocates).then_some(node)
}

/// The spec an edit applies to: a spec path in a layer, given directly
/// ([`Address::spec`]) or as a stage path through an [`EditTarget`]
/// ([`EditTarget::prim`], [`EditTarget::property`]).
///
/// A stage address is mapped when the edit is applied, and its times are
/// stage times, mapped through the target's layer offset. A spec address
/// names the spec itself, and its times are layer times.
#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    layer: LayerId,
    offset: LayerOffset,
    path: AddressPath,
}

#[derive(Clone, Debug, PartialEq)]
enum AddressPath {
    Spec(SpecPath),
    Stage {
        map: NamespaceMap,
        prim: PathId,
        property: Option<TokenId>,
    },
}

impl Address {
    /// Addresses the spec at `path` in `layer`: a prim spec (`/Rock`), a
    /// variant spec (`/Rock{shape=jagged}`), or a property spec of either
    /// (`/Rock{shape=jagged}.roughness`).
    #[must_use]
    pub fn spec(layer: LayerId, path: SpecPath) -> Self {
        Self {
            layer,
            offset: LayerOffset::IDENTITY,
            path: AddressPath::Spec(path),
        }
    }

    /// The layer the addressed spec is in.
    #[must_use]
    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The stage prim and property this address names, for a stage
    /// address.
    pub(crate) fn stage_path(&self) -> Option<(PathId, Option<TokenId>)> {
        match self.path {
            AddressPath::Spec(_) => None,
            AddressPath::Stage { prim, property, .. } => Some((prim, property)),
        }
    }

    /// The addressed spec path; `None` if the target does not map the
    /// stage path.
    pub(crate) fn resolve(&self, paths: &mut PathInterner) -> Option<SpecPath> {
        match &self.path {
            AddressPath::Spec(path) => Some(path.clone()),
            AddressPath::Stage {
                map,
                prim,
                property,
            } => {
                let spec = map.map_prim(*prim, paths)?;
                Some(match property {
                    Some(name) => spec.with_property(*name),
                    None => spec,
                })
            }
        }
    }

    /// Maps a time given with this address to a time in its layer.
    pub(crate) fn layer_time(&self, time: f64) -> f64 {
        self.offset.map_time(time)
    }
}
