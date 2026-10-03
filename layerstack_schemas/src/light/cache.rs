// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Caller-owned retained light inputs and component upload revisions.
use super::{
    LightCaptureError, LightInput, LightInputs, LightListError, LightListMode, LightListStats,
    LightValue,
};
use crate::{Scene, Time, XformCache};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use layerstack::{Changes, PathId, PropertyField, TargetPath};

/// Cache-local component revisions. Compare only within the same cache instance.
/// Revisions identify capture changes, not actual GPU submission/execution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightRevisions {
    /// World matrix or inherited visibility changed.
    pub transform: u64,
    /// Input values, providers, declarations, light kind or stage units changed.
    pub parameters: u64,
    /// Selected shader ID/context changed.
    pub shader: u64,
    /// Forwarded filters, geometry, portals or non-port attributes changed.
    /// This does not version a baked light-link mask; see `LightLinkMembership`.
    pub relationships: u64,
}
/// Observable work performed since construction or stats reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightCacheStats {
    /// Capture requests, including failures.
    pub requests: u64,
    /// Requests answered without recapturing inputs.
    pub cache_hits: u64,
    /// Full input captures, including failed captures.
    pub captures: u64,
    /// Discovery requests, including missing roots.
    pub discovery_requests: u64,
    /// Discovery requests answered by retained lists.
    pub discovery_hits: u64,
    /// Prims visited by uncached discovery traversals.
    pub visited_prims: usize,
    /// Retained records examined when routing scene changes.
    pub routed: u64,
    /// Existing records made dirty by scene/time changes.
    pub invalidated: u64,
}
/// Retained storage counts. These are work-item counts, not heap-byte estimates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightCacheMemory {
    /// Retained emitter records.
    pub lights: usize,
    /// Retained root/mode discovery lists.
    pub discovery_entries: usize,
    /// Targets retained across discovery lists, counting overlap between lists.
    pub discovered_targets: usize,
    /// Composed input records, including custom/shaping/shadow inputs.
    pub inputs: usize,
    /// Provider records across all retained inputs.
    pub providers: usize,
    /// Retained property dependency records.
    pub dependencies: usize,
}
/// Borrowed retained capture with revisions for selective engine uploads.
#[derive(Clone, Copy, Debug)]
pub struct LightSample<'a> {
    /// Owned inputs retained by the cache until the next mutable operation.
    pub inputs: &'a LightInputs,
    /// Component change stamps, including nonzero stamps on first capture.
    pub revisions: LightRevisions,
    /// Whether this request captured scene state rather than reusing it.
    pub evaluated: bool,
}
#[derive(Debug)]
struct Held {
    inputs: LightInputs,
    revisions: LightRevisions,
    dirty: bool,
}
/// Retains captures for one stage/store pair, explicit time and renderer contexts.
///
/// Feed every complete successful edit report to `apply_changes` before querying.
/// Clear before changing scenes or after losing change history. The cache borrows
/// no scene, has no scheduler and follows namespace deletion/recreation. Context
/// changes are explicit and lazy. Assets, shaders, link masks and GPU buffers are
/// not executed/loaded/uploaded; engine adapters own those lifecycles.
#[derive(Debug)]
pub struct LightCache {
    time: Time,
    contexts: Vec<String>,
    transforms: XformCache,
    held: BTreeMap<PathId, Held>,
    lists: BTreeMap<(PathId, LightListMode), Vec<TargetPath>>,
    next_revision: u64,
    stats: LightCacheStats,
}
impl LightCache {
    /// Empty retained state for one evaluation time and ordered shader contexts.
    #[must_use]
    pub fn new(time: Time, contexts: &[&str]) -> Self {
        Self {
            time,
            contexts: contexts.iter().map(|s| (*s).into()).collect(),
            transforms: XformCache::new(time),
            held: BTreeMap::new(),
            lists: BTreeMap::new(),
            next_revision: 1,
            stats: LightCacheStats::default(),
        }
    }
    /// Current evaluation time.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }
    /// Changes time without invalidating static captures. Time is updated in
    /// every returned capture even when its component revisions stay stable.
    pub fn set_time(&mut self, time: Time) {
        if self.time != time {
            let default_changed =
                matches!(self.time, Time::Default) != matches!(time, Time::Default);
            self.time = time;
            self.transforms.set_time(time);
            for h in self.held.values_mut() {
                if (h.inputs.might_vary || (default_changed && h.inputs.default_time_sensitive))
                    && !h.dirty
                {
                    h.dirty = true;
                    self.stats.invalidated += 1;
                }
                h.inputs.time = time;
            }
        }
    }
    /// Changes shader selection contexts; captures are refreshed lazily.
    pub fn set_render_contexts(&mut self, contexts: &[&str]) {
        let contexts: Vec<String> = contexts.iter().map(|s| (*s).into()).collect();
        if self.contexts != contexts {
            self.contexts = contexts;
            for h in self.held.values_mut() {
                if !h.dirty {
                    h.dirty = true;
                    self.stats.invalidated += 1;
                }
            }
        }
    }
    /// Releases retained scene data. Monotonic revisions remain unreused so
    /// existing engine upload stamps cannot accidentally match recaptured data.
    pub fn clear(&mut self) {
        self.held.clear();
        self.lists.clear();
        self.transforms.clear();
    }
    /// Releases one emitter record. Returns whether one existed.
    pub fn remove(&mut self, light: PathId) -> bool {
        self.held.remove(&light).is_some()
    }
    /// Current request/routing/capture counts.
    #[must_use]
    pub fn stats(&self) -> LightCacheStats {
        self.stats
    }
    /// Resets only the work counters.
    pub fn reset_stats(&mut self) {
        self.stats = LightCacheStats::default();
    }
    /// Retained work-item counts, not an estimate of heap bytes.
    #[must_use]
    pub fn memory(&self) -> LightCacheMemory {
        let mut m = LightCacheMemory {
            lights: self.held.len(),
            discovery_entries: self.lists.len(),
            discovered_targets: self.lists.values().map(Vec::len).sum(),
            ..LightCacheMemory::default()
        };
        for h in self.held.values() {
            m.inputs += h.inputs.inputs.len();
            m.dependencies += h.inputs.dependencies.len();
            m.providers += h
                .inputs
                .inputs
                .iter()
                .map(|i| i.providers.sources.len())
                .sum::<usize>();
        }
        m
    }
    fn revision(&mut self) -> u64 {
        let v = self.next_revision;
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .expect("light revision exhausted");
        v
    }
    /// Discovers emitters and filters using retained lists. Structural changes
    /// retire lists; precise input-value edits preserve them. Model-cache queries
    /// conservatively retire on forwarded relationship target changes anywhere.
    pub fn discover(
        &mut self,
        scene: &Scene<'_>,
        root: PathId,
        mode: LightListMode,
    ) -> Result<&[TargetPath], LightListError> {
        self.stats.discovery_requests += 1;
        let key = (root, mode);
        if self.lists.contains_key(&key) {
            self.stats.discovery_hits += 1;
        } else {
            let mut work = LightListStats::default();
            let result = super::discover(*scene, root, mode, &mut work);
            self.stats.visited_prims += work.visited_prims;
            self.lists.insert(key, result?);
        }
        Ok(&self.lists[&key])
    }
    /// Borrows a retained capture. Failed requests are not cached, and retire
    /// any previous capture at that path. Shader requirements are successful,
    /// inspectable captures rather than errors for the whole emitter.
    pub fn capture(
        &mut self,
        scene: &Scene<'_>,
        light: PathId,
    ) -> Result<LightSample<'_>, LightCaptureError> {
        self.stats.requests += 1;
        let evaluated = self.held.get(&light).is_none_or(|h| h.dirty);
        if evaluated {
            self.stats.captures += 1;
            let contexts: Vec<_> = self.contexts.iter().map(String::as_str).collect();
            let inputs = match LightInputs::read_with_transforms(
                scene,
                light,
                self.time,
                &contexts,
                &mut self.transforms,
            ) {
                Ok(i) => i,
                Err(e) => {
                    self.held.remove(&light);
                    return Err(e);
                }
            };
            let mut revisions = self
                .held
                .get(&light)
                .map_or(LightRevisions::default(), |h| h.revisions);
            let (transform, parameters, shader, relationships) =
                self.held.get(&light).map_or((true, true, true, true), |h| {
                    let old = &h.inputs;
                    (
                        !same_matrix(&old.world_transform, &inputs.world_transform)
                            || old.visibility != inputs.visibility
                            || old.transform_problems != inputs.transform_problems,
                        old.kind != inputs.kind
                            || old.type_name != inputs.type_name
                            || old.meters_per_unit.to_bits() != inputs.meters_per_unit.to_bits()
                            || old.up_axis != inputs.up_axis
                            || old.color_space != inputs.color_space
                            || !same_inputs(&old.inputs, &inputs.inputs)
                            || !same_attributes(&old.attributes, &inputs.attributes),
                        old.shader != inputs.shader,
                        old.relationships != inputs.relationships
                            || old.relationship_issues != inputs.relationship_issues
                            || !same_attributes(&old.attributes, &inputs.attributes),
                    )
                });
            if transform {
                revisions.transform = self.revision();
            }
            if parameters {
                revisions.parameters = self.revision();
            }
            if shader {
                revisions.shader = self.revision();
            }
            if relationships {
                revisions.relationships = self.revision();
            }
            self.held.insert(
                light,
                Held {
                    inputs,
                    revisions,
                    dirty: false,
                },
            );
        } else {
            self.stats.cache_hits += 1;
        }
        let held = &self.held[&light];
        Ok(LightSample {
            inputs: &held.inputs,
            revisions: held.revisions,
            evaluated,
        })
    }
    /// Routes one complete report against the resulting scene. Unrelated
    /// properties preserve captures. New custom inputs and structural/type/API
    /// changes are conservative; upstream provider and forwarded-target edits
    /// are tracked even outside the emitter subtree. AOUSD Core §12–13;
    /// OpenUSD `ObjectsChanged` subtree versus info-only notices.
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &Changes) {
        self.transforms.apply_changes(scene, changes);
        let structural = !changes.resynced.is_empty()
            || !changes.created.is_empty()
            || !changes.removed.is_empty();
        let unknown = changes
            .changed_info_only
            .iter()
            .any(|p| changes.properties_for(*p).is_none());
        let cache_changed = changes
            .property_changes
            .iter()
            .flat_map(|p| &p.fields)
            .any(|f| {
                let name = scene.store().tokens().resolve(f.name);
                f.field == PropertyField::Targets || name == "lightList:cacheBehavior"
            });
        self.lists.retain(|(_, mode), _| {
            !structural && !unknown && (*mode == LightListMode::IgnoreCache || !cache_changed)
        });

        for (&light, h) in &mut self.held {
            self.stats.routed += 1;
            let structural = changes
                .resynced
                .iter()
                .chain(&changes.created)
                .chain(&changes.removed)
                .any(|root| {
                    let root = scene.store().paths().resolve(*root);
                    root.is_prefix_of(scene.store().paths().resolve(light))
                        || h.inputs
                            .dependencies
                            .iter()
                            .any(|d| root.is_prefix_of(scene.store().paths().resolve(d.prim)))
                        || h.inputs
                            .relationships
                            .iter()
                            .flat_map(|(_, targets)| targets)
                            .any(|target| {
                                let path = match target {
                                    TargetPath::Prim(p) => *p,
                                    TargetPath::Property(p) => p.prim_path(),
                                };
                                root.is_prefix_of(scene.store().paths().resolve(path))
                            })
                });
            let info = changes.changed_info_only.iter().any(|path| {
                if let Some(fields) = changes.properties_for(*path) {
                    fields.iter().any(|field| {
                        let name = scene.store().tokens().resolve(field.name);
                        (*path == light
                            && (name.starts_with("inputs:") || name == "light:shaderId"))
                            || h.inputs
                                .dependencies
                                .iter()
                                .any(|d| d.prim == *path && d.property == name)
                            || (h.inputs.ancestors.contains(path)
                                && (name.starts_with("xformOp:")
                                    || name == "xformOpOrder"
                                    || name == "visibility"))
                    })
                } else {
                    h.inputs.ancestors.contains(path)
                        || h.inputs.dependencies.iter().any(|d| d.prim == *path)
                }
            });
            if (structural || info) && !h.dirty {
                h.dirty = true;
                self.stats.invalidated += 1;
            }
        }
    }
}
fn same_matrix(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> bool {
    a.iter()
        .flatten()
        .zip(b.iter().flatten())
        .all(|(a, b)| a.to_bits() == b.to_bits())
}
fn same_value(a: &LightValue, b: &LightValue) -> bool {
    a.text == b.text
        && a.provenance == b.provenance
        && match (&a.value, &b.value) {
            (Some(a), Some(b)) => a.same_representation(b),
            (None, None) => true,
            _ => false,
        }
}
fn same_inputs(a: &[LightInput], b: &[LightInput]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.property == b.property
                && a.property_type == b.property_type
                && a.status == b.status
                && a.providers == b.providers
                && same_value(&a.own, &b.own)
                && a.provider_values.len() == b.provider_values.len()
                && a.provider_values
                    .iter()
                    .zip(&b.provider_values)
                    .all(|(a, b)| same_value(a, b))
        })
}
fn same_attributes(a: &[(String, LightValue)], b: &[(String, LightValue)]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((an, av), (bn, bv))| an == bn && same_value(av, bv))
}
