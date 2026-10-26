// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reading the layers a stage opens on several threads
//! ([`StageOptions::threads`](layerstack::StageOptions::threads)).
//!
//! Every layer the root reaches is identified, read and parsed by whichever
//! thread takes it, each of its arcs resolved to a provisional id without
//! waiting for the target, and each target found for the first time
//! offered to the pool at once, so an idle thread reads it while the layer
//! naming it is still being read ([`layerstack::parallel::Added::offer`]). One pass then walks the layers depth first, in
//! the order their arcs asked for their targets, recording them as reading
//! them one at a time does ([`Reader::load`]): the same layer ids, bindings,
//! failures, diagnostics and pending layers, in the same order. Last, each
//! layer's provisional ids are rewritten to the final ones, and an arc whose
//! target failed to read becomes unresolved, as the parser makes an arc
//! whose target fails while it reads.
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
use alloc::sync::Arc;
use hashbrown::{HashMap, HashSet};
use layerstack::{
    AssetResolveError, AssetResolver, Layer, LayerId, PathInterner, ResolvedAsset, TokenInterner,
    asset::PackagePath,
    parallel::{Added, Threads, with_offering_pool_in},
};

/// The first provisional layer id; the real ones a store hands out stay
/// far below it.
const PROVISIONAL: u64 = 1 << 62;

/// A source to read, by identifier, and the provisional id its layer is
/// read as.
struct Task {
    identifier: Arc<str>,
    provisional: LayerId,
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
        provisional: LayerId,
    },
}

/// What reading one source found.
struct Read {
    calls: Vec<Call>,
    outcome: Outcome,
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
    /// The threads of the open: the pool reading sources runs on them, and
    /// a USDC source is read on those it leaves idle ([`read_loose`]).
    threads: &'a Threads,
    /// [`Reader::identify`]'s cache.
    identities: Mutex<Identities>,
    /// Each source found so far, by identifier, with its provisional id.
    sources: Mutex<HashMap<Arc<str>, LayerId>>,
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

    /// The source `identifier`, shared, with its provisional id, and the
    /// first time it is found, the task to read it.
    fn source(&self, identifier: String) -> (Arc<str>, LayerId, Option<Task>) {
        let mut sources = lock(&self.sources);
        if let Some((identifier, &id)) = sources.get_key_value(identifier.as_str()) {
            return (Arc::clone(identifier), id, None);
        }
        let identifier: Arc<str> = Arc::from(identifier);
        let provisional = LayerId(PROVISIONAL + sources.len() as u64);
        sources.insert(Arc::clone(&identifier), provisional);
        let task = Task {
            identifier: Arc::clone(&identifier),
            provisional,
        };
        (identifier, provisional, Some(task))
    }

    fn read(&self, task: Task, added: &mut Added<'_, Task>) -> (LayerId, Read) {
        let mut read = Read {
            calls: Vec::new(),
            outcome: Outcome::Package,
        };
        let format = self.formats.for_identifier(&task.identifier);
        let bytes = if format.is_none_or(FileFormat::reads_source) {
            let (outer, _) = match super::format::split_args(&task.identifier) {
                Ok(split) => split,
                Err(error) => {
                    read.outcome = Outcome::Unread(error);
                    return (task.provisional, read);
                }
            };
            match self.storage.read_encoded(outer) {
                Ok(bytes) => bytes,
                Err(error) => {
                    read.outcome = Outcome::Unread(error.at(outer));
                    return (task.provisional, read);
                }
            }
        } else {
            layerstack::encoded_bytes::EncodedBytes::from(Vec::new())
        };
        let format = source_format(self.formats, &task.identifier, &bytes);
        if format == Format::Usdz {
            return (task.provisional, read);
        }
        let mut resolver = TaskResolver {
            shared: self,
            anchor: &task.identifier,
            calls: Vec::new(),
            asked: HashMap::new(),
            added,
            package: false,
        };
        let loose = read_loose(
            format,
            &bytes,
            &task.identifier,
            task.provisional,
            self.tokens,
            self.paths,
            &mut resolver,
            self.schemas,
            self.usdc_options,
            self.formats,
            self.threads,
        );
        if !resolver.package {
            read.calls = resolver.calls;
            read.outcome = Outcome::Read {
                format,
                read: alloc::boxed::Box::new(loose),
            };
        }
        (task.provisional, read)
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
        let (identifier, provisional, task) = self.shared.source(identifier);
        if let Some(task) = task {
            self.added.push(task);
            self.added.offer();
        }
        self.calls.push(Call::Target {
            asset: Arc::clone(asset),
            identifier: Arc::clone(&identifier),
            provisional,
        });
        Ok(ResolvedAsset {
            layer_id: provisional,
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
                Call::Target {
                    identifier,
                    provisional,
                    ..
                } => Ok(ResolvedAsset {
                    layer_id: *provisional,
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
        self.shared.formats.is_dynamic(asset_path)
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
            threads: &self.threads,
            identities: Mutex::new(self.identities.clone()),
            sources: Mutex::default(),
        };
        let (identifier, root_id, root) = shared.source(identifier);
        let work = |task: Task, added: &mut Added<'_, Task>| shared.read(task, added);
        let reads: HashMap<LayerId, Read> = with_offering_pool_in(&self.threads, &work, |pool| {
            pool.run(root.into_iter().collect())
        })
        .into_iter()
        .collect();
        if reads
            .values()
            .any(|read| matches!(read.outcome, Outcome::Package))
        {
            return None;
        }
        self.identities = shared.identities.into_inner().unwrap_or_default();
        let first_pending = self.pending.len();
        let first_diagnostic = self.diagnostics.len();
        let mut replay = Replay {
            reader: self,
            reads,
            ids: HashMap::new(),
            failed: HashSet::new(),
        };
        let result = replay.visit(&identifier, root_id);
        let Replay {
            ids, failed, reads, ..
        } = replay;
        let final_id = |id: LayerId| {
            if id.0 < PROVISIONAL || id == LayerId::UNRESOLVED {
                id
            } else if failed.contains(&id) {
                LayerId::UNRESOLVED
            } else {
                ids.get(&id).copied().unwrap_or(LayerId::UNRESOLVED)
            }
        };
        // The reads are dropped, and the layers they hold are rewritten, on
        // the threads the reading used.
        let threads = &self.threads;
        let pending = &mut self.pending[first_pending..];
        drop_and_map(threads, reads, pending, &final_id);
        // A diagnostic names the layer read, even one that failed, and so
        // do those a strict rejection carries.
        let rename = |diagnostics: &mut [layerstack_usdz::MemberDiagnostic]| {
            for diagnostic in diagnostics {
                if let Some(&id) = ids.get(&diagnostic.layer_id) {
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
                layer.map_layer_ids(final_id);
                layer
            }),
        }))
    }
}

/// What finishing a read on threads does, in tasks.
enum Finish<'l> {
    /// Drops some of the reads replayed.
    Drop(Vec<Read>),
    /// Rewrites the layer ids that some of the layers read name.
    Map(&'l mut [Layer]),
}

/// Drops `reads` and rewrites the provisional layer ids of `layers` with
/// `map`, on the threads `threads` spares: each read holds the arcs its
/// layer resolved, and dropping or walking several thousand of them
/// would otherwise leave every thread but one idle. Each layer's change
/// history, which records every spec its reader inserted and the
/// rewriting, is forgotten there too, as committing a layer forgets it
/// ([`Layer::forget_change_history`]), so freeing it does not wait for
/// the one thread committing.
fn drop_and_map(
    threads: &Threads,
    reads: HashMap<LayerId, Read>,
    layers: &mut [Layer],
    map: &(dyn Fn(LayerId) -> LayerId + Sync),
) {
    let map_layer = |layer: &mut Layer| {
        layer.map_layer_ids(map);
        layer.forget_change_history();
    };
    if threads.spare() == 0 {
        drop(reads);
        layers.iter_mut().for_each(map_layer);
        return;
    }
    let mut tasks: Vec<Finish<'_>> = layers
        .chunks_mut(super::LAYERS_PER_TASK)
        .map(Finish::Map)
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
        Finish::Map(layers) => layers.iter_mut().for_each(map_layer),
    };
    layerstack::parallel::with_pool_in(threads, &work, |pool| pool.run(tasks));
}

/// The single-threaded pass recording the sources read on threads.
struct Replay<'r, 'a, B> {
    reader: &'r mut Reader<'a, B>,
    reads: HashMap<LayerId, Read>,
    /// The final id of each provisional one.
    ids: HashMap<LayerId, LayerId>,
    /// The provisional ids of sources that failed to read.
    failed: HashSet<LayerId>,
}

impl<B: Storage> Replay<'_, '_, B> {
    /// Records the source `identifier`, read as `provisional`, as
    /// `Reader::load_external` loads it once identified: its id, then its
    /// arcs' targets in order, then its diagnostics.
    fn visit(
        &mut self,
        identifier: &Arc<str>,
        provisional: LayerId,
    ) -> Result<(LayerId, Option<Layer>), IoError> {
        let catalog = &mut *self.reader.catalog;
        let id = if let Some(&id) = catalog.ids.get(&**identifier) {
            id
        } else {
            let id = catalog.allocate()?;
            catalog.ids.insert(Arc::clone(identifier), id);
            catalog.sources.insert(
                id,
                Source {
                    identifier: Arc::clone(identifier),
                    format: Format::Usda,
                    package: None,
                },
            );
            id
        };
        self.ids.insert(provisional, id);
        if catalog.states.contains_key(&id) {
            return Ok((id, None));
        }
        let read = self.reads.get_mut(&provisional).expect("read source");
        let format = match &read.outcome {
            Outcome::Unread(error) => {
                self.failed.insert(provisional);
                return Err(error.clone());
            }
            Outcome::Read { format, .. } => *format,
            Outcome::Package => unreachable!("packages are read on one thread"),
        };
        catalog.sources.get_mut(&id).expect("source").format = format;
        catalog.states.insert(id, State::Loading);
        let first_failure = self.reader.failures.len();
        let first_diagnostic = self.reader.diagnostics.len();
        // A source that failed to parse is read again each time an arc asks
        // for it, so its arcs and diagnostics are recorded again too.
        let calls = core::mem::take(&mut read.calls);
        let mut children = Vec::new();
        for call in &calls {
            self.call(call, id, &mut children);
        }
        let read = self.reads.get_mut(&provisional).expect("read source");
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
            self.reader.catalog.states.remove(&id);
            self.failed.insert(provisional);
            return Err(error);
        }
        let layer = core::mem::replace(
            &mut loose.layer,
            Err(IoError::new(IoErrorKind::Rejected, "layer recorded")),
        );
        self.reader.catalog.states.insert(id, State::Loaded);
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
                provisional,
            } => (asset, self.visit(identifier, *provisional)),
        };
        let bound = match result {
            Ok((id, layer)) => {
                children.extend(layer);
                id
            }
            Err(error) => {
                self.reader.failures.push(AssetFailure {
                    asset: String::from(&**asset),
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
