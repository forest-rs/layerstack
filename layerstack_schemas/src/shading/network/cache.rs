// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Caller-owned immutable graph retention with observable work and revisions.
use super::*;

/// Cache-local component versions. They describe captured USD data, never GPU
/// execution or changed texture bytes at an unchanged authored asset identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaterialRevisions {
    /// Terminal/context, nodes, declarations, identifiers or connections changed.
    pub topology: u64,
    /// Captured constant storage, defaults, decode state or primvar names changed.
    pub parameters: u64,
    /// Authored asset spellings or winning authoring provenance changed.
    pub resources: u64,
}
/// Observable retained work. Counts are requests/records, not heap-byte estimates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaterialCacheStats {
    /// Requests including invalid requests.
    pub requests: u64,
    /// Requests reusing the exact immutable graph handle.
    pub cache_hits: u64,
    /// Graph capture attempts, including failures.
    pub captures: u64,
    /// Dependency snapshots/query stamps checked during retained requests.
    pub dependency_checks: u64,
    /// Current retained material/terminal records.
    pub records: usize,
}
/// An immutable handoff that remains valid after cache updates or removal.
#[derive(Clone, Debug)]
pub struct MaterialNetworkSample {
    /// Owned upstream graph; cache hits reuse this exact Arc identity.
    pub network: Arc<MaterialNetwork>,
    /// Component revisions scoped to this cache instance.
    pub revisions: MaterialRevisions,
    /// Whether this request captured USD rather than reusing a current graph.
    pub evaluated: bool,
}
impl MaterialNetworkSample {
    /// Bounded typed Preview Surface handoff retaining this exact graph handle.
    /// No store lookup, texture decoding or shader execution occurs.
    #[must_use]
    pub fn preview_surface(
        &self,
        max_depth: usize,
    ) -> crate::shading::preview_surface::PreviewSurfaceNetwork {
        crate::shading::preview_surface::PreviewSurfaceNetwork::capture(
            self.network.clone(),
            max_depth,
        )
    }
}
struct Held {
    network: Arc<MaterialNetwork>,
    evidence: Evidence,
    revisions: MaterialRevisions,
    dirty: bool,
}
/// Retains one graph per material/terminal in one token/path domain. Contexts
/// and time are explicit. Every request checks immutable prim/query identity,
/// including missing targets and clip dependencies: omitted notices, expired
/// history and replacement stages cannot silently reuse old data. A changed
/// dependency recaptures that material; unrelated materials retain their handles.
/// `apply_changes` is optional eager routing. No scene is borrowed across edits.
/// Clear before switching domains. External resource content versions belong to
/// the host; an unchanged spelling/source does not imply unchanged texture bytes.
pub struct MaterialNetworkCache {
    time: Time,
    contexts: Vec<String>,
    identity: Option<StoreIdentity>,
    held: BTreeMap<(PathId, u8), Held>,
    next_revision: u64,
    stats: MaterialCacheStats,
}
impl core::fmt::Debug for MaterialNetworkCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MaterialNetworkCache")
            .field("time", &self.time)
            .field("contexts", &self.contexts)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}
fn terminal_key(t: MaterialTerminal) -> u8 {
    match t {
        MaterialTerminal::Surface => 0,
        MaterialTerminal::Displacement => 1,
        MaterialTerminal::Volume => 2,
    }
}
impl MaterialNetworkCache {
    /// Empty cache with explicit evaluation time and ordered render contexts.
    #[must_use]
    pub fn new(time: Time, contexts: &[&str]) -> Self {
        Self {
            time,
            contexts: contexts.iter().map(|s| (*s).into()).collect(),
            identity: None,
            held: BTreeMap::new(),
            next_revision: 1,
            stats: MaterialCacheStats::default(),
        }
    }
    /// Current time; numeric reads use the selected interpolation policy.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }
    /// Changes explicit evaluation time, lazily recapturing held materials.
    /// Parameter/resource stamps stay stable if selected values are unchanged.
    pub fn set_time(&mut self, time: Time) {
        if self.time != time {
            self.time = time;
            for h in self.held.values_mut() {
                h.dirty = true;
            }
        }
    }
    /// Changes ordered render contexts and lazily reselects terminals.
    pub fn set_render_contexts(&mut self, contexts: &[&str]) {
        let c: Vec<String> = contexts.iter().map(|s| (*s).into()).collect();
        if c != self.contexts {
            self.contexts = c;
            for h in self.held.values_mut() {
                h.dirty = true;
            }
        }
    }
    /// Releases captures and store affinity. Revision numbers remain unreused.
    pub fn clear(&mut self) {
        self.held.clear();
        self.identity = None;
    }
    /// Releases all terminal records for one material. Old Arc handoffs survive.
    pub fn remove(&mut self, material: PathId) -> bool {
        let before = self.held.len();
        self.held.retain(|(p, _), _| *p != material);
        before != self.held.len()
    }
    /// Work counters and current record count.
    #[must_use]
    pub fn stats(&self) -> MaterialCacheStats {
        MaterialCacheStats {
            records: self.held.len(),
            ..self.stats
        }
    }
    /// Resets cumulative work counters without releasing captures/revisions.
    pub fn reset_stats(&mut self) {
        self.stats = MaterialCacheStats::default();
    }
    /// Marks captures touched by complete change evidence. Snapshots are still
    /// checked on every `get`, so missing/expired reports need no special repair.
    /// Rejects foreign domains before interpreting any store-local change IDs.
    pub fn apply_changes(
        &mut self,
        scene: &Scene<'_>,
        changes: &Changes,
    ) -> Result<(), MaterialCaptureError> {
        self.check_domain(scene)?;
        for held in self.held.values_mut() {
            held.dirty |= held.network.dependencies.iter().any(|dep| {
                changes.changed_info_only.contains(&dep.prim)
                    || changes
                        .created
                        .iter()
                        .chain(&changes.removed)
                        .chain(&changes.resynced)
                        .any(|root| {
                            scene
                                .store()
                                .paths()
                                .resolve(dep.prim)
                                .strip_prefix(scene.store().paths().resolve(*root))
                                .is_some()
                        })
            });
        }
        Ok(())
    }
    fn check_domain(&mut self, scene: &Scene<'_>) -> Result<(), MaterialCaptureError> {
        let identity = scene.store().identity();
        if scene.stage().store_identity() != Some(&identity) {
            return Err(MaterialCaptureError::SceneDomain);
        }
        if self.identity.as_ref().is_some_and(|id| *id != identity) {
            return Err(MaterialCaptureError::CacheDomain);
        }
        self.identity = Some(identity);
        Ok(())
    }
    fn revision(&mut self) -> u64 {
        let r = self.next_revision;
        self.next_revision = r.checked_add(1).expect("material revision space exhausted");
        r
    }
    /// Gets a retained immutable graph. Shared-node changes refresh every
    /// dependent material on its next request. Invalid/missing material errors
    /// release its current record; deletion/recreation receives fresh revisions.
    pub fn get(
        &mut self,
        scene: &Scene<'_>,
        material: PathId,
        terminal: MaterialTerminal,
    ) -> Result<MaterialNetworkSample, MaterialCaptureError> {
        self.stats.requests += 1;
        self.check_domain(scene)?;
        if matches!(self.time,Time::At {code,..} if !code.is_finite()) {
            return Err(MaterialCaptureError::InvalidTime);
        }
        let key = (material, terminal_key(terminal));
        if let Some(h) = self.held.get(&key) {
            self.stats.dependency_checks +=
                (h.evidence.prims.len() + h.evidence.queries.len()) as u64;
            if !h.dirty && h.evidence.current(scene, self.time) {
                self.stats.cache_hits += 1;
                return Ok(MaterialNetworkSample {
                    network: h.network.clone(),
                    revisions: h.revisions,
                    evaluated: false,
                });
            }
        }
        self.stats.captures += 1;
        let contexts: Vec<_> = self.contexts.iter().map(String::as_str).collect();
        let (network, evidence) = match MaterialNetwork::capture_evidence(
            scene, material, terminal, &contexts, self.time,
        ) {
            Ok(v) => v,
            Err(e) => {
                self.held.remove(&key);
                return Err(e);
            }
        };
        let old = self.held.remove(&key);
        let mut revisions = old
            .as_ref()
            .map_or(MaterialRevisions::default(), |h| h.revisions);
        if old
            .as_ref()
            .is_none_or(|h| !same_topology(&h.network, &network))
        {
            revisions.topology = self.revision();
        }
        if old
            .as_ref()
            .is_none_or(|h| !same_parameters(&h.network, &network))
        {
            revisions.parameters = self.revision();
        }
        if old
            .as_ref()
            .is_none_or(|h| h.network.resources != network.resources)
        {
            revisions.resources = self.revision();
        }
        let network = Arc::new(network);
        self.held.insert(
            key,
            Held {
                network: network.clone(),
                evidence,
                revisions,
                dirty: false,
            },
        );
        Ok(MaterialNetworkSample {
            network,
            revisions,
            evaluated: true,
        })
    }
}
fn same_topology(a: &MaterialNetwork, b: &MaterialNetwork) -> bool {
    a.source.context == b.source.context
        && a.source.shader == b.source.shader
        && a.source.trace == b.source.trace
        && a.nodes.len() == b.nodes.len()
        && a.nodes.iter().zip(&b.nodes).all(|(a, b)| {
            a.path == b.path
                && a.type_name == b.type_name
                && a.identifier == b.identifier
                && a.implementation_source == b.implementation_source
                && a.is_shader == b.is_shader
                && a.is_container == b.is_container
                && a.ports.len() == b.ports.len()
                && a.ports.iter().zip(&b.ports).all(|(a, b)| {
                    a.name == b.name
                        && a.kind == b.kind
                        && a.property_type == b.property_type
                        && a.connected == b.connected
                        && a.providers == b.providers
                })
        })
}
fn same_parameters(a: &MaterialNetwork, b: &MaterialNetwork) -> bool {
    fn value(a: &MaterialValue, b: &MaterialValue) -> bool {
        a.value == b.value && a.text == b.text && a.decode_error == b.decode_error
    }
    a.primvars == b.primvars
        && a.nodes.len() == b.nodes.len()
        && a.nodes.iter().zip(&b.nodes).all(|(a, b)| {
            a.ports.len() == b.ports.len()
                && a.ports.iter().zip(&b.ports).all(|(a, b)| {
                    value(&a.own, &b.own)
                        && a.node_default == b.node_default
                        && a.provider_values.len() == b.provider_values.len()
                        && a.provider_values
                            .iter()
                            .zip(&b.provider_values)
                            .all(|(a, b)| value(a, b))
                        && a.constant.as_ref().map(|v| v.origin)
                            == b.constant.as_ref().map(|v| v.origin)
                })
        })
}
