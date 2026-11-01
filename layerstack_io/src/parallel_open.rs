// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reading the layers a stage opens on several threads
//! ([`StageOptions::threads`](layerstack::StageOptions::threads)).
//!
//! Every layer the root reaches is identified, read and parsed by whichever
//! thread takes it, each of its arcs resolved to its target's final id
//! without waiting for the target, as an id is derived from the identifier
//! that names it ([`crate::layer_ids`]), and each target found for the
//! first time offered to the pool at once, so an idle thread reads it while
//! the layer naming it is still being read
//! ([`layerstack::parallel::Added::offer`]). One pass then walks the layers
//! depth first, in the order their arcs asked for their targets, recording
//! them as reading them one at a time does ([`Reader::load`]): the same
//! bindings, failures, diagnostics and pending layers, in the same order.
//! Last, an arc whose target failed to read becomes unresolved, as the
//! parser makes an arc whose target fails while it reads; only the layers
//! naming such a target are rewritten. Under a collision of derived ids,
//! which the threads may settle in another order than one thread does, the
//! pass renumbers every layer read to the ids one thread gives them.
//!
//! A layer a package holds is read on one thread instead.
//!
//! OpenUSD opens the layers prim indexing reaches from its worker threads
//! (`SdfLayer::FindOrOpen` in `_EvalRefOrPayloadArcs`).

use std::sync::Mutex;

use super::{
    AssetFailure, FileFormat, FileFormats, Format, FormatArgs, Identities, ImportPolicy, IoError,
    IoErrorKind, LooseRead, Reader, Source, State, Storage, String, Vec, asset_error, read_loose,
    source_format, source_identifier, strict_failure, strict_rejects,
};
use crate::layer_ids;
use crate::timing::{LayerRead, LoadClock, ReadTime, TimeSpan};
use alloc::{collections::BTreeSet, sync::Arc};
use hashbrown::{HashMap, HashSet};
use layerstack::{
    AssetResolveError, AssetResolver, Layer, LayerId, PathInterner, ResolvedAsset, TokenInterner,
    asset::PackagePath,
    parallel::{Added, Threads, with_offering_pool_in},
};

/// A source to read, by identifier, and the id its layer is read as.
struct Task {
    identifier: Arc<str>,
    id: LayerId,
}

/// One arc a layer resolved while it was read, in the order it asked.
///
/// Its asset path and identifier are shared with the layer's other calls
/// for the same asset and every call naming the same source, so recording
/// them in the catalog and the bindings copies no string.
#[derive(Clone)]
enum Call {
    /// The asset path did not identify a source.
    Failed { asset: Arc<str>, error: IoError },
    /// The asset path identified the source `identifier`.
    Target {
        asset: Arc<str>,
        identifier: Arc<str>,
        id: LayerId,
    },
}

/// What reading one source found.
struct Read {
    calls: Vec<Call>,
    outcome: Outcome,
    /// The read's work and time, for the reader's log, when it decoded
    /// the source.
    logged: Option<LayerRead>,
}

enum Outcome {
    /// Reading the source's bytes failed.
    Unread(IoError),
    Read {
        format: Format,
        read: alloc::boxed::Box<LooseRead>,
    },
    /// The source is a package, or resolves an arc into one.
    Package,
}

/// What the threads reading sources share.
struct Shared<'a, B> {
    storage: &'a B,
    tokens: &'a TokenInterner,
    paths: &'a PathInterner,
    schemas: Option<&'a layerstack::SchemaRegistry>,
    usdc_options: super::UsdcReadOptions,
    formats: &'a FileFormats,
    defers_variant_arcs: bool,
    /// Whether every payload is left for composition to ask for
    /// ([`super::excludes_payloads`]).
    defers_payloads: bool,
    /// The threads of the open: the pool reading sources runs on them, and
    /// a USDC source is read on those it leaves idle ([`read_loose`]).
    threads: &'a Threads,
    /// [`Reader::identify`]'s cache.
    identities: Mutex<Identities>,
    /// The ids the document's catalog gave identifiers before this open.
    known: &'a HashMap<Arc<str>, LayerId>,
    /// The ids the catalog and the store held before this open.
    reserved: &'a BTreeSet<LayerId>,
    sources: Mutex<Sources>,
    /// The clock the reads are timed with ([`super::LoadOptions::clock`]).
    clock: Option<&'a LoadClock>,
}

/// The sources found so far.
#[derive(Default)]
struct Sources {
    /// Each source, by identifier, with its id.
    ids: HashMap<Arc<str>, LayerId>,
    /// The ids of `ids`.
    taken: HashSet<LayerId>,
    /// Whether a source's id stepped past a taken candidate
    /// ([`layer_ids::derive`]): threads may settle a collision in another
    /// order than one thread does.
    stepped: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<B: Storage> Shared<'_, B> {
    fn identify(&self, asset: &str, anchor: &str) -> Result<String, IoError> {
        let key = (Some(String::from(anchor)), String::from(asset));
        if let Some(found) = lock(&self.identities).get(&key) {
            return found.clone();
        }
        // Identified without the lock, so threads identify sources at once.
        let found = self
            .storage
            .identify(asset, Some(anchor))
            .map_err(|e| e.at(asset));
        lock(&self.identities).entry(key).or_insert(found).clone()
    }

    /// The source `identifier`, shared, with its id, and the first time
    /// it is found, the task to read it.
    fn source(&self, identifier: String) -> Result<(Arc<str>, LayerId, Option<Task>), IoError> {
        // Hashed before taking the lock, so threads hash at once.
        let first = layer_ids::candidate(&identifier, 0);
        let mut sources = lock(&self.sources);
        if let Some((identifier, &id)) = sources.ids.get_key_value(identifier.as_str()) {
            return Ok((Arc::clone(identifier), id, None));
        }
        let taken = |sources: &Sources, id: LayerId| {
            id == LayerId::UNRESOLVED || self.reserved.contains(&id) || sources.taken.contains(&id)
        };
        let id = if let Some(&id) = self.known.get(identifier.as_str()) {
            id
        } else if !taken(&sources, first) {
            first
        } else {
            sources.stepped = true;
            layer_ids::derive(&identifier, |id| taken(&sources, id))?
        };
        sources.taken.insert(id);
        let identifier: Arc<str> = Arc::from(identifier);
        sources.ids.insert(Arc::clone(&identifier), id);
        let task = Task {
            identifier: Arc::clone(&identifier),
            id,
        };
        Ok((identifier, id, Some(task)))
    }

    fn read(&self, task: Task, added: &mut Added<'_, Task>) -> (LayerId, Read) {
        let mut read = Read {
            calls: Vec::new(),
            outcome: Outcome::Package,
            logged: None,
        };
        let format = self.formats.for_identifier(&task.identifier);
        let bytes = if format.is_none_or(FileFormat::reads_source) {
            let (outer, _) = match super::format::split_args(&task.identifier) {
                Ok(split) => split,
                Err(error) => {
                    read.outcome = Outcome::Unread(error);
                    return (task.id, read);
                }
            };
            match self.storage.read_encoded(outer) {
                Ok(bytes) => bytes,
                Err(error) => {
                    read.outcome = Outcome::Unread(error.at(outer));
                    return (task.id, read);
                }
            }
        } else {
            layerstack::encoded_bytes::EncodedBytes::from(Vec::new())
        };
        let format = source_format(self.formats, &task.identifier, &bytes);
        if format == Format::Usdz {
            return (task.id, read);
        }
        let mut resolver = TaskResolver {
            shared: self,
            anchor: &task.identifier,
            calls: Vec::new(),
            asked: HashMap::new(),
            added,
            package: false,
        };
        let start = self.clock.map(LoadClock::now);
        let loose = read_loose(
            format,
            &bytes,
            &task.identifier,
            task.id,
            self.tokens,
            self.paths,
            &mut resolver,
            self.schemas,
            self.usdc_options,
            self.formats,
            self.threads,
        );
        if !resolver.package {
            // Reads on threads do not nest: an arc's target is a task of
            // its own, so the whole span is this layer's.
            let time = start.zip(self.clock).map(|(start, clock)| {
                let span = TimeSpan {
                    start,
                    end: clock.now(),
                };
                ReadTime {
                    span,
                    own: span.duration(),
                }
            });
            read.logged = Some(LayerRead {
                layer: task.id,
                bytes: bytes.len() as u64,
                prims: loose
                    .layer
                    .as_ref()
                    .map_or(0, |layer| crate::timing::prim_count(layer, self.paths))
                    as u64,
                time,
            });
            read.calls = resolver.calls;
            read.outcome = Outcome::Read {
                format,
                read: alloc::boxed::Box::new(loose),
            };
        }
        (task.id, read)
    }
}

/// The resolver one source is read with: it identifies each arc's asset,
/// queues new sources and records the arcs in order.
struct TaskResolver<'s, 'p, 'a, B> {
    shared: &'s Shared<'a, B>,
    /// The identifier of the source being read, which its arcs anchor to.
    anchor: &'s str,
    calls: Vec<Call>,
    /// The first of `calls` for each asset path asked: a layer that names
    /// one asset many times, as a scene's root names each asset it places,
    /// resolves it once.
    asked: HashMap<Arc<str>, usize>,
    /// The sources first found by this layer's arcs, each offered to the
    /// pool as it is found, so another thread may read it while this layer
    /// is still being read.
    added: &'s mut Added<'p, Task>,
    /// Whether an arc reached into a package.
    package: bool,
}

impl<B: Storage> TaskResolver<'_, '_, '_, B> {
    /// Resolves an asset path this layer has not asked for yet.
    fn resolve_new(&mut self, asset: &Arc<str>) -> Result<ResolvedAsset, AssetResolveError> {
        let failed = |calls: &mut Vec<Call>, error: IoError| {
            calls.push(Call::Failed {
                asset: Arc::clone(asset),
                error: error.clone(),
            });
            Err(asset_error(error))
        };
        let (path, args) = match super::format::split_args(asset) {
            Ok(split) => split,
            Err(error) => return failed(&mut self.calls, error),
        };
        let path = match PackagePath::parse(path, 32) {
            Ok(path) => path,
            Err(reason) => {
                return failed(&mut self.calls, IoError::new(IoErrorKind::Rejected, reason));
            }
        };
        if !path.members().is_empty() {
            self.package = true;
            return Err(AssetResolveError::NotFound);
        }
        let anchor = FormatArgs::strip(self.anchor);
        let outer = match self.shared.identify(path.outer(), anchor) {
            Ok(outer) => outer,
            Err(error) => return failed(&mut self.calls, error),
        };
        let identifier = source_identifier(self.shared.formats, &path, &outer, args);
        let (identifier, id, task) = match self.shared.source(identifier) {
            Ok(source) => source,
            Err(error) => return failed(&mut self.calls, error),
        };
        if let Some(task) = task {
            self.added.push(task);
            self.added.offer();
        }
        self.calls.push(Call::Target {
            asset: Arc::clone(asset),
            identifier: Arc::clone(&identifier),
            id,
        });
        Ok(ResolvedAsset {
            layer_id: id,
            resolved_path: identifier,
            layer: None,
        })
    }
}

impl<B: Storage> AssetResolver for TaskResolver<'_, '_, '_, B> {
    fn resolve(
        &mut self,
        asset: &str,
        _anchor: Option<LayerId>,
        _tokens: &TokenInterner,
        _paths: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        if let Some(&first) = self.asked.get(asset) {
            // The same asset resolves as it did, and is recorded again.
            let call = self.calls[first].clone();
            let result = match &call {
                Call::Failed { error, .. } => Err(asset_error(error.clone())),
                Call::Target { identifier, id, .. } => Ok(ResolvedAsset {
                    layer_id: *id,
                    resolved_path: Arc::clone(identifier),
                    layer: None,
                }),
            };
            self.calls.push(call);
            return result;
        }
        let asset: Arc<str> = Arc::from(asset);
        let result = self.resolve_new(&asset);
        if !self.package {
            self.asked.insert(asset, self.calls.len() - 1);
        }
        result
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }

    fn defers_variant_arcs(&self) -> bool {
        self.shared.defers_variant_arcs
    }

    fn defers_payload(&self, asset_path: &str) -> bool {
        self.shared.defers_payloads || self.shared.formats.is_dynamic(asset_path)
    }

    fn allocate_layer_id(&mut self) -> Option<LayerId> {
        self.package = true;
        None
    }
}

impl<B: Storage> Reader<'_, B> {
    /// Loads the root `asset` and every layer it reaches on the reader's
    /// threads, as [`Self::load`] does on one; `None` when it must be read
    /// on one (see the [module](self) documentation).
    pub(super) fn load_on_threads(
        &mut self,
        asset: &str,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Option<Result<ResolvedAsset, IoError>> {
        if self.threads.limit() <= 1 || self.reachable_only {
            return None;
        }
        let (path, args) = match super::format::split_args(asset) {
            Ok(split) => split,
            Err(error) => return Some(Err(error)),
        };
        let path = match PackagePath::parse(path, 32) {
            Ok(path) => path,
            Err(reason) => return Some(Err(IoError::new(IoErrorKind::Rejected, reason))),
        };
        if !path.members().is_empty() {
            return None;
        }
        let outer = match self.identify(path.outer(), None) {
            Ok(outer) => outer,
            Err(error) => return Some(Err(error)),
        };
        let identifier = source_identifier(&self.formats, &path, &outer, args);
        if self.catalog.ids.contains_key(identifier.as_str()) {
            return None;
        }
        let shared = Shared {
            storage: &*self.storage,
            tokens,
            paths,
            schemas: self.schemas.as_deref(),
            usdc_options: self.usdc_options,
            formats: &self.formats,
            defers_variant_arcs: self.defers_variant_arcs(),
            defers_payloads: self.defers_payloads,
            threads: &self.threads,
            identities: Mutex::new(self.identities.clone()),
            known: &self.catalog.ids,
            reserved: &self.catalog.reserved,
            sources: Mutex::default(),
            clock: self.log.clock(),
        };
        let (identifier, root_id, root) = match shared.source(identifier) {
            Ok(source) => source,
            Err(error) => return Some(Err(error)),
        };
        let work = |task: Task, added: &mut Added<'_, Task>| shared.read(task, added);
        let reads: HashMap<LayerId, Read> = with_offering_pool_in(&self.threads, &work, |pool| {
            pool.run(root.into_iter().collect())
        })
        .into_iter()
        .collect();
        // A source storage cannot supply yet leaves its arcs deferred, as
        // one thread reading records them (`Reader::resolve`).
        if reads.values().any(|read| {
            matches!(read.outcome, Outcome::Package)
                || matches!(&read.outcome, Outcome::Unread(error) if error.kind == IoErrorKind::Unavailable)
                || read.calls.iter().any(|call| {
                    matches!(call, Call::Failed { error, .. } if error.kind == IoErrorKind::Unavailable)
                })
        }) {
            return None;
        }
        let stepped = lock(&shared.sources).stepped;
        self.identities = shared.identities.into_inner().unwrap_or_default();
        let first_pending = self.pending.len();
        let first_diagnostic = self.diagnostics.len();
        let mut replay = Replay {
            reader: self,
            settled: !stepped,
            reads,
            renamed: HashMap::new(),
            failed: HashSet::new(),
        };
        let result = replay.visit(&identifier, root_id);
        let Replay {
            renamed,
            failed,
            reads,
            ..
        } = replay;
        debug_assert!(stepped || renamed.is_empty(), "ids settle as on one thread");
        let final_id = |id: LayerId| {
            if failed.contains(&id) {
                LayerId::UNRESOLVED
            } else {
                renamed.get(&id).copied().unwrap_or(id)
            }
        };
        // The layers naming a source that failed to read, whose arcs to it
        // become unresolved; when a collision renumbered sources, every
        // layer read is rewritten instead.
        let naming_failed: HashSet<LayerId> = if failed.is_empty() {
            HashSet::new()
        } else {
            reads
                .iter()
                .filter(|(_, read)| {
                    read.calls
                        .iter()
                        .any(|call| matches!(call, Call::Target { id, .. } if failed.contains(id)))
                })
                .map(|(&id, _)| id)
                .collect()
        };
        let rewrite = |layer: &mut Layer| {
            if !renamed.is_empty() || naming_failed.contains(&layer.id()) {
                layer.map_layer_ids(final_id);
            }
        };
        // The reads are dropped, and the layers they hold finished, on the
        // threads the reading used.
        let threads = &self.threads;
        let pending = &mut self.pending[first_pending..];
        finish(threads, reads, pending, &rewrite);
        // A diagnostic names the layer read, even one that failed, and so
        // do those a strict rejection carries.
        let rename = |diagnostics: &mut [layerstack_usdz::MemberDiagnostic]| {
            for diagnostic in diagnostics {
                if let Some(&id) = renamed.get(&diagnostic.layer_id) {
                    diagnostic.layer_id = id;
                }
            }
        };
        rename(&mut self.diagnostics[first_diagnostic..]);
        let result = result.map_err(|mut error| {
            rename(&mut error.diagnostics);
            error
        });
        Some(result.map(|(layer_id, layer)| ResolvedAsset {
            layer_id,
            resolved_path: identifier,
            layer: layer.map(|mut layer| {
                rewrite(&mut layer);
                layer
            }),
        }))
    }
}

/// What finishing a read on threads does, in tasks.
enum Finish<'l> {
    /// Drops some of the reads replayed.
    Drop(Vec<Read>),
    /// Finishes some of the layers read.
    Layers(&'l mut [Layer]),
}

/// Drops `reads` and finishes `layers`, applying `rewrite` to each, on the
/// threads `threads` spares: each read holds the arcs its layer resolved,
/// and dropping several thousand of them would otherwise leave every
/// thread but one idle. Each layer's change history, which records every
/// spec its reader inserted and any rewriting, is forgotten there too, as
/// committing a layer forgets it ([`Layer::forget_change_history`]), so
/// freeing it does not wait for the one thread committing.
fn finish(
    threads: &Threads,
    reads: HashMap<LayerId, Read>,
    layers: &mut [Layer],
    rewrite: &(dyn Fn(&mut Layer) + Sync),
) {
    let finish_layer = |layer: &mut Layer| {
        rewrite(layer);
        layer.forget_change_history();
    };
    if threads.spare() == 0 {
        drop(reads);
        layers.iter_mut().for_each(finish_layer);
        return;
    }
    let mut tasks: Vec<Finish<'_>> = layers
        .chunks_mut(super::LAYERS_PER_TASK)
        .map(Finish::Layers)
        .collect();
    let mut reads = reads.into_values();
    loop {
        let chunk: Vec<Read> = reads.by_ref().take(super::LAYERS_PER_TASK).collect();
        if chunk.is_empty() {
            break;
        }
        tasks.push(Finish::Drop(chunk));
    }
    let work = |task: Finish<'_>, _: &mut Vec<Finish<'_>>| match task {
        Finish::Drop(reads) => drop(reads),
        Finish::Layers(layers) => layers.iter_mut().for_each(finish_layer),
    };
    layerstack::parallel::with_pool_in(threads, &work, |pool| pool.run(tasks));
}

/// The single-threaded pass recording the sources read on threads.
struct Replay<'r, 'a, B> {
    reader: &'r mut Reader<'a, B>,
    /// Whether every source was read as the id one thread gives it: none
    /// stepped past a taken candidate ([`Sources::stepped`]), so each
    /// took its first, which no other source and no earlier id holds.
    settled: bool,
    reads: HashMap<LayerId, Read>,
    /// The id one thread gives each source read as another, after a
    /// collision the threads settled in another order.
    renamed: HashMap<LayerId, LayerId>,
    /// The ids, as read, of sources that failed to read.
    failed: HashSet<LayerId>,
}

impl<B: Storage> Replay<'_, '_, B> {
    /// Records the source `identifier`, read as `read_id`, as
    /// `Reader::load_external` loads it once identified: its id, then its
    /// arcs' targets in order, then its diagnostics.
    fn visit(
        &mut self,
        identifier: &Arc<str>,
        read_id: LayerId,
    ) -> Result<(LayerId, Option<Layer>), IoError> {
        let catalog = &mut *self.reader.catalog;
        let id = if let Some(&id) = catalog.ids.get(&**identifier) {
            id
        } else {
            let id = if self.settled {
                catalog.reserved.insert(read_id);
                read_id
            } else {
                catalog.allocate_for(identifier)?
            };
            catalog.ids.insert(Arc::clone(identifier), id);
            catalog.insert_source(
                id,
                Source {
                    identifier: Arc::clone(identifier),
                    format: Format::Usda,
                    package: None,
                },
            );
            id
        };
        if id != read_id {
            self.renamed.insert(read_id, id);
        }
        if catalog.state(id).is_some() {
            return Ok((id, None));
        }
        let read = self.reads.get_mut(&read_id).expect("read source");
        let format = match &read.outcome {
            Outcome::Unread(error) => {
                self.failed.insert(read_id);
                return Err(error.clone());
            }
            Outcome::Read { format, .. } => *format,
            Outcome::Package => unreachable!("packages are read on one thread"),
        };
        catalog.source_mut(id).expect("source").format = format;
        catalog.set_state(id, State::Loading);
        if let Some(mut logged) = read.logged.take() {
            logged.layer = id;
            self.reader.log.push(logged);
        }
        let first_failure = self.reader.failures.len();
        let first_diagnostic = self.reader.diagnostics.len();
        // A source that failed to parse is read again each time an arc asks
        // for it, so its arcs and diagnostics are recorded again too.
        let calls = core::mem::take(&mut read.calls);
        let mut children = Vec::new();
        for call in &calls {
            self.call(call, id, &mut children);
        }
        let read = self.reads.get_mut(&read_id).expect("read source");
        read.calls = calls;
        let Outcome::Read { read: loose, .. } = &mut read.outcome else {
            unreachable!("matched above")
        };
        let error = match &loose.layer {
            Ok(_) => {
                self.reader.pending.extend(children);
                self.reader
                    .diagnostics
                    .extend(loose.diagnostics.iter().cloned());
                if let Some(values) = loose.retained.take() {
                    self.reader.catalog.retained_values.insert(id, values);
                }
                strict_error(self.reader, identifier, first_failure, first_diagnostic)
            }
            Err(error) => {
                // A rejected text layer keeps the layers its arcs reached and
                // its diagnostics; a rejected binary layer neither.
                if format == Format::Usda {
                    self.reader.pending.extend(children);
                    self.reader
                        .diagnostics
                        .extend(loose.diagnostics.iter().cloned());
                }
                Some(error.clone())
            }
        };
        if let Some(error) = error {
            self.reader.catalog.clear_state(id);
            self.failed.insert(read_id);
            return Err(error);
        }
        let layer = core::mem::replace(
            &mut loose.layer,
            Err(IoError::new(IoErrorKind::Rejected, "layer recorded")),
        );
        // The thread that read the source dropped its view of the bytes;
        // the layer holds any it keeps.
        if self
            .reader
            .formats
            .for_identifier(identifier)
            .is_none_or(FileFormat::reads_source)
            && let Ok((outer, _)) = super::format::split_args(identifier)
        {
            self.reader.storage.release_source(outer);
        }
        self.reader.catalog.set_state(id, State::Loaded);
        Ok((id, layer.ok()))
    }

    /// Records the arc `call` of the layer `anchor`, as `Reader::resolve`
    /// does, adding the layer its target reads, if new, to `children`.
    fn call(&mut self, call: &Call, anchor: LayerId, children: &mut Vec<Layer>) {
        let (asset, result) = match call {
            Call::Failed { asset, error } => (asset, Err(error.clone())),
            Call::Target {
                asset,
                identifier,
                id,
            } => (asset, self.visit(identifier, *id)),
        };
        let bound = match result {
            Ok((id, layer)) => {
                children.extend(layer);
                id
            }
            Err(error) => {
                self.reader.failures.push(AssetFailure {
                    asset_path: String::from(&**asset),
                    anchor: Some(anchor),
                    error,
                });
                LayerId::UNRESOLVED
            }
        };
        self.reader.bind(anchor, Arc::clone(asset), bound);
    }
}

/// The error a strict import rejects the layer `identifier` with: the
/// first failure other than a missing dependency, or error diagnostic,
/// recorded since reading it began, as `Reader::read_layer` decides.
fn strict_error<B>(
    reader: &Reader<'_, B>,
    identifier: &str,
    first_failure: usize,
    first_diagnostic: usize,
) -> Option<IoError> {
    if reader.policy != ImportPolicy::Strict {
        return None;
    }
    if let Some(failure) = strict_failure(&reader.failures[first_failure..]) {
        return Some(failure.error.clone());
    }
    reader.diagnostics[first_diagnostic..]
        .iter()
        .any(strict_rejects)
        .then(|| crate::strict_rejection(identifier, &reader.diagnostics[first_diagnostic..]))
}
