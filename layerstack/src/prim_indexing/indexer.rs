// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Builds one prim index.
//!
//! A prim's index starts as a copy of its namespace parent's, each node's
//! site moved to the child's name. The arcs authored at each node's site are
//! then evaluated as tasks, strongest kinds first: relocations, references,
//! payloads, inherits and specializes, the classes they imply, then
//! variants, whose selections can depend on every opinion gathered so far,
//! and last the payloads to dynamic file formats, whose arguments can too
//! (see [`crate::dynamic_format`]).
//! An arc to a site below a root prim brings the opinions of that site's
//! ancestors along: the index of the target site is built (recursively, in
//! the target's layer stack) and grafted beneath the arc's node.
//!
//! Spec: AOUSD Core §10 (composition arcs), §10.4 (strength ordering).
//! OpenUSD: `Pcp_BuildPrimIndex` and the tasks of `Pcp_PrimIndexer`
//! (`pxr/usd/pcp/primIndex.cpp`).

use alloc::{string::String, sync::Arc, vec::Vec};

use hashbrown::HashSet;

use crate::{
    asset::{AssetRequest, AssetUse, FormatArgs},
    composition_error::{
        ArcCycle, ArcCycleSite, ArcToProhibitedChild, CompositionError, OpinionAtRelocationSource,
        UnresolvedAsset, UnresolvedDefaultPrim, UnresolvedPrimPath,
    },
    doc::{AssetAvailability, LayerId, LayerOffset, LayerStore, ReferenceTarget},
    dynamic_format::{DynamicDependency, DynamicFileFormat, DynamicFormatContext, DynamicFormats},
    expression_variables::{ExpressionScope, VariableReads},
    interner::TokenId,
    layer_stack::LayerStackIdentifier,
    path::PathId,
    prim_index::ArcKind,
    spec_path::SpecPath,
    variant_fallbacks::VariantFallbacks,
};

use super::{
    dynamic_args::Walk,
    graph::{Graph, NONE, Node, NodeIndex, Site, SubgraphArc, is_class_based, non_variant_depth},
    layer_stacks::{LayerStacks, StackId},
    map_function::MapFunction,
    site::{self, ChildIndex},
    site_paths,
};

/// What every prim index of one composition shares.
pub(crate) struct Ctx<'s> {
    pub(crate) store: &'s dyn LayerStore,
    pub(crate) stacks: LayerStacks<'s>,
    pub(crate) child_index: &'s ChildIndex,
    pub(crate) fallbacks: &'s VariantFallbacks,
    /// Whether payloads at a stage path are loaded.
    pub(crate) include_payload: &'s (dyn Fn(&crate::path::PathInterner, PathId) -> bool + Sync),
    /// The stage's own layer stack.
    pub(crate) stage_stack: StackId,
    pub(crate) root_path: PathId,
    /// The dynamic file formats payloads compose arguments for.
    pub(crate) dynamic_formats: &'s DynamicFormats,
    /// The registry whose plugin metadata dynamic arguments may read.
    pub(crate) schemas: Option<&'s crate::schema::SchemaRegistry>,
    /// The token `__AnyType__`, when interned: a type name naming no
    /// type ([`crate::Stage::source_type_name`]).
    pub(crate) any_type: Option<TokenId>,
    /// The shapes of the prim index graphs composed so far, which graphs
    /// of the same shape share.
    pub(crate) shapes: crate::prim_index_graph::GraphShapes,
    /// The most nodes one prim index holds
    /// ([`crate::CompositionLimits::nodes_per_prim_index`]).
    pub(crate) nodes_per_prim_index: Option<usize>,
}

/// Whether a prim index's payload was included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PayloadState {
    #[default]
    NoPayload,
    Included,
    Excluded,
}

/// An error building a prim index found, in the order found.
pub(crate) enum Recorded {
    Error(CompositionError),
    /// An arc of `prim` reached `stack`: the errors gathering the stack
    /// found belong to the first prim, in traversal order, whose arc
    /// reaches it (OpenUSD appends them to that prim index's errors in
    /// `_EvalRefOrPayloadArcs`), whichever thread gathered it.
    StackErrors {
        stack: StackId,
        prim: PathId,
    },
}

/// What building a prim index found.
#[derive(Default)]
pub(crate) struct Outputs {
    pub(crate) graph: Graph,
    pub(crate) errors: Vec<Recorded>,
    pub(crate) payload_state: PayloadState,
    pub(crate) asset_requests: Vec<AssetRequest>,
    /// Layers whose `defaultPrim` an arc consulted.
    pub(crate) default_prim_layers: Vec<LayerId>,
    pub(crate) reads: VariableReads,
    /// Layers that author a reference or payload this index follows.
    pub(crate) arc_layers: Vec<LayerId>,
    /// What the arguments of the dynamic payloads this index follows were
    /// composed from.
    pub(crate) dynamic: DynamicDependency,
    /// The layers the store has not loaded yet that arcs of this index
    /// target, each once: an index that reads any is incomplete, and is
    /// not kept ([`crate::LayerStore::is_layer_pending`]).
    pub(crate) pending: Vec<LayerId>,
    /// Whether the index reached [`Ctx::nodes_per_prim_index`]: an arc
    /// was not added.
    pub(crate) full: bool,
}

impl Outputs {
    fn append(&mut self, other: Self) {
        self.errors.extend(other.errors);
        if other.payload_state != PayloadState::NoPayload
            && self.payload_state == PayloadState::NoPayload
        {
            self.payload_state = other.payload_state;
        }
        self.asset_requests.extend(other.asset_requests);
        self.default_prim_layers.extend(other.default_prim_layers);
        self.reads.extend(other.reads);
        self.arc_layers.extend(other.arc_layers);
        self.dynamic.append(other.dynamic);
        for layer in other.pending {
            if !self.pending.contains(&layer) {
                self.pending.push(layer);
            }
        }
        self.full |= other.full;
    }
}

/// Links a recursively built index to the index it will be grafted into.
/// OpenUSD: `PcpPrimIndex_StackFrame`.
pub(crate) struct Frame<'f> {
    /// The site the recursive index is built for.
    requested_site: Site,
    pub(super) parent_graph: &'f Graph,
    pub(super) parent_node: NodeIndex,
    arc_kind: ArcKind,
    pub(super) arc_map_to_parent: &'f MapFunction,
    pub(super) previous: Option<&'f Self>,
    skip_duplicate_nodes: bool,
}

/// Walks up a node's ancestors, across frames.
/// OpenUSD: `PcpPrimIndex_StackFrameIterator`.
#[derive(Clone, Copy)]
struct FrameCursor<'a> {
    graph: &'a Graph,
    node: NodeIndex,
    frame: Option<&'a Frame<'a>>,
}

impl<'a> FrameCursor<'a> {
    fn new(graph: &'a Graph, node: NodeIndex, frame: Option<&'a Frame<'a>>) -> Self {
        Self { graph, node, frame }
    }

    fn is_valid(&self) -> bool {
        self.node != NONE
    }

    fn entry(&self) -> &'a Node {
        self.graph.node(self.node)
    }

    fn next(&mut self) {
        if self.entry().arc_kind != ArcKind::Local {
            self.node = self.entry().parent;
        } else {
            self.next_frame();
        }
    }

    fn next_frame(&mut self) {
        match self.frame {
            Some(frame) => {
                self.graph = frame.parent_graph;
                self.node = frame.parent_node;
                self.frame = frame.previous;
            }
            None => self.node = NONE,
        }
    }

    fn arc_kind(&self) -> Option<ArcKind> {
        let kind = self.entry().arc_kind;
        if kind != ArcKind::Local {
            return Some(kind);
        }
        self.frame.map(|frame| frame.arc_kind)
    }
}

/// The tasks of a prim index, in priority order. OpenUSD: `Task::Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TaskKind {
    NodeRelocations,
    ImpliedRelocations,
    NodeReferences,
    NodePayloads,
    NodeInherits,
    NodeSpecializes,
    ImpliedSpecializes,
    ImpliedClasses,
    AncestralVariantSets,
    AncestralVariantAuthored,
    AncestralVariantFallback,
    AncestralVariantNoneFound,
    AncestralDynamicPayloads,
    VariantSets,
    VariantAuthored,
    VariantFallback,
    VariantNoneFound,
    NodeDynamicPayloads,
    UnresolvedPrimPathError,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Task {
    kind: TaskKind,
    node: NodeIndex,
    vset_path: Option<SpecPath>,
    vset_name: Option<TokenId>,
    vset_num: u16,
}

impl Task {
    fn new(kind: TaskKind, node: NodeIndex) -> Self {
        Self {
            kind,
            node,
            vset_path: None,
            vset_name: None,
            vset_num: 0,
        }
    }

    fn variant(kind: TaskKind, node: NodeIndex, path: SpecPath, name: TokenId, num: u16) -> Self {
        Self {
            kind,
            node,
            vset_path: Some(path),
            vset_name: Some(name),
            vset_num: num,
        }
    }

    fn is_implied(&self) -> bool {
        matches!(
            self.kind,
            TaskKind::ImpliedClasses | TaskKind::ImpliedSpecializes
        )
    }
}

/// Which tasks to queue for a node. OpenUSD: `Task::Tasks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaskSet(u32);

impl TaskSet {
    const ALL: Self = Self(u32::MAX);
    const NONE: Self = Self(0);

    fn bit(kind: TaskKind) -> u32 {
        1 << kind as u32
    }

    fn has(self, kind: TaskKind) -> bool {
        self.0 & Self::bit(kind) != 0
    }

    fn without(self, kind: TaskKind) -> Self {
        Self(self.0 & !Self::bit(kind))
    }

    fn with(self, kind: TaskKind) -> Self {
        Self(self.0 | Self::bit(kind))
    }

    /// Without the variant and dynamic payload tasks, which wait for the
    /// opinions of stronger sites. OpenUSD: `VariantsAndDynamicPayloadTasks`.
    fn without_variants_and_dynamic_payloads(self) -> Self {
        self.without(TaskKind::VariantSets)
            .without(TaskKind::NodeDynamicPayloads)
    }

    /// OpenUSD: `AncestralVariantsAndDynamicPayloadTasks`.
    fn without_ancestral_variants_and_dynamic_payloads(self) -> Self {
        self.without(TaskKind::AncestralVariantSets)
            .without(TaskKind::AncestralDynamicPayloads)
    }

    fn without_expressed_arcs(self) -> Self {
        self.without(TaskKind::NodeSpecializes)
            .without(TaskKind::NodeInherits)
            .without(TaskKind::NodePayloads)
            .without(TaskKind::NodeReferences)
            .without(TaskKind::NodeRelocations)
    }
}

/// Options for adding an arc. OpenUSD: `_ArcOptions`.
#[derive(Clone, Copy)]
struct ArcOptions {
    direct_node_contributes: bool,
    include_ancestral_opinions: bool,
    skip_duplicate_nodes: bool,
    copy_ancestor_flag_from_origin: bool,
    tasks: TaskSet,
}

impl Default for ArcOptions {
    fn default() -> Self {
        Self {
            direct_node_contributes: true,
            include_ancestral_opinions: false,
            skip_duplicate_nodes: false,
            copy_ancestor_flag_from_origin: false,
            tasks: TaskSet::ALL,
        }
    }
}

/// Builds one prim index. OpenUSD: `Pcp_PrimIndexer`.
struct Indexer<'c, 's, 'f> {
    ctx: &'c Ctx<'s>,
    root_site: &'c Site,
    previous_frame: Option<&'f Frame<'f>>,
    evaluate_implied_specializes: bool,
    evaluate_variants: bool,
    /// The finalized index of the namespace parent of a stage prim, for
    /// the prim's index to start from.
    parent: Option<&'c Graph>,
    outputs: &'c mut Outputs,
    /// The open tasks, a heap with the highest priority task first
    /// ([`Indexer::higher_priority`]): a prim with thousands of arcs has
    /// a task for each node they add.
    tasks: Vec<Task>,
    unique: HashSet<Task>,
}

/// Builds the prim index of `site`. OpenUSD: `Pcp_BuildPrimIndex`.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors `Pcp_BuildPrimIndex`, whose inputs are separate"
)]
pub(crate) fn build_prim_index(
    ctx: &Ctx<'_>,
    site: &Site,
    root_site: &Site,
    evaluate_implied_specializes: bool,
    evaluate_variants: bool,
    root_contributes: bool,
    previous_frame: Option<&Frame<'_>>,
    parent: Option<&Graph>,
    outputs: &mut Outputs,
) {
    let paths = ctx.store.paths();
    let depth = paths.resolve(site.path.prim_path()).depth();
    if depth == 0 && !site.path.has_variant_selections() {
        let mut graph = Graph::new(site.clone(), 0);
        let has_specs = site::has_specs(ctx.store, ctx.stacks.get(site.stack), &site.path);
        graph.node_mut(0).has_specs = has_specs;
        outputs.graph = graph;
        return;
    }
    let mut indexer = Indexer {
        ctx,
        root_site,
        previous_frame,
        evaluate_implied_specializes,
        evaluate_variants,
        parent,
        outputs,
        tasks: Vec::new(),
        unique: HashSet::new(),
    };
    if site_paths::is_variant_selection_path(&site.path) {
        let namespace_depth = non_variant_depth(indexer.ctx.store.paths(), &site.path);
        let mut graph = Graph::new(site.clone(), namespace_depth);
        let has_specs = site::has_specs(
            indexer.ctx.store,
            indexer.ctx.stacks.get(site.stack),
            &site.path,
        );
        graph.node_mut(0).has_specs = has_specs;
        graph.set_inert(indexer.ctx.store.paths(), 0, !root_contributes);
        indexer.outputs.graph = graph;
    } else {
        indexer.build_initial_from_ancestor(site, root_contributes);
        if indexer.elide_if_prohibited() {
            return;
        }
    }
    indexer.add_tasks_for_root();
    while let Some(task) = indexer.pop_task() {
        indexer.run(task);
    }
}

impl Indexer<'_, '_, '_> {
    fn graph(&self) -> &Graph {
        &self.outputs.graph
    }

    fn paths(&self) -> &crate::path::PathInterner {
        self.ctx.store.paths()
    }

    fn originating_prim(&self) -> PathId {
        self.root_site.path.prim_path()
    }

    fn record_error(&mut self, error: CompositionError) {
        self.outputs.errors.push(Recorded::Error(error));
    }

    // ── Tasks ──────────────────────────────────────────────────────────

    /// Adds `task` to the heap of open tasks. OpenUSD: `AddTask`, which
    /// keeps its tasks in a heap too (`push_heap`).
    fn add_task(&mut self, task: Task) {
        if !task.is_implied() || self.unique.insert(task.clone()) {
            self.tasks.push(task);
            self.sift_up(self.tasks.len() - 1);
        }
    }

    /// Removes and returns the highest priority task. OpenUSD: `PopTask`.
    fn pop_task(&mut self) -> Option<Task> {
        if self.tasks.is_empty() {
            return None;
        }
        let task = self.tasks.swap_remove(0);
        self.sift_down(0);
        if task.is_implied() {
            self.unique.remove(&task);
        }
        Some(task)
    }

    /// Moves the task at `at` toward the top of the heap while it has a
    /// higher priority than its parent.
    fn sift_up(&mut self, mut at: usize) {
        while at > 0 {
            let parent = (at - 1) / 2;
            if !self.higher_priority(&self.tasks[at], &self.tasks[parent]) {
                break;
            }
            self.tasks.swap(at, parent);
            at = parent;
        }
    }

    /// Moves the task at `at` toward the bottom of the heap while a child
    /// has a higher priority.
    fn sift_down(&mut self, mut at: usize) {
        let len = self.tasks.len();
        loop {
            let mut best = at;
            for child in [2 * at + 1, 2 * at + 2] {
                if child < len && self.higher_priority(&self.tasks[child], &self.tasks[best]) {
                    best = child;
                }
            }
            if best == at {
                return;
            }
            self.tasks.swap(at, best);
            at = best;
        }
    }

    /// Whether `a` runs before `b`. OpenUSD: `Task::PriorityOrder`.
    fn higher_priority(&self, a: &Task, b: &Task) -> bool {
        if a.kind != b.kind {
            return a.kind < b.kind;
        }
        let graph = self.graph();
        let paths = self.paths();
        match a.kind {
            // Dynamic payloads read opinions of other sites: strength order.
            TaskKind::AncestralDynamicPayloads | TaskKind::NodeDynamicPayloads => {
                graph.cmp_nodes(paths, a.node, b.node).is_lt()
            }
            TaskKind::AncestralVariantAuthored
            | TaskKind::AncestralVariantFallback
            | TaskKind::VariantAuthored
            | TaskKind::VariantFallback => {
                if a.node != b.node {
                    graph.cmp_nodes(paths, a.node, b.node).is_lt()
                } else {
                    (&a.vset_path, a.vset_num) < (&b.vset_path, b.vset_num)
                }
            }
            TaskKind::AncestralVariantNoneFound | TaskKind::VariantNoneFound => {
                (a.node, &a.vset_path, a.vset_num) < (b.node, &b.vset_path, b.vset_num)
            }
            TaskKind::ImpliedClasses => {
                let is_ancestor = |x: NodeIndex, mut y: NodeIndex| {
                    while y != 0 && y != NONE {
                        if x == y {
                            return true;
                        }
                        y = graph.node(y).parent;
                    }
                    false
                };
                // Descendants before ancestors.
                if b.node > a.node && is_ancestor(a.node, b.node) {
                    return false;
                }
                if a.node > b.node && is_ancestor(b.node, a.node) {
                    return true;
                }
                graph.cmp_nodes(paths, a.node, b.node).is_lt()
            }
            _ => a.node < b.node,
        }
    }

    /// Promotes the variant tasks that found no authored selection to
    /// look for one again. A promoted task runs sooner: it moves up the
    /// heap. OpenUSD: `RetryVariantTasks`.
    fn retry_variant_tasks(&mut self) {
        for at in 0..self.tasks.len() {
            let task = &mut self.tasks[at];
            task.kind = match task.kind {
                TaskKind::VariantFallback | TaskKind::VariantNoneFound => TaskKind::VariantAuthored,
                TaskKind::AncestralVariantFallback | TaskKind::AncestralVariantNoneFound => {
                    TaskKind::AncestralVariantAuthored
                }
                _ => continue,
            };
            self.sift_up(at);
        }
    }

    fn run(&mut self, task: Task) {
        let node = task.node;
        match task.kind {
            TaskKind::NodeRelocations => self.eval_node_relocations(node),
            TaskKind::ImpliedRelocations => self.eval_implied_relocations(node),
            TaskKind::NodeReferences => self.eval_node_references(node),
            TaskKind::NodePayloads | TaskKind::NodeDynamicPayloads => {
                let path = self.graph().node(node).site.path.clone();
                self.eval_node_payloads(node, task.kind == TaskKind::NodeDynamicPayloads, path);
            }
            TaskKind::AncestralDynamicPayloads => self.eval_ancestral_dynamic_payloads(node),
            TaskKind::NodeInherits => self.eval_node_class_arcs(node, ArcKind::Inherits),
            TaskKind::NodeSpecializes => self.eval_node_class_arcs(node, ArcKind::Specializes),
            TaskKind::ImpliedClasses => self.eval_implied_classes(node),
            TaskKind::ImpliedSpecializes => self.eval_implied_specializes(node),
            TaskKind::AncestralVariantSets => self.eval_ancestral_variant_sets(node),
            TaskKind::VariantSets => self.eval_node_variant_sets(node),
            TaskKind::AncestralVariantAuthored | TaskKind::VariantAuthored => {
                let ancestral = task.kind == TaskKind::AncestralVariantAuthored;
                self.eval_authored_variant(
                    node,
                    task.vset_path.expect("variant task path"),
                    task.vset_name.expect("variant task set"),
                    task.vset_num,
                    ancestral,
                );
            }
            TaskKind::AncestralVariantFallback | TaskKind::VariantFallback => {
                let ancestral = task.kind == TaskKind::AncestralVariantFallback;
                self.eval_fallback_variant(
                    node,
                    task.vset_path.expect("variant task path"),
                    task.vset_name.expect("variant task set"),
                    task.vset_num,
                    ancestral,
                );
            }
            TaskKind::AncestralVariantNoneFound | TaskKind::VariantNoneFound => {}
            TaskKind::UnresolvedPrimPathError => self.eval_unresolved_prim_path_error(node),
        }
    }

    /// The tasks `node`'s specs could need. OpenUSD: `_ScanArcs`.
    fn scan_arcs(&self, node: NodeIndex) -> TaskSet {
        let mut tasks = TaskSet::ALL
            .without(TaskKind::NodeRelocations)
            .without(TaskKind::NodeInherits)
            .without(TaskKind::VariantSets)
            .without(TaskKind::NodeReferences)
            .without(TaskKind::NodePayloads)
            .without(TaskKind::NodeDynamicPayloads)
            .without(TaskKind::NodeSpecializes);
        let entry = self.graph().node(node);
        if !entry.can_contribute_specs() {
            return tasks;
        }
        let stack = self.ctx.stacks.get(entry.site.stack);
        if stack.has_relocates() {
            tasks = tasks.with(TaskKind::NodeRelocations);
        }
        if !entry.has_specs {
            return tasks;
        }
        for (_, _, spec) in site::specs(self.ctx.store, stack, &entry.site.path) {
            if !spec.inherits().is_empty() {
                tasks = tasks.with(TaskKind::NodeInherits);
            }
            if !spec.variant_set_names().is_empty() {
                tasks = tasks.with(TaskKind::VariantSets);
            }
            if !spec.references().is_empty() {
                tasks = tasks.with(TaskKind::NodeReferences);
            }
            if !spec.payloads().is_empty() {
                tasks = tasks.with(TaskKind::NodePayloads);
                if !self.ctx.dynamic_formats.is_empty() {
                    tasks = tasks.with(TaskKind::NodeDynamicPayloads);
                }
            }
            if !spec.specializes().is_empty() {
                tasks = tasks.with(TaskKind::NodeSpecializes);
            }
        }
        tasks
    }

    /// Whether a site above `node`'s authors variant sets, or payloads
    /// (which may be dynamic). OpenUSD: `_ScanAncestralArcs`.
    fn scan_ancestral_arcs(&mut self, node: NodeIndex) -> TaskSet {
        let mut tasks = TaskSet::ALL.without_ancestral_variants_and_dynamic_payloads();
        let entry = self.graph().node(node).clone();
        let paths = self.ctx.store.paths();
        let Some(mut path) = site_paths::parent(paths, &entry.site.path) else {
            return tasks;
        };
        if entry.restricted_depth != 0 {
            while site_paths::element_count(self.ctx.store.paths(), &path) != 0
                && site_paths::element_count(self.ctx.store.paths(), &path)
                    >= entry.restricted_depth as usize
            {
                match site_paths::parent(self.ctx.store.paths(), &path) {
                    Some(parent) => path = parent,
                    None => break,
                }
            }
        }
        let stack = self.ctx.stacks.get(entry.site.stack);
        loop {
            if self.ctx.store.paths().resolve(path.prim_path()).depth() == 0
                && !path.has_variant_selections()
            {
                return tasks;
            }
            for (_, _, spec) in site::specs(self.ctx.store, stack, &path) {
                if !spec.variant_set_names().is_empty() {
                    tasks = tasks.with(TaskKind::AncestralVariantSets);
                }
                if !spec.payloads().is_empty() && !self.ctx.dynamic_formats.is_empty() {
                    tasks = tasks.with(TaskKind::AncestralDynamicPayloads);
                }
            }
            if tasks.has(TaskKind::AncestralVariantSets)
                && (tasks.has(TaskKind::AncestralDynamicPayloads)
                    || self.ctx.dynamic_formats.is_empty())
            {
                return tasks;
            }
            match site_paths::parent(self.ctx.store.paths(), &path) {
                Some(parent) => path = parent,
                None => return tasks,
            }
        }
    }

    /// OpenUSD: `_AddTasksForNodeRecursively`.
    fn add_tasks_for_node_recursively(&mut self, node: NodeIndex, mut tasks: TaskSet) {
        let children = self.graph().child_list(node);
        for child in children {
            self.add_tasks_for_node_recursively(child, tasks);
        }
        let kind = self.graph().node(node).arc_kind;
        if tasks.has(TaskKind::UnresolvedPrimPathError)
            && kind != ArcKind::References
            && kind != ArcKind::Payloads
        {
            tasks = tasks.without(TaskKind::UnresolvedPrimPathError);
        }
        if tasks.has(TaskKind::ImpliedRelocations) && kind != ArcKind::Relocates {
            tasks = tasks.without(TaskKind::ImpliedRelocations);
        }
        tasks = TaskSet(tasks.0 & self.scan_arcs(node).0);
        if tasks.has(TaskKind::AncestralVariantSets)
            || tasks.has(TaskKind::AncestralDynamicPayloads)
        {
            tasks = TaskSet(tasks.0 & self.scan_ancestral_arcs(node).0);
        }
        for kind in [
            TaskKind::UnresolvedPrimPathError,
            TaskKind::VariantSets,
            TaskKind::NodeDynamicPayloads,
            TaskKind::AncestralDynamicPayloads,
            TaskKind::AncestralVariantSets,
            TaskKind::NodeSpecializes,
            TaskKind::NodeInherits,
            TaskKind::NodePayloads,
            TaskKind::NodeReferences,
            TaskKind::NodeRelocations,
            TaskKind::ImpliedRelocations,
        ] {
            if tasks.has(kind) {
                self.add_task(Task::new(kind, node));
            }
        }
    }

    fn add_tasks_for_root(&mut self) {
        let mut tasks = TaskSet::ALL
            .without_ancestral_variants_and_dynamic_payloads()
            .without(TaskKind::UnresolvedPrimPathError);
        if !self.evaluate_variants {
            tasks = tasks.without_variants_and_dynamic_payloads();
        }
        self.add_tasks_for_node_recursively(0, tasks);
    }

    /// OpenUSD: `AddTasksForNode`.
    fn add_tasks_for_node(&mut self, node: NodeIndex, mut tasks: TaskSet) {
        if tasks.has(TaskKind::ImpliedClasses) {
            let kind = self.graph().node(node).arc_kind;
            if is_class_based(kind) {
                let base = self.starting_node_for_implied_classes(node);
                if base != NONE {
                    self.add_task(Task::new(TaskKind::ImpliedClasses, base));
                }
            } else if self.has_class_based_child(node) {
                self.add_task(Task::new(TaskKind::ImpliedClasses, node));
            }
        }
        if tasks.has(TaskKind::ImpliedSpecializes)
            && self.evaluate_implied_specializes
            && self.has_specializes_in_subtree(node)
        {
            self.add_task(Task::new(TaskKind::ImpliedSpecializes, node));
        }
        if !self.evaluate_variants {
            tasks = tasks.without_variants_and_dynamic_payloads();
        }
        if self.previous_frame.is_some() {
            tasks = tasks.without(TaskKind::UnresolvedPrimPathError);
        }
        self.add_tasks_for_node_recursively(node, tasks);
    }

    fn has_specializes_in_subtree(&self, node: NodeIndex) -> bool {
        let graph = self.graph();
        graph
            .subtree(node)
            .into_iter()
            .any(|n| graph.node(n).arc_kind == ArcKind::Specializes)
    }

    /// OpenUSD: `_GetPropagatedSpecializesNode`.
    fn propagated_specializes_node(&self, node: NodeIndex) -> NodeIndex {
        let graph = self.graph();
        if graph.node(node).arc_kind != ArcKind::Specializes {
            return NONE;
        }
        let children = graph.child_list(0);
        for &child in children.iter().rev() {
            if graph.node(child).arc_kind != ArcKind::Specializes {
                break;
            }
            if graph.node(child).origin == node && graph.is_propagated_specializes(child) {
                return child;
            }
        }
        NONE
    }

    /// Whether the prim index being built, its own graph and those of the
    /// frames it is built for, holds [`Ctx::nodes_per_prim_index`] nodes:
    /// it adds no more arcs. The work of adding an arc grows with the
    /// index (`Graph::link_child` orders a new node among its siblings by
    /// walking the index), so an index growing without end, as an endless
    /// namespace's do, costs more than its nodes.
    ///
    /// Spec: AOUSD Core §10.6, §11; OpenUSD has no limit (see
    /// [`crate::composition_limits`]).
    fn is_full(&self) -> bool {
        let Some(limit) = self.ctx.nodes_per_prim_index else {
            return false;
        };
        let mut nodes = self.outputs.graph.nodes.len();
        let mut frame = self.previous_frame;
        while let Some(at) = frame {
            nodes += at.parent_graph.nodes.len();
            frame = at.previous;
        }
        nodes >= limit
    }

    /// OpenUSD: `_HasClassBasedChild`.
    fn has_class_based_child(&self, node: NodeIndex) -> bool {
        let propagated = self.propagated_specializes_node(node);
        let parent = if propagated != NONE { propagated } else { node };
        self.graph()
            .children(parent)
            .any(|child| is_class_based(self.graph().node(child).arc_kind))
    }

    /// OpenUSD: `_FindStartingNodeForImpliedClasses`.
    fn starting_node_for_implied_classes(&mut self, node: NodeIndex) -> NodeIndex {
        let mut start = node;
        while is_class_based(self.graph().node(start).arc_kind) {
            let (instance, class) = self
                .graph()
                .starting_node_of_class_hierarchy(self.paths(), start);
            start = instance;
            if is_class_based(self.graph().node(instance).arc_kind) {
                let paths = self.ctx.store.paths();
                let ancestral = self.outputs.graph.path_at_introduction(paths, instance);
                let class_path = self.outputs.graph.node(class).site.path.prim_path();
                if has_prefix(self.paths(), class_path, ancestral.prim_path()) {
                    break;
                }
            }
        }
        start
    }

    // ── Initial index ─────────────────────────────────────────────────

    /// OpenUSD: `_BuildInitialPrimIndexFromAncestor`.
    fn build_initial_from_ancestor(&mut self, site: &Site, root_contributes: bool) {
        let paths = self.ctx.store.paths();
        let parent_path = site_paths::parent(paths, &site.path).expect("not the root");
        let name = paths
            .resolve(site.path.prim_path())
            .leaf()
            .expect("prim name");
        let ancestor_instanceable;
        let cached = (self.previous_frame.is_none()
            && self.evaluate_implied_specializes
            && site.stack == self.ctx.stage_stack)
            .then_some(self.parent)
            .flatten();
        if let Some(parent) = cached.filter(|_| !parent_path.has_variant_selections()) {
            debug_assert!(
                parent.node(0).site.path.prim_path() == parent_path.prim_path(),
                "the parent's index"
            );
            self.outputs.graph = parent.for_child();
            ancestor_instanceable = parent.instanceable;
        } else {
            let parent_site = Site {
                stack: site.stack,
                path: parent_path,
            };
            let root_site = self.root_site.clone();
            build_prim_index(
                self.ctx,
                &parent_site,
                &root_site,
                self.evaluate_implied_specializes,
                self.evaluate_variants,
                true,
                self.previous_frame,
                None,
                self.outputs,
            );
            ancestor_instanceable = self.prim_index_is_instanceable();
        }
        if ancestor_instanceable {
            self.disable_non_instanceable_nodes();
        }
        let paths = self.ctx.store.paths();
        self.outputs.graph.append_child_name(paths, name);
        self.outputs.graph.has_payloads = false;
        self.outputs.payload_state = PayloadState::NoPayload;
        self.convert_nodes_for_child();
        if !root_contributes {
            self.outputs
                .graph
                .set_inert(self.ctx.store.paths(), 0, true);
        }
    }

    /// OpenUSD: `_ConvertNodeForChild`.
    fn convert_nodes_for_child(&mut self) {
        for at in 0..self.outputs.graph.len() {
            let index = super::graph::node_index(at);
            let entry = self.outputs.graph.node(index);
            // The root rechecks too: a spec may be authored without specs
            // for its ancestors.
            if entry.has_specs || at == 0 {
                let stack = self.ctx.stacks.get(entry.site.stack);
                let has = site::has_specs(self.ctx.store, stack, &entry.site.path);
                self.outputs.graph.node_mut(index).has_specs = has;
            }
            let entry = self.outputs.graph.node(index);
            if !entry.inert && entry.has_specs && !entry.has_value_clips {
                let stack = self.ctx.stacks.get(entry.site.stack);
                let clips = site::has_value_clips(self.ctx.store, stack, &entry.site.path);
                self.outputs.graph.node_mut(index).has_value_clips = clips;
            }
            if at != 0 {
                let node = self.outputs.graph.node_mut(index);
                node.due_to_ancestor = true;
                node.transitive_direct = false;
                node.transitive_ancestral = true;
            }
        }
    }

    /// Whether `node` contributes to an instance. OpenUSD:
    /// `Pcp_ChildNodeIsInstanceable`.
    fn child_node_is_instanceable(&self, node: NodeIndex) -> bool {
        let entry = self.graph().node(node);
        entry.transitive_direct && entry.can_contribute_specs() && entry.has_specs
    }

    /// Marks the nodes of an instance's index that cannot have opinions
    /// about its descendants inert. OpenUSD:
    /// `Pcp_DisableNonInstanceableNodesVisitor`.
    fn disable_non_instanceable_nodes(&mut self) {
        self.outputs
            .graph
            .set_inert(self.ctx.store.paths(), 0, true);
        let mut pending = self.graph().child_list(0);
        while let Some(node) = pending.pop() {
            if self.graph().node(node).culled {
                continue;
            }
            if self.child_node_is_instanceable(node) {
                continue;
            }
            self.outputs
                .graph
                .set_inert(self.ctx.store.paths(), node, true);
            pending.extend(self.graph().child_list(node));
        }
    }

    /// OpenUSD: `Pcp_PrimIndexIsInstanceable`.
    fn prim_index_is_instanceable(&self) -> bool {
        is_instanceable(self.ctx.store, &self.ctx.stacks, self.graph())
    }

    /// OpenUSD: `_ElidePrimIndexIfProhibited`.
    fn elide_if_prohibited(&mut self) -> bool {
        let graph = self.graph();
        let mut found = None;
        let mut stack = alloc::vec![0];
        while let Some(node) = stack.pop() {
            let entry = graph.node(node);
            if entry.culled {
                continue;
            }
            stack.extend(graph.children(node));
            if entry.inert {
                continue;
            }
            let layer_stack = self.ctx.stacks.get(entry.site.stack);
            if !layer_stack.has_relocates() {
                continue;
            }
            if layer_stack
                .relocations
                .target_of(entry.site.path.prim_path())
                .is_some()
                && !entry.site.path.has_variant_selections()
            {
                found = Some(node);
                break;
            }
        }
        let Some(node) = found else {
            return false;
        };
        if let Some(frame) = self.previous_frame {
            let entry = self.graph().node(node);
            let error = CompositionError::ArcToProhibitedChild(ArcToProhibitedChild {
                prim: self.originating_prim(),
                arc: frame.arc_kind,
                layer_stack: self
                    .ctx
                    .stacks
                    .get(frame.requested_site.stack)
                    .identifier
                    .root,
                target: frame.requested_site.path.prim_path(),
                relocation_source: entry.site.path.prim_path(),
            });
            self.record_error(error);
        }
        self.outputs
            .graph
            .set_inert(self.ctx.store.paths(), 0, true);
        for child in self.graph().child_list(0) {
            self.elide_subtree(child, true);
        }
        true
    }

    // ── Adding arcs ────────────────────────────────────────────────────

    /// OpenUSD: `_AddArc`.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors `_AddArc`, whose inputs are separate"
    )]
    fn add_arc(
        &mut self,
        arc_kind: ArcKind,
        parent: NodeIndex,
        origin: NodeIndex,
        site: Site,
        map_to_parent: MapFunction,
        sibling_num: u16,
        namespace_depth: u16,
        mut opts: ArcOptions,
    ) -> NodeIndex {
        if let Some(error) = self.check_for_cycle(parent, origin, arc_kind, &site) {
            self.record_error(error);
            return NONE;
        }
        if self.is_full() {
            self.outputs.full = true;
            return NONE;
        }
        if let Some(frame) = self.previous_frame {
            opts.skip_duplicate_nodes |= frame.skip_duplicate_nodes;
        }
        if opts.skip_duplicate_nodes && self.has_duplicate_node(parent, &site) {
            return NONE;
        }
        let new_node;
        if !opts.include_ancestral_opinions {
            let map_to_root = {
                let paths = self.ctx.store.paths();
                self.outputs
                    .graph
                    .node(parent)
                    .map_to_root
                    .clone()
                    .compose(paths, &map_to_parent)
            };
            let stack = self.ctx.stacks.get(site.stack);
            let has_specs = site::has_specs(self.ctx.store, stack, &site.path);
            let node = Node {
                arc_kind,
                parent,
                origin,
                first_child: NONE,
                last_child: NONE,
                prev_sibling: NONE,
                next_sibling: NONE,
                site: site.clone(),
                map_to_parent: map_to_parent.clone(),
                map_to_root,
                namespace_depth,
                sibling_num,
                inert: false,
                culled: false,
                has_specs,
                has_value_clips: false,
                restricted_depth: 0,
                due_to_ancestor: false,
                transitive_direct: false,
                transitive_ancestral: false,
                from_parent_prim: false,
                authoring_layer: None,
            };
            let index = self.outputs.graph.push_node(node);
            let paths = self.ctx.store.paths();
            self.outputs.graph.link_child(paths, parent, index);
            if !opts.direct_node_contributes {
                self.outputs
                    .graph
                    .set_inert(self.ctx.store.paths(), index, true);
                self.outputs.graph.node_mut(index).restricted_depth = 1;
            }
            let entry = self.outputs.graph.node(index);
            if !entry.inert && entry.has_specs {
                let stack = self.ctx.stacks.get(site.stack);
                let clips = site::has_value_clips(self.ctx.store, stack, &site.path);
                self.outputs.graph.node_mut(index).has_value_clips = clips;
            }
            new_node = index;
        } else {
            let mut child_outputs = Outputs::default();
            {
                let frame = Frame {
                    requested_site: site.clone(),
                    parent_graph: &self.outputs.graph,
                    parent_node: parent,
                    arc_kind,
                    arc_map_to_parent: &map_to_parent,
                    previous: self.previous_frame,
                    skip_duplicate_nodes: opts.skip_duplicate_nodes,
                };
                let root_site = self.root_site.clone();
                build_prim_index(
                    self.ctx,
                    &site,
                    &root_site,
                    false,
                    false,
                    opts.direct_node_contributes,
                    Some(&frame),
                    None,
                    &mut child_outputs,
                );
            }
            let subgraph = core::mem::take(&mut child_outputs.graph);
            let has_payloads = subgraph.has_payloads;
            let paths = self.ctx.store.paths();
            new_node = self.outputs.graph.insert_subgraph(
                paths,
                parent,
                subgraph,
                SubgraphArc {
                    arc_kind,
                    origin,
                    map_to_parent: map_to_parent.clone(),
                    namespace_depth,
                    sibling_num,
                },
            );
            if has_payloads {
                self.outputs.graph.has_payloads = true;
            }
            self.outputs.append(child_outputs);
        }

        if opts.copy_ancestor_flag_from_origin {
            let origin_entry = self.graph().node(origin).clone();
            let node = self.outputs.graph.node_mut(new_node);
            node.due_to_ancestor = origin_entry.due_to_ancestor;
            node.transitive_direct = origin_entry.transitive_direct;
            node.transitive_ancestral = origin_entry.transitive_ancestral;
        } else {
            let ancestral = self.graph().node(parent).transitive_ancestral;
            let node = self.outputs.graph.node_mut(new_node);
            node.transitive_direct = true;
            node.transitive_ancestral = ancestral;
        }
        if opts.include_ancestral_opinions && self.previous_frame.is_none() {
            let (direct, ancestral) = {
                let node = self.graph().node(new_node);
                (node.transitive_direct, node.transitive_ancestral)
            };
            for n in self.graph().subtree(new_node) {
                let node = self.outputs.graph.node_mut(n);
                node.transitive_direct |= direct;
                node.transitive_ancestral |= ancestral;
            }
        }

        let mut tasks = opts.tasks;
        if opts.include_ancestral_opinions {
            tasks = tasks.without_expressed_arcs();
        }
        if !(self.evaluate_variants && opts.include_ancestral_opinions) {
            tasks = tasks.without_ancestral_variants_and_dynamic_payloads();
        }
        if !opts.include_ancestral_opinions {
            tasks = tasks.without(TaskKind::ImpliedSpecializes);
        }
        self.add_tasks_for_node(new_node, tasks);

        // A placeholder for an unresolved default target contributes
        // nothing but records the dependency.
        let root = self.ctx.root_path;
        let entry = self.graph().node(new_node);
        if entry.site.path.prim_path() == root && !entry.site.path.has_variant_selections() {
            self.outputs
                .graph
                .inert_subtree(self.ctx.store.paths(), new_node);
        }
        new_node
    }

    /// Whether a node of the graph being built (or of a graph it will be
    /// grafted into) already uses `site`. OpenUSD: the duplicate check of
    /// `_AddArc`.
    fn has_duplicate_node(&mut self, parent: NodeIndex, site: &Site) -> bool {
        let mut site_in_graph = site.clone();
        let mut cursor = FrameCursor::new(&self.outputs.graph, parent, self.previous_frame);
        while cursor.is_valid() {
            if cursor.graph.node_using_site(&site_in_graph).is_some() {
                return true;
            }
            if let Some(frame) = cursor.frame {
                let requested = &frame.requested_site.path;
                let current = &cursor.graph.node(0).site.path;
                let paths = self.ctx.store.paths();
                site_in_graph.path =
                    replace_spec_prefix(paths, requested, current, &site_in_graph.path);
            }
            cursor.next_frame();
        }
        false
    }

    /// OpenUSD: `_CheckForCycle`.
    fn check_for_cycle(
        &mut self,
        parent: NodeIndex,
        origin: NodeIndex,
        arc_kind: ArcKind,
        site: &Site,
    ) -> Option<CompositionError> {
        let implied = |kind: ArcKind| is_class_based(kind) && parent != origin;
        if implied(arc_kind) {
            let mut cursor = FrameCursor::new(&self.outputs.graph, parent, self.previous_frame);
            while cursor.is_valid() && cursor.arc_kind().is_some_and(implied) {
                cursor.next();
            }
            if cursor.is_valid() && cursor.arc_kind() == Some(ArcKind::Relocates) {
                return None;
            }
        }
        if arc_kind == ArcKind::Variants {
            return None;
        }
        let mut found = false;
        let mut child_site = site.clone();
        let mut cursor = FrameCursor::new(&self.outputs.graph, parent, self.previous_frame);
        while cursor.is_valid() {
            // Any node from the cursor up to its graph's root.
            let mut node = cursor.node;
            while node != NONE {
                let entry = cursor.graph.node(node);
                if entry.site.stack == child_site.stack
                    && (has_spec_prefix(self.ctx.store.paths(), &entry.site.path, &child_site.path)
                        || has_spec_prefix(
                            self.ctx.store.paths(),
                            &child_site.path,
                            &entry.site.path,
                        ))
                {
                    found = true;
                    break;
                }
                node = entry.parent;
            }
            if found {
                break;
            }
            if let Some(frame) = cursor.frame {
                let requested = &frame.requested_site.path;
                let current = &cursor.graph.node(0).site.path;
                let paths = self.ctx.store.paths();
                child_site.path = if *current == child_site.path {
                    requested.clone()
                } else {
                    replace_spec_prefix(paths, requested, current, &child_site.path)
                };
            }
            cursor.next_frame();
        }
        if !found {
            return None;
        }
        let mut sites = Vec::new();
        let mut cursor = FrameCursor::new(&self.outputs.graph, parent, self.previous_frame);
        while cursor.is_valid() {
            let entry = cursor.entry();
            // A variant branch stays at its host's prim path; the chain
            // lists prim paths.
            if cursor.arc_kind() == Some(ArcKind::Variants) {
                cursor.next();
                continue;
            }
            sites.push(ArcCycleSite {
                layer_stack: self.ctx.stacks.get(entry.site.stack).identifier.root,
                path: entry.site.path.prim_path(),
                arc: cursor.arc_kind().filter(|kind| *kind != ArcKind::Local),
            });
            cursor.next();
        }
        sites.reverse();
        sites.push(ArcCycleSite {
            layer_stack: self.ctx.stacks.get(site.stack).identifier.root,
            path: site.path.prim_path(),
            arc: Some(arc_kind),
        });
        Some(CompositionError::ArcCycle(ArcCycle {
            prim: self.originating_prim(),
            sites,
        }))
    }

    /// The map function of an arc from a node at `target_path` (in
    /// `target_stack`) to `source`, with `offset`, composed with the
    /// relocations of the target stack at that path. OpenUSD:
    /// `_CreateMapExpressionForArc`.
    fn map_for_arc(
        &mut self,
        source: PathId,
        target_path: &SpecPath,
        target_stack: StackId,
        offset: LayerOffset,
    ) -> MapFunction {
        let target = target_path.prim_path();
        let paths = self.ctx.store.paths();
        let arc = MapFunction::new(paths, [(source, Some(target))], offset);
        let stack = self.ctx.stacks.get(target_stack);
        if !stack.has_relocates() {
            return arc;
        }
        let relocates = relocates_map_at(self.ctx.store, stack, target);
        relocates.compose(self.ctx.store.paths(), &arc)
    }

    // ── References and payloads ───────────────────────────────────────

    /// OpenUSD: `_EvalNodeReferences`.
    fn eval_node_references(&mut self, node: NodeIndex) {
        let entry = self.graph().node(node).clone();
        if !entry.can_contribute_specs() {
            return;
        }
        let stack = self.ctx.stacks.get(entry.site.stack);
        let scope = self.scope_for(entry.site.stack);
        let arcs = site::references(
            self.ctx.store,
            stack,
            &entry.site.path,
            false,
            scope.as_ref(),
            None,
        );
        if let Some(scope) = scope {
            self.absorb_scope(scope, true);
        }
        for (arc_num, arc) in arcs.into_iter().enumerate() {
            let arc_num = u16::try_from(arc_num).unwrap_or(u16::MAX);
            let intro = entry.site.path.clone();
            self.eval_ref_or_payload(node, &entry, ArcKind::References, arc, arc_num, &intro);
        }
    }

    /// Evaluates the payloads authored at `path_at_intro` of `node`'s layer
    /// stack (`node`'s own site, or an ancestor of it), those to dynamic
    /// file formats when `dynamic`, the rest otherwise.
    ///
    /// Static payloads are evaluated with references; the dynamic ones
    /// after variants, once the opinions their arguments read are in the
    /// index. A payload whose inclusion no stage path governs (an ancestral
    /// one of a subroot arc's target) is always included, and its node's
    /// dynamic payloads are then evaluated within the recursive index too.
    ///
    /// Spec: AOUSD Core §10.3.2.2 (payloads). OpenUSD: `_EvalNodePayloads`.
    fn eval_node_payloads(&mut self, node: NodeIndex, dynamic: bool, path_at_intro: SpecPath) {
        let entry = self.graph().node(node).clone();
        if !entry.can_contribute_specs() {
            return;
        }
        let stack = self.ctx.stacks.get(entry.site.stack);
        let scope = self.scope_for(entry.site.stack);
        let arcs = site::references(
            self.ctx.store,
            stack,
            &path_at_intro,
            true,
            scope.as_ref(),
            Some(self.ctx.dynamic_formats),
        );
        // Both passes compose the site's payloads; one reports what their
        // expressions found.
        if let Some(scope) = scope {
            self.absorb_scope(scope, !dynamic);
        }
        if arcs.is_empty() {
            return;
        }
        if path_at_intro == entry.site.path {
            self.outputs.graph.has_payloads = true;
        }
        // Arc numbers count every payload of the site, of either pass.
        let arcs: Vec<(u16, site::SiteArc)> = arcs
            .into_iter()
            .enumerate()
            .map(|(arc_num, arc)| (u16::try_from(arc_num).unwrap_or(u16::MAX), arc))
            .filter(|(_, arc)| self.is_dynamic_payload(&arc.arc) == dynamic)
            .collect();
        match self.payload_inclusion_path(node, &path_at_intro) {
            None => {
                self.outputs.payload_state = PayloadState::Included;
                for (arc_num, arc) in arcs {
                    let intro = entry.site.path.clone();
                    self.eval_ref_or_payload(node, &entry, ArcKind::Payloads, arc, arc_num, &intro);
                }
                if !dynamic && !self.ctx.dynamic_formats.is_empty() {
                    self.add_task(Task::new(TaskKind::NodeDynamicPayloads, node));
                }
                return;
            }
            Some(path) => {
                let include = (self.ctx.include_payload)(self.ctx.store.paths(), path);
                self.outputs.payload_state = if include {
                    PayloadState::Included
                } else {
                    PayloadState::Excluded
                };
                if !include {
                    return;
                }
            }
        }
        for (arc_num, arc) in arcs {
            self.eval_ref_or_payload(
                node,
                &entry,
                ArcKind::Payloads,
                arc,
                arc_num,
                &path_at_intro,
            );
        }
    }

    /// Evaluates the dynamic payloads authored at the ancestral sites of
    /// `node`, whose recursive index skipped them.
    /// OpenUSD: `_EvalNodeAncestralDynamicPayloads`.
    fn eval_ancestral_dynamic_payloads(&mut self, node: NodeIndex) {
        let start = self.graph().node(node).site.path.clone();
        let mut path = site_paths::parent(self.ctx.store.paths(), &start);
        while let Some(current) = path {
            let paths = self.ctx.store.paths();
            if paths.resolve(current.prim_path()).depth() == 0 && !current.has_variant_selections()
            {
                break;
            }
            if self.node_can_contribute_ancestral_opinions(node, &current) {
                self.eval_node_payloads(node, true, current.clone());
            }
            path = site_paths::parent(self.ctx.store.paths(), &current);
        }
    }

    /// Whether `arc` is a payload to a registered dynamic file format.
    /// OpenUSD: `_GetDynamicFileFormat`.
    fn is_dynamic_payload(&self, arc: &crate::doc::Reference) -> bool {
        arc.asset
            .as_deref()
            .is_some_and(|asset| self.ctx.dynamic_formats.for_asset(asset).is_some())
    }

    /// The identifier of the layer the payload to `asset`, the `arc_num`th
    /// of `parent`'s introduced at `path_at_intro`, targets: `asset` with
    /// `format`'s arguments composed at this point of indexing over those
    /// it carries. Records what they were composed from.
    ///
    /// OpenUSD: `_ComposeFieldsForFileFormatArguments`, and the arguments
    /// `SdfLayer::FindOrOpen` merges over the identifier's own.
    fn compose_dynamic_args(
        &mut self,
        parent: NodeIndex,
        path_at_intro: &SpecPath,
        arc_num: u16,
        format: &Arc<dyn DynamicFileFormat>,
        asset: &str,
    ) -> String {
        let walk = Walk {
            store: self.ctx.store,
            stacks: &self.ctx.stacks,
            schemas: self.ctx.schemas,
            graph: &self.outputs.graph,
            parent,
            path_in_node: path_at_intro.clone(),
            arc_num,
            previous_frame: self.previous_frame,
        };
        let mut context = DynamicFormatContext::new(walk);
        let composed = format.compose_args(asset, &mut context);
        let dependency = context.into_dependency(Arc::clone(format), composed.dependency);
        self.outputs.dynamic.append(dependency);
        let (path, mut args) = FormatArgs::split(asset)
            .unwrap_or_else(|_| (FormatArgs::strip(asset), FormatArgs::new()));
        args.extend(composed.args);
        args.identifier(path)
    }

    /// OpenUSD: `_EvalRefOrPayloadArcs`, for one arc authored at
    /// `path_at_intro` of `node`'s layer stack.
    fn eval_ref_or_payload(
        &mut self,
        node: NodeIndex,
        entry: &Node,
        kind: ArcKind,
        arc: site::SiteArc,
        arc_num: u16,
        path_at_intro: &SpecPath,
    ) {
        let reference = &arc.arc;
        let prim = self.originating_prim();
        let used_by = if kind == ArcKind::Payloads {
            AssetUse::Payload
        } else {
            AssetUse::Reference
        };
        let is_internal = reference.asset.is_none();
        let mut offset = reference.layer_offset;
        let stack_id;
        if is_internal {
            stack_id = entry.site.stack;
        } else {
            if reference.is_expression() {
                return;
            }
            let asset = reference.asset.as_deref().unwrap_or_default();
            let dynamic = (kind == ArcKind::Payloads)
                .then(|| self.ctx.dynamic_formats.shared_for_asset(asset).cloned())
                .flatten();
            let target_layer = if let Some(format) = dynamic {
                // The layer the composed arguments name, anchored to the
                // layer authoring the payload.
                let anchor = if reference.deferred {
                    reference.layer
                } else {
                    arc.source_layer
                };
                let identifier =
                    self.compose_dynamic_args(node, path_at_intro, arc_num, &format, asset);
                match self.ctx.store.asset_availability(anchor, &identifier) {
                    AssetAvailability::Loaded(layer) => {
                        self.outputs.dynamic.targets.insert(layer);
                        layer
                    }
                    AssetAvailability::Missing => {
                        self.record_error(CompositionError::UnresolvedAsset(UnresolvedAsset {
                            prim,
                            arc: kind,
                            asset_path: identifier,
                            introducing_layer: arc.source_layer,
                            introducing_spec: path_at_intro.clone(),
                        }));
                        return;
                    }
                    AssetAvailability::Unavailable => {
                        self.outputs.asset_requests.push(AssetRequest {
                            anchor,
                            asset_path: identifier,
                            prim,
                            used_by,
                        });
                        return;
                    }
                }
            } else {
                if reference.deferred {
                    self.outputs.asset_requests.push(AssetRequest {
                        anchor: reference.layer,
                        asset_path: reference.asset.clone().unwrap_or_default(),
                        prim,
                        used_by,
                    });
                    return;
                }
                if reference.is_unresolved() {
                    // The arc contributes nothing; an asset that is not
                    // found does not fail the stage (AOUSD Core §10.3.2.1,
                    // §10.6; OpenUSD `PcpErrorInvalidAssetPath`).
                    self.record_error(CompositionError::UnresolvedAsset(UnresolvedAsset {
                        prim,
                        arc: kind,
                        asset_path: reference.asset.clone().unwrap_or_default(),
                        introducing_layer: arc.source_layer,
                        introducing_spec: path_at_intro.clone(),
                    }));
                    return;
                }
                reference.layer
            };
            offset = arc.source_offset.compose(offset);
            let identifier = LayerStackIdentifier {
                root: target_layer,
                session: None,
            };
            let found =
                self.ctx
                    .stacks
                    .find_or_compute(self.ctx.store, identifier, Some(entry.site.stack));
            stack_id = match found {
                Ok(stack_id) => stack_id,
                // The arc's layer stack is incomplete: the index waits for
                // it, and is built again whole once it loads.
                Err(pending) => {
                    for layer in pending {
                        if !self.outputs.pending.contains(&layer) {
                            self.outputs.pending.push(layer);
                        }
                    }
                    return;
                }
            };
            self.outputs.errors.push(Recorded::StackErrors {
                stack: stack_id,
                prim,
            });
            let tokens = self.ctx.store.tokens();
            let source_rate = self
                .ctx
                .store
                .layer(arc.source_layer)
                .map_or(24.0, |layer| layer.time_codes_per_second(tokens));
            let dest_rate = identifier.time_codes_per_second(self.ctx.store);
            if source_rate != dest_rate {
                offset.scale = offset.scale * source_rate / dest_rate;
            }
        }
        self.outputs.arc_layers.push(arc.source_layer);
        let stack_root = self.ctx.stacks.get(stack_id).identifier.root;
        let mut direct_contributes = true;
        let target = match reference.target {
            ReferenceTarget::Prim(path) => path,
            ReferenceTarget::DefaultPrim => {
                self.outputs.default_prim_layers.push(stack_root);
                let names: Option<Vec<String>> =
                    self.ctx.store.layer(stack_root).and_then(|layer| {
                        let tokens = self.ctx.store.tokens();
                        let names =
                            crate::doc::default_prim_names(tokens.resolve(layer.default_prim?))?;
                        Some(names.into_iter().map(String::from).collect())
                    });
                let path = names.filter(|names| !names.is_empty()).map(|names| {
                    let segments: Vec<TokenId> = names
                        .iter()
                        .map(|name| self.ctx.store.tokens().intern(name))
                        .collect();
                    segments
                });
                match path {
                    Some(segments) => self.ctx.store.paths().intern_segments(&segments),
                    None => {
                        self.record_error(CompositionError::UnresolvedDefaultPrim(
                            UnresolvedDefaultPrim {
                                prim,
                                arc: kind,
                                layer: stack_root,
                                path: None,
                            },
                        ));
                        direct_contributes = false;
                        self.ctx.root_path
                    }
                }
            }
        };
        let intro_path = path_at_intro.clone();
        let mut map = self.map_for_arc(target, &intro_path, entry.site.stack, offset);
        if is_internal {
            map = map.with_root_identity(self.ctx.store.paths());
        }
        // An arc introduced at an ancestral site targets the matching
        // descendant of its target.
        let site_path = if intro_path == entry.site.path {
            SpecPath::from_prim_path(target)
        } else {
            site_paths::replace_prefix(
                self.ctx.store.paths(),
                &entry.site.path,
                &intro_path,
                &SpecPath::from_prim_path(target),
            )
        };
        let root = self.ctx.root_path;
        let site_prim = site_path.prim_path();
        let is_root_prim =
            site_prim != root && self.ctx.store.paths().resolve(site_prim).depth() == 1;
        let opts = ArcOptions {
            direct_node_contributes: direct_contributes,
            include_ancestral_opinions: site_prim != root && !is_root_prim,
            ..ArcOptions::default()
        };
        let namespace_depth = non_variant_depth(self.ctx.store.paths(), &intro_path);
        let new_node = self.add_arc(
            kind,
            node,
            node,
            Site {
                stack: stack_id,
                path: site_path,
            },
            map,
            arc_num,
            namespace_depth,
            opts,
        );
        if new_node != NONE {
            self.outputs.graph.node_mut(new_node).authoring_layer = Some(arc.source_layer);
        }
    }

    /// The stage path whose load state includes the payloads of `node`, or
    /// `None` when no stage path maps to it (the payload is then always
    /// included). OpenUSD: `MapNodePathToPayloadInclusionPath`.
    fn payload_inclusion_path(&mut self, node: NodeIndex, path: &SpecPath) -> Option<PathId> {
        let mut p =
            map_to_root_for_payload(self.ctx.store, &self.outputs.graph, node, path.prim_path())?;
        let mut frame = self.previous_frame;
        while let Some(f) = frame {
            p = map_for_payload_inclusion(
                self.ctx.store.paths(),
                f.arc_map_to_parent,
                f.arc_kind,
                p,
            )?;
            p = map_to_root_for_payload(self.ctx.store, f.parent_graph, f.parent_node, p)?;
            frame = f.previous;
        }
        Some(p)
    }

    /// OpenUSD: `_EvalUnresolvedPrimPathError`.
    fn eval_unresolved_prim_path_error(&mut self, node: NodeIndex) {
        let paths = self.ctx.store.paths();
        let at_intro = self.outputs.graph.path_at_introduction(paths, node);
        // The placeholder of an unresolved default target, at the
        // pseudo-root, was reported when it was added; every layer has a
        // pseudo-root.
        if at_intro.prim_path() == self.ctx.root_path && !at_intro.has_variant_selections() {
            return;
        }
        let below_intro = self.graph().depth_below_introduction(self.paths(), node);
        let mut exists = false;
        for n in self.graph().subtree(node) {
            let found = if below_intro == 0 {
                self.graph().node(n).has_specs
            } else {
                let path = self
                    .outputs
                    .graph
                    .path_at_introduction(self.ctx.store.paths(), n);
                let stack = self.ctx.stacks.get(self.graph().node(n).site.stack);
                site::has_specs(self.ctx.store, stack, &path)
            };
            if found {
                exists = true;
                break;
            }
        }
        if exists {
            return;
        }
        let entry = self.graph().node(node).clone();
        let parent = self.graph().node(entry.parent).clone();
        let prim = self.originating_prim();
        // The arc is authored at the parent's site at the depth it was
        // introduced: an ancestral arc at an ancestor of the parent's site.
        let introducing_site = self.outputs.graph.intro_path(self.ctx.store.paths(), node);
        let parent_stack = self.ctx.stacks.get(parent.site.stack);
        let scope = self.scope_for(parent.site.stack);
        let arcs = site::references(
            self.ctx.store,
            parent_stack,
            &introducing_site,
            entry.arc_kind == ArcKind::Payloads,
            scope.as_ref(),
            (entry.arc_kind == ArcKind::Payloads).then_some(self.ctx.dynamic_formats),
        );
        let source = arcs.get(entry.sibling_num as usize).cloned();
        let target_layer = self.ctx.stacks.get(entry.site.stack).identifier.root;
        let error = match source.as_ref().map(|arc| arc.arc.target.clone()) {
            Some(ReferenceTarget::DefaultPrim) => {
                CompositionError::UnresolvedDefaultPrim(UnresolvedDefaultPrim {
                    prim,
                    arc: entry.arc_kind,
                    layer: target_layer,
                    path: Some(at_intro.prim_path()),
                })
            }
            _ => CompositionError::UnresolvedPrimPath(UnresolvedPrimPath {
                prim,
                arc: entry.arc_kind,
                layer: target_layer,
                path: at_intro.prim_path(),
                introducing_layer: source.as_ref().map(|arc| arc.source_layer),
                introducing_spec: introducing_site.clone(),
            }),
        };
        self.record_error(error);
    }

    /// The scope asset path expressions authored in `stack` evaluate in.
    fn scope_for(&self, stack: StackId) -> Option<ExpressionScope> {
        let stack = self.ctx.stacks.get(stack);
        let mut chain: Vec<LayerStackIdentifier> = stack.chain.to_vec();
        if chain.last() != Some(&stack.identifier) {
            chain.push(stack.identifier);
        }
        Some(ExpressionScope::new(chain))
    }

    /// Keeps the variables `scope` read, and, when `report`, the errors
    /// its expressions found.
    fn absorb_scope(&mut self, scope: ExpressionScope, report: bool) {
        let prim = self.originating_prim();
        let findings = scope.into_findings();
        self.outputs.reads.extend(findings.reads);
        if !report {
            return;
        }
        for (context, layer, expression, error) in findings.errors {
            self.record_error(crate::expression_variables::expression_error(
                context,
                layer,
                Some(prim),
                &expression,
                error,
            ));
        }
    }

    // ── Relocations ────────────────────────────────────────────────────

    /// OpenUSD: `_ElideSubtree`.
    fn elide_subtree(&mut self, node: NodeIndex, cull: bool) {
        for n in self.graph().subtree(node) {
            let paths = self.ctx.store.paths();
            if cull {
                self.outputs.graph.set_culled(paths, n);
            } else {
                self.outputs.graph.set_inert(paths, n, true);
            }
            self.outputs.graph.node_mut(n).restricted_depth = 1;
        }
    }

    /// OpenUSD: `_ElideRelocatedSubtrees`.
    fn elide_relocated_subtrees(&mut self, node: NodeIndex) {
        for child in self.graph().child_list(node) {
            let entry = self.graph().node(child);
            if entry.arc_kind == ArcKind::Relocates {
                continue;
            }
            if entry.can_contribute_specs() {
                let stack = self.ctx.stacks.get(entry.site.stack);
                if !entry.site.path.has_variant_selections()
                    && stack
                        .relocations
                        .target_of(entry.site.path.prim_path())
                        .is_some()
                {
                    self.elide_subtree(child, true);
                    continue;
                }
            }
            self.elide_relocated_subtrees(child);
        }
    }

    /// OpenUSD: `_EvalNodeRelocations`.
    fn eval_node_relocations(&mut self, node: NodeIndex) {
        let entry = self.graph().node(node).clone();
        if !entry.can_contribute_specs() || entry.site.path.has_variant_selections() {
            return;
        }
        let stack = self.ctx.stacks.get(entry.site.stack);
        let Some(source) = stack.relocations.source_of(entry.site.path.prim_path()) else {
            return;
        };
        for child in self.graph().child_list(node) {
            match self.graph().node(child).arc_kind {
                ArcKind::Variants | ArcKind::Local => continue,
                _ => self.elide_subtree(child, true),
            }
        }
        let opts = ArcOptions {
            direct_node_contributes: false,
            include_ancestral_opinions: true,
            ..ArcOptions::default()
        };
        let depth = non_variant_depth(self.paths(), &entry.site.path);
        let new_node = self.add_arc(
            ArcKind::Relocates,
            node,
            node,
            Site {
                stack: entry.site.stack,
                path: SpecPath::from_prim_path(source),
            },
            MapFunction::identity(),
            0,
            depth,
            opts,
        );
        if new_node == NONE {
            return;
        }
        let prim = self.originating_prim();
        let stack = self.ctx.stacks.get(entry.site.stack);
        let new_site = self.graph().node(new_node).site.path.clone();
        let errors: Vec<CompositionError> = site::specs(self.ctx.store, stack, &new_site)
            .map(|(_, layer, _)| {
                CompositionError::OpinionAtRelocationSource(OpinionAtRelocationSource {
                    prim,
                    layer,
                    path: new_site.prim_path(),
                })
            })
            .collect();
        for error in errors {
            self.record_error(error);
        }
        self.elide_relocated_subtrees(new_node);
    }

    /// OpenUSD: `_EvalImpliedRelocations`.
    fn eval_implied_relocations(&mut self, node: NodeIndex) {
        let entry = self.graph().node(node).clone();
        if entry.arc_kind != ArcKind::Relocates || entry.due_to_ancestor {
            return;
        }
        let parent = entry.parent;
        if parent == NONE {
            return;
        }
        let grandparent = self.graph().node(parent).parent;
        if grandparent == NONE {
            return;
        }
        let parent_map = self.graph().node(parent).map_to_parent.clone();
        let Some(source) =
            parent_map.map_source_to_target(self.ctx.store.paths(), entry.site.path.prim_path())
        else {
            return;
        };
        let exists = self.graph().children(grandparent).any(|child| {
            let c = self.graph().node(child);
            c.arc_kind == ArcKind::Relocates && c.site.path == SpecPath::from_prim_path(source)
        });
        if exists {
            return;
        }
        let stack = self.graph().node(grandparent).site.stack;
        let depth = non_variant_depth(self.paths(), &self.graph().node(grandparent).site.path);
        self.add_arc(
            ArcKind::Relocates,
            grandparent,
            node,
            Site {
                stack,
                path: SpecPath::from_prim_path(source),
            },
            MapFunction::identity(),
            0,
            depth,
            ArcOptions {
                direct_node_contributes: false,
                ..ArcOptions::default()
            },
        );
    }

    // ── Class-based arcs ───────────────────────────────────────────────

    /// OpenUSD: `_EvalNodeInherits`, `_EvalNodeSpecializes`.
    fn eval_node_class_arcs(&mut self, node: NodeIndex, kind: ArcKind) {
        let entry = self.graph().node(node).clone();
        if !entry.can_contribute_specs() {
            return;
        }
        let stack = self.ctx.stacks.get(entry.site.stack);
        let arcs = site::class_arcs(
            self.ctx.store,
            stack,
            &entry.site.path,
            kind == ArcKind::Specializes,
        );
        for (arc_num, class) in arcs.into_iter().enumerate() {
            let arc_num = u16::try_from(arc_num).unwrap_or(u16::MAX);
            let map = self
                .map_for_arc(
                    class,
                    &entry.site.path,
                    entry.site.stack,
                    LayerOffset::IDENTITY,
                )
                .with_root_identity(self.ctx.store.paths());
            self.add_class_based_arc(
                Some(SpecPath::from_prim_path(class)),
                kind,
                node,
                node,
                map,
                arc_num,
                None,
            );
        }
    }

    /// OpenUSD: `_AddClassBasedArc`.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors `_AddClassBasedArc`, whose inputs are separate"
    )]
    fn add_class_based_arc(
        &mut self,
        inherit_path: Option<SpecPath>,
        kind: ArcKind,
        parent: NodeIndex,
        origin: NodeIndex,
        map: MapFunction,
        arc_num: u16,
        ignore_if_same_as: Option<&Site>,
    ) -> NodeIndex {
        let Some(inherit_path) = inherit_path else {
            return NONE;
        };
        let parent_entry = self.graph().node(parent).clone();
        let parent_arc_kind = FrameCursor::new(&self.outputs.graph, parent, self.previous_frame)
            .arc_kind()
            .unwrap_or(ArcKind::Local);
        let inherit_site = Site {
            stack: parent_entry.site.stack,
            path: inherit_path.clone(),
        };
        let matching = if parent_arc_kind != ArcKind::Relocates {
            self.graph()
                .children(parent)
                .find(|child| self.graph().node(*child).site == inherit_site)
        } else {
            let depth_below = self.graph().depth_below_introduction(self.paths(), origin);
            self.graph().children(parent).find(|child| {
                let c = self.graph().node(*child);
                c.arc_kind == kind
                    && self
                        .graph()
                        .depth_below_introduction(self.paths(), c.origin)
                        == depth_below
                    && c.map_to_parent == map
            })
        };
        if let Some(child) = matching {
            let implied = is_class_based(kind) && parent != origin;
            if kind == ArcKind::Specializes && parent == 0 && implied {
                let child_origin = self.graph().node(child).origin;
                if self
                    .graph()
                    .cmp_nodes(self.paths(), origin, child_origin)
                    .is_lt()
                {
                    self.outputs
                        .graph
                        .inert_subtree(self.ctx.store.paths(), child);
                } else {
                    return child;
                }
            } else {
                return child;
            }
        }

        let namespace_depth = non_variant_depth(self.paths(), &parent_entry.site.path);
        if kind == ArcKind::Specializes {
            let add_placeholder = parent != 0 || self.previous_frame.is_some();
            if add_placeholder {
                let opts = ArcOptions {
                    direct_node_contributes: false,
                    include_ancestral_opinions: false,
                    tasks: TaskSet::NONE,
                    ..ArcOptions::default()
                };
                let placeholder = self.add_arc(
                    kind,
                    parent,
                    origin,
                    inherit_site,
                    map,
                    arc_num,
                    namespace_depth,
                    opts,
                );
                if self.previous_frame.is_none()
                    && placeholder != NONE
                    && !self.is_relocates_placeholder_implied_arc(placeholder)
                {
                    let propagated = self.propagate_node_to_root(placeholder);
                    if propagated != NONE && self.graph().node(propagated).origin != placeholder {
                        self.add_tasks_for_node(
                            placeholder,
                            TaskSet::NONE.with(TaskKind::ImpliedClasses),
                        );
                    }
                    return propagated;
                }
                return placeholder;
            }
        }

        let direct_contributes = inherit_path != parent_entry.site.path
            && ignore_if_same_as.is_none_or(|ignore| *ignore != inherit_site);
        let is_root_prim = self.paths().resolve(inherit_path.prim_path()).depth() == 1;
        let opts = ArcOptions {
            direct_node_contributes: direct_contributes,
            skip_duplicate_nodes: direct_contributes,
            include_ancestral_opinions: direct_contributes && !is_root_prim,
            ..ArcOptions::default()
        };
        self.add_arc(
            kind,
            parent,
            origin,
            inherit_site,
            map,
            arc_num,
            namespace_depth,
            opts,
        )
    }

    /// OpenUSD: `_IsRelocatesPlaceholderImpliedArc`.
    fn is_relocates_placeholder_implied_arc(&self, node: NodeIndex) -> bool {
        let graph = self.graph();
        let entry = graph.node(node);
        let parent = entry.parent;
        parent != entry.origin
            && graph.node(parent).arc_kind == ArcKind::Relocates
            && graph.node(parent).site == entry.site
    }

    /// OpenUSD: `_PropagateNodeToRoot`.
    fn propagate_node_to_root(&mut self, source: NodeIndex) -> NodeIndex {
        let entry = self.graph().node(source).clone();
        if let Some(existing) = self
            .graph()
            .children(0)
            .find(|child| self.graph().node(*child).site == entry.site)
        {
            return existing;
        }
        let is_root_prim = self.paths().resolve(entry.site.path.prim_path()).depth() == 1;
        let opts = ArcOptions {
            skip_duplicate_nodes: true,
            include_ancestral_opinions: !is_root_prim,
            copy_ancestor_flag_from_origin: true,
            ..ArcOptions::default()
        };
        let namespace_depth = non_variant_depth(self.paths(), &self.graph().node(0).site.path);
        self.add_arc(
            entry.arc_kind,
            0,
            source,
            entry.site.clone(),
            entry.map_to_root.clone(),
            entry.sibling_num,
            namespace_depth,
            opts,
        )
    }

    /// OpenUSD: `_FindSpecializesToPropagateToRoot`.
    fn find_specializes_to_propagate(&mut self, node: NodeIndex) {
        if self.is_relocates_placeholder_implied_arc(node) {
            return;
        }
        if self.graph().node(node).arc_kind == ArcKind::Specializes {
            self.propagate_node_to_root(node);
        }
        for child in self.graph().child_list(node) {
            self.find_specializes_to_propagate(child);
        }
    }

    /// OpenUSD: `_EvalImpliedSpecializes`.
    fn eval_implied_specializes(&mut self, node: NodeIndex) {
        if self.graph().node(node).parent == NONE {
            return;
        }
        self.find_specializes_to_propagate(node);
    }

    /// OpenUSD: `_EvalImpliedClasses`.
    fn eval_implied_classes(&mut self, node: NodeIndex) {
        let parent = self.graph().node(node).parent;
        if parent == NONE || !self.has_class_based_child(node) {
            return;
        }
        let transfer = self.graph().node(node).map_to_parent.clone();
        let transfer = transfer.with_root_identity(self.ctx.store.paths());
        self.eval_implied_class_tree(parent, node, &transfer, true);
    }

    /// OpenUSD: `_EvalImpliedClassTree`.
    fn eval_implied_class_tree(
        &mut self,
        dest: NodeIndex,
        source: NodeIndex,
        transfer: &MapFunction,
        source_is_start: bool,
    ) {
        if self.graph().node(dest).arc_kind == ArcKind::Relocates {
            let dest_map = self.graph().node(dest).map_to_parent.clone();
            let dest_map = dest_map.with_root_identity(self.ctx.store.paths());
            let new_transfer = dest_map.compose(self.ctx.store.paths(), transfer);
            let dest_parent = self.graph().node(dest).parent;
            self.eval_implied_class_tree(dest_parent, source, &new_transfer, source_is_start);
            self.add_task(Task::new(TaskKind::ImpliedClasses, dest));
            return;
        }
        let propagated = self.propagated_specializes_node(source);
        let source_children = self.graph().child_list(if propagated != NONE {
            propagated
        } else {
            source
        });
        for source_child in source_children {
            let child_entry = self.graph().node(source_child).clone();
            if !is_class_based(child_entry.arc_kind) {
                continue;
            }
            if source_is_start
                && is_class_based(self.graph().node(source).arc_kind)
                && self.graph().depth_below_introduction(self.paths(), source)
                    == self
                        .graph()
                        .depth_below_introduction(self.paths(), source_child)
            {
                continue;
            }
            let dest_class_map = MapFunction::implied_class(
                self.ctx.store.paths(),
                transfer,
                &child_entry.map_to_parent,
            );
            let mut dest_child = NONE;
            for child in self.graph().child_list(dest) {
                if self.graph().node(child).origin != source_child {
                    continue;
                }
                if self.graph().is_propagated_specializes(child) {
                    continue;
                }
                dest_child = child;
                break;
            }
            if dest_child == NONE {
                let dest_path = self.graph().node(dest).site.path.clone();
                let inherit_path =
                    self.determine_inherit_path(&dest_path, &child_entry.site.path, transfer);
                dest_child = self.add_class_based_arc(
                    inherit_path,
                    child_entry.arc_kind,
                    dest,
                    source_child,
                    dest_class_map,
                    child_entry.sibling_num,
                    Some(&child_entry.site),
                );
            }
            if dest_child != NONE && self.has_class_based_child(source_child) {
                let propagated = self.propagated_specializes_node(dest_child);
                if propagated != NONE {
                    dest_child = propagated;
                }
                self.eval_implied_class_tree(dest_child, source_child, transfer, false);
            }
        }
    }

    /// OpenUSD: `_DetermineInheritPath`.
    fn determine_inherit_path(
        &mut self,
        dest_parent: &SpecPath,
        source_child: &SpecPath,
        transfer: &MapFunction,
    ) -> Option<SpecPath> {
        let mapped =
            transfer.map_source_to_target(self.ctx.store.paths(), source_child.prim_path())?;
        if !dest_parent.has_variant_selections() {
            return Some(SpecPath::from_prim_path(mapped));
        }
        Some(site_paths::apply_selections_of(
            self.ctx.store.paths(),
            mapped,
            dest_parent,
        ))
    }

    // ── Variants ───────────────────────────────────────────────────────

    /// OpenUSD: `_EvalNodeVariantSets`.
    fn eval_node_variant_sets(&mut self, node: NodeIndex) {
        let entry = self.graph().node(node).clone();
        if !entry.can_contribute_specs() {
            return;
        }
        self.eval_variant_sets_at_site(node, entry.site.path.clone(), false);
    }

    /// OpenUSD: `_EvalVariantSetsAtSite`.
    fn eval_variant_sets_at_site(&mut self, node: NodeIndex, site_path: SpecPath, ancestral: bool) {
        let stack = self.ctx.stacks.get(self.graph().node(node).site.stack);
        let names = site::variant_sets(self.ctx.store, stack, &site_path);
        let kind = if ancestral {
            TaskKind::AncestralVariantAuthored
        } else {
            TaskKind::VariantAuthored
        };
        for (num, name) in names.into_iter().enumerate() {
            let num = u16::try_from(num).unwrap_or(u16::MAX);
            self.add_task(Task::variant(kind, node, site_path.clone(), name, num));
        }
    }

    /// OpenUSD: `_EvalNodeAncestralVariantSets`.
    fn eval_ancestral_variant_sets(&mut self, node: NodeIndex) {
        let start = self.graph().node(node).site.path.clone();
        let mut path = site_paths::parent(self.ctx.store.paths(), &start);
        while let Some(current) = path {
            let paths = self.ctx.store.paths();
            if paths.resolve(current.prim_path()).depth() == 0 && !current.has_variant_selections()
            {
                break;
            }
            if self.node_can_contribute_ancestral_opinions(node, &current) {
                self.eval_variant_sets_at_site(node, current.clone(), true);
            }
            if site_paths::is_variant_selection_path(&current) {
                break;
            }
            path = site_paths::parent(self.ctx.store.paths(), &current);
        }
    }

    /// OpenUSD: `_NodeCanContributeAncestralOpinions`.
    fn node_can_contribute_ancestral_opinions(&self, node: NodeIndex, path: &SpecPath) -> bool {
        let depth = self.graph().node(node).restricted_depth as usize;
        depth == 0 || depth > site_paths::element_count(self.paths(), path)
    }

    /// OpenUSD: `_EvalNodeAuthoredVariant`.
    fn eval_authored_variant(
        &mut self,
        node: NodeIndex,
        vset_path: SpecPath,
        vset: TokenId,
        vset_num: u16,
        ancestral: bool,
    ) {
        if !self.node_can_contribute_ancestral_opinions(node, &vset_path) {
            return;
        }
        let selection = self.compose_variant_selection(node, vset_path.prim_path(), vset);
        let selection =
            selection.filter(|variant| !self.ctx.store.tokens().resolve(*variant).is_empty());
        let Some(variant) = selection else {
            let kind = if ancestral {
                TaskKind::AncestralVariantFallback
            } else {
                TaskKind::VariantFallback
            };
            self.add_task(Task::variant(kind, node, vset_path, vset, vset_num));
            return;
        };
        if ancestral {
            self.add_ancestral_variant_arc(node, &vset_path, vset, vset_num, variant);
        } else {
            self.add_variant_arc(node, vset, vset_num, variant);
        }
    }

    /// OpenUSD: `_EvalNodeFallbackVariant`.
    fn eval_fallback_variant(
        &mut self,
        node: NodeIndex,
        vset_path: SpecPath,
        vset: TokenId,
        vset_num: u16,
        ancestral: bool,
    ) {
        if !self.node_can_contribute_ancestral_opinions(node, &vset_path) {
            return;
        }
        let stack = self.ctx.stacks.get(self.graph().node(node).site.stack);
        let options = site::variant_set_options(self.ctx.store, stack, &vset_path, vset);
        let chosen = self
            .ctx
            .fallbacks
            .get(&vset)
            .and_then(|fallbacks| fallbacks.iter().copied().find(|v| options.contains(v)));
        let Some(variant) = chosen else {
            let kind = if ancestral {
                TaskKind::AncestralVariantNoneFound
            } else {
                TaskKind::VariantNoneFound
            };
            self.add_task(Task::variant(kind, node, vset_path, vset, vset_num));
            return;
        };
        if ancestral {
            self.add_ancestral_variant_arc(node, &vset_path, vset, vset_num, variant);
        } else {
            self.add_variant_arc(node, vset, vset_num, variant);
        }
    }

    /// Whether `site` authors something that may bring in new variant
    /// selections. OpenUSD: `_SiteMightIntroducePendingVariantSelections`.
    fn site_might_introduce_selections(&self, stack: StackId, path: &SpecPath) -> bool {
        let stack = self.ctx.stacks.get(stack);
        site::specs(self.ctx.store, stack, path).any(|(_, _, spec)| spec.may_introduce_selections())
    }

    /// OpenUSD: `_AddVariantArc`.
    fn add_variant_arc(&mut self, node: NodeIndex, vset: TokenId, vset_num: u16, variant: TokenId) {
        let entry = self.graph().node(node).clone();
        let var_path = site_paths::append_selection(self.paths(), &entry.site.path, vset, variant);
        let depth = non_variant_depth(self.paths(), &entry.site.path);
        let new_node = self.add_arc(
            ArcKind::Variants,
            node,
            node,
            Site {
                stack: entry.site.stack,
                path: var_path.clone(),
            },
            MapFunction::identity(),
            vset_num,
            depth,
            ArcOptions::default(),
        );
        if new_node != NONE && self.site_might_introduce_selections(entry.site.stack, &var_path) {
            self.retry_variant_tasks();
        }
    }

    /// OpenUSD: `_AddAncestralVariantArc`.
    fn add_ancestral_variant_arc(
        &mut self,
        node: NodeIndex,
        vset_path: &SpecPath,
        vset: TokenId,
        vset_num: u16,
        variant: TokenId,
    ) {
        let entry = self.graph().node(node).clone();
        let ancestral_var_path =
            site_paths::append_selection(self.paths(), vset_path, vset, variant);
        let var_path = site_paths::replace_prefix(
            self.ctx.store.paths(),
            &entry.site.path,
            vset_path,
            &ancestral_var_path,
        );
        let namespace_depth = non_variant_depth(self.paths(), vset_path);
        let mut skip = false;
        let mut n = node;
        while n != 0 && n != NONE {
            let e = self.graph().node(n);
            if is_class_based(e.arc_kind)
                && self.graph().depth_below_introduction(self.paths(), n) == 0
                && !e.inert
            {
                skip = true;
                break;
            }
            n = e.parent;
        }
        let new_node = self.add_arc(
            ArcKind::Variants,
            node,
            node,
            Site {
                stack: entry.site.stack,
                path: var_path,
            },
            MapFunction::identity(),
            vset_num,
            namespace_depth,
            ArcOptions {
                include_ancestral_opinions: true,
                skip_duplicate_nodes: skip,
                ..ArcOptions::default()
            },
        );
        if new_node != NONE
            && self.site_might_introduce_selections(entry.site.stack, &ancestral_var_path)
        {
            self.retry_variant_tasks();
        }
    }

    /// The variant selected for `vset` at the stripped path `path_in_node`
    /// of `node`: the strongest selection already applied in the index, or
    /// the strongest authored. OpenUSD: `_ComposeVariantSelection`.
    fn compose_variant_selection(
        &mut self,
        node: NodeIndex,
        path_in_node: PathId,
        vset: TokenId,
    ) -> Option<TokenId> {
        let (start_path, start) = self.translate_path_to_root_or_closest(node, path_in_node);
        if let Some(found) = self.find_prior_variant_selection(start, &start_path, vset) {
            return Some(found);
        }
        self.compose_selection_across_nodes(start, &start_path, vset)
    }

    /// OpenUSD: `Pcp_TranslatePathFromNodeToRootOrClosestNode`.
    fn translate_path_to_root_or_closest(
        &mut self,
        node: NodeIndex,
        path: PathId,
    ) -> (SpecPath, NodeIndex) {
        if node == 0 {
            return (SpecPath::from_prim_path(path), 0);
        }
        let mut current = node;
        let mut current_path = path;
        let map_to_root = self.graph().node(node).map_to_root.clone();
        if let Some(in_root) = map_to_root.map_source_to_target(self.ctx.store.paths(), path) {
            current = 0;
            current_path = in_root;
        } else {
            while current != 0 {
                let map = self.graph().node(current).map_to_parent.clone();
                let Some(in_parent) =
                    map.map_source_to_target(self.ctx.store.paths(), current_path)
                else {
                    break;
                };
                current = self.graph().node(current).parent;
                current_path = in_parent;
            }
        }
        let at_intro = self
            .outputs
            .graph
            .path_at_introduction(self.ctx.store.paths(), current);
        let site = site_paths::apply_selections_of(self.paths(), current_path, &at_intro);
        (site, current)
    }

    /// The nodes of `start`'s subtree in strength order, each with
    /// `start_path` translated into its namespace (`None` where it does not
    /// map, and beneath).
    fn translated_subtree(
        &mut self,
        start: NodeIndex,
        start_path: &SpecPath,
    ) -> Vec<(NodeIndex, Option<PathId>)> {
        let subtree = self.graph().subtree(start);
        // Each node's translated path, by node: a parent precedes its
        // children in the subtree.
        let mut translated: Vec<Option<PathId>> = alloc::vec![None; self.graph().len()];
        let mut out = Vec::with_capacity(subtree.len());
        for node in subtree {
            let path = if node == start {
                Some(start_path.prim_path())
            } else {
                let entry = self.graph().node(node);
                let in_parent = translated[entry.parent as usize];
                let map = entry.map_to_parent.clone();
                in_parent.and_then(|p| map.map_target_to_source(self.ctx.store.paths(), p))
            };
            translated[node as usize] = path;
            out.push((node, path));
        }
        out
    }

    /// OpenUSD: `_FindPriorVariantSelection`.
    fn find_prior_variant_selection(
        &mut self,
        start: NodeIndex,
        start_path: &SpecPath,
        vset: TokenId,
    ) -> Option<TokenId> {
        for (node, path) in self.translated_subtree(start, start_path) {
            if self.graph().node(node).arc_kind != ArcKind::Variants {
                continue;
            }
            let at_intro = self
                .outputs
                .graph
                .path_at_introduction(self.ctx.store.paths(), node);
            let Some(selection) = at_intro.trailing_selection() else {
                continue;
            };
            if selection.set != vset {
                continue;
            }
            let Some(path) = path else {
                continue;
            };
            let prim = site_paths::without_trailing_selections(self.paths(), &at_intro);
            if !prim.has_variant_selections() && prim.prim_path() == path {
                return Some(selection.variant);
            }
        }
        None
    }

    /// OpenUSD: `_ComposeVariantSelectionAcrossNodes`.
    fn compose_selection_across_nodes(
        &mut self,
        start: NodeIndex,
        start_path: &SpecPath,
        vset: TokenId,
    ) -> Option<TokenId> {
        for (node, path) in self.translated_subtree(start, start_path) {
            let Some(path) = path else {
                continue;
            };
            let plain = SpecPath::from_prim_path(path);
            if !self.node_can_contribute_ancestral_opinions(node, &plain) {
                continue;
            }
            let site_path = if self.graph().node(node).arc_kind == ArcKind::Variants {
                let at_intro = self
                    .outputs
                    .graph
                    .path_at_introduction(self.ctx.store.paths(), node);
                site_paths::apply_selections_of(self.paths(), path, &at_intro)
            } else {
                plain
            };
            let stack_id = self.graph().node(node).site.stack;
            let stack = self.ctx.stacks.get(stack_id);
            if !site::has_variant_selections(self.ctx.store, stack, &site_path) {
                continue;
            }
            let authored = site::authored_selections(self.ctx.store, stack, &site_path, vset);
            for variant in authored {
                match self.evaluate_selection(stack_id, variant) {
                    Some(evaluated) => return Some(evaluated),
                    None => continue,
                }
            }
        }
        None
    }

    /// `variant` as authored in `layer`, evaluated when it is a variable
    /// expression; `None` when it fails to evaluate.
    fn evaluate_selection(&mut self, stack: StackId, variant: TokenId) -> Option<TokenId> {
        let text = self.ctx.store.tokens().resolve(variant);
        if !crate::variable_expression::is_expression(text) {
            return Some(variant);
        }
        let text = String::from(text);
        let computed = self.ctx.stacks.get(stack);
        let mut chain: Vec<LayerStackIdentifier> = computed.chain.to_vec();
        if chain.last() != Some(&computed.identifier) {
            chain.push(computed.identifier);
        }
        // Variant selection expression errors and reads are reported
        // where the stage reads selections (`read_selections`).
        let result = crate::expression_variables::evaluate(self.ctx.store, &chain, &text, None);
        match result {
            Ok(name) => {
                let name = name.unwrap_or_default();
                Some(self.ctx.store.tokens().intern(name))
            }
            Err(_) => None,
        }
    }
}

/// Whether the graph `graph` is an instance's index: it has instanceable
/// data and its strongest `instanceable` opinion is `true`. OpenUSD:
/// `Pcp_PrimIndexIsInstanceable`.
pub(crate) fn is_instanceable(
    store: &dyn LayerStore,
    stacks: &LayerStacks<'_>,
    graph: &Graph,
) -> bool {
    let instanceable_node =
        |node: &Node| node.transitive_direct && node.can_contribute_specs() && node.has_specs;
    let mut has_data = false;
    let mut pending = graph.child_list(0);
    while let Some(node) = pending.pop() {
        let entry = graph.node(node);
        if entry.culled {
            continue;
        }
        if instanceable_node(entry) {
            has_data = true;
            break;
        }
        pending.extend(graph.children(node));
    }
    if !has_data {
        return false;
    }
    for node in graph.strength_order() {
        let entry = graph.node(node);
        if !entry.can_contribute_specs() {
            continue;
        }
        let stack = stacks.get(entry.site.stack);
        for (_, _, spec) in site::specs(store, stack, &entry.site.path) {
            if let Some(value) = spec.instanceable(store.tokens()) {
                return value;
            }
        }
    }
    false
}

/// Whether `path` is `prefix` or beneath it.
fn has_prefix(paths: &crate::path::PathInterner, path: PathId, prefix: PathId) -> bool {
    path == prefix
        || paths
            .resolve(path)
            .segments()
            .starts_with(paths.resolve(prefix).segments())
}

/// Whether the site path `path` is `prefix` or beneath it.
fn has_spec_prefix(paths: &crate::path::PathInterner, path: &SpecPath, prefix: &SpecPath) -> bool {
    if !has_prefix(paths, path.prim_path(), prefix.prim_path()) {
        return false;
    }
    let (own, theirs) = (path.selection_entries(), prefix.selection_entries());
    theirs.len() <= own.len() && own[..theirs.len()] == *theirs
}

/// What `path`, a site in a recursively built graph rooted at `current`,
/// becomes once that graph is the index of `requested`:
/// `requested.ReplacePrefix(current, path)` in OpenUSD (`/C` added while
/// `/A` is built for `/A/B` becomes `/C/B`). `requested` is returned when
/// `current` is not a prefix of it.
fn replace_spec_prefix(
    paths: &crate::path::PathInterner,
    requested: &SpecPath,
    current: &SpecPath,
    path: &SpecPath,
) -> SpecPath {
    if !has_spec_prefix(paths, requested, current) {
        return requested.clone();
    }
    site_paths::replace_prefix(paths, requested, current, path)
}

/// The relocations of `stack` that affect namespace at and below `path`,
/// as a map function with the root identity. OpenUSD:
/// `GetExpressionForRelocatesAtPath`, `_FilterRelocationsForPath`.
fn relocates_map_at(
    store: &dyn LayerStore,
    stack: &super::layer_stacks::ComputedStack<'_>,
    path: PathId,
) -> MapFunction {
    let mut pairs: Vec<(PathId, Option<PathId>)> = Vec::new();
    let mut targets: Vec<PathId> = Vec::new();
    for &(source, target) in &stack.combined_source_to_target {
        if let Some(target) = target
            && has_prefix(store.paths(), source, path)
        {
            pairs.push((source, Some(target)));
            targets.push(target);
        }
    }
    for relocate in stack.relocations.iter() {
        if let Some(target) = relocate.target
            && has_prefix(store.paths(), relocate.source, path)
            && !targets.contains(&target)
        {
            pairs.push((relocate.source, Some(target)));
            targets.push(target);
        }
    }
    let root = store.paths().intern_segments(&[]);
    pairs.push((root, Some(root)));
    // Composed by deferring, as OpenUSD's `PcpLayerStack::GetExpressionForRelocatesAtPath` does:
    // a path mapped through the arc and then the relocations must map
    // back through each.
    MapFunction::new(store.paths(), pairs, LayerOffset::IDENTITY).deferred()
}

/// Maps `path` across an arc for payload inclusion, ignoring the identity
/// an internal reference or payload adds. OpenUSD:
/// `_MapPathToNodeParentPayloadInclusionPath`.
fn map_for_payload_inclusion(
    paths: &crate::path::PathInterner,
    map_to_parent: &MapFunction,
    kind: ArcKind,
    path: PathId,
) -> Option<PathId> {
    if map_to_parent.has_root_identity() && matches!(kind, ArcKind::References | ArcKind::Payloads)
    {
        return map_to_parent
            .without_root_identity(paths)
            .map_source_to_target(paths, path);
    }
    map_to_parent.map_source_to_target(paths, path)
}

/// OpenUSD: `_MapPathToNodeRootPayloadInclusionPath`.
fn map_to_root_for_payload(
    store: &dyn LayerStore,
    graph: &Graph,
    node: NodeIndex,
    path: PathId,
) -> Option<PathId> {
    let entry = graph.node(node);
    let mapped = entry.map_to_root.map_source_to_target(store.paths(), path);
    if mapped == Some(path) && entry.map_to_root.has_root_identity() {
        let mut current = node;
        let mut mapped = Some(path);
        while let Some(p) = mapped {
            if current == 0 {
                break;
            }
            let e = graph.node(current);
            mapped = map_for_payload_inclusion(store.paths(), &e.map_to_parent, e.arc_kind, p);
            current = e.parent;
        }
        return mapped;
    }
    mapped
}
