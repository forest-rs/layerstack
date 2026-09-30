// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Taxonomy discovery and snapshot-owned semantic label queries.

use crate::{PrimView, Scene, Time, usd_semantics::SemanticsLabelsApi};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use layerstack::PathId;

/// An interval of numeric stage times for label queries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelInterval {
    start: f64,
    end: f64,
    include_start: bool,
    include_end: bool,
}

impl LabelInterval {
    /// Creates an interval, optionally excluding either endpoint.
    /// Infinite bounds are supported for unbounded intervals. NaN,
    /// reversed bounds, an open zero-width interval, and singleton
    /// intervals at either infinity return [`LabelQueryError::EmptyInterval`].
    ///
    /// # Errors
    /// Returns an error if the interval is empty or has NaN bounds.
    pub fn new(
        start: f64,
        end: f64,
        include_start: bool,
        include_end: bool,
    ) -> Result<Self, LabelQueryError> {
        if start.is_nan()
            || end.is_nan()
            || start > end
            || (start == end && (!start.is_finite() || !(include_start && include_end)))
        {
            return Err(LabelQueryError::EmptyInterval);
        }
        Ok(Self {
            start,
            end,
            include_start,
            include_end,
        })
    }

    fn contains(self, time: f64) -> bool {
        (time > self.start || (self.include_start && time == self.start))
            && (time < self.end || (self.include_end && time == self.end))
    }
}

/// Invalid semantic label query configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelQueryError {
    /// A taxonomy must have a nonempty name.
    EmptyTaxonomy,
    /// Interval bounds are empty, reversed or NaN.
    EmptyInterval,
}

impl core::fmt::Display for LabelQueryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::EmptyTaxonomy => "a label query needs a nonempty taxonomy",
            Self::EmptyInterval => "a label query needs a nonempty interval of stage times",
        })
    }
}

impl core::error::Error for LabelQueryError {}

#[derive(Clone, Copy, Debug)]
enum QueryTime {
    At(Time),
    Interval(LabelInterval),
}

/// A retained label query for one taxonomy and one immutable scene snapshot.
///
/// Direct results for prims with the taxonomy applied are cached by prim;
/// unlabeled prims do not allocate cache entries. The borrowed [`Scene`] ties both the
/// composed stage and its store to the query. Ordinary mutation requires
/// dropping the query first; custom stores must preserve the snapshot's
/// token and path identities. Construct a new query for the resulting
/// snapshot after edits; results never cross stages implicitly.
///
/// Labels are sorted and deduplicated. Inherited labels union the prim's
/// labels with all ancestors' labels; they never override ancestor labels.
/// OpenUSD: `UsdSemanticsLabelsQuery` in `usdSemantics/labelsQuery.cpp`.
///
/// ```
/// use layerstack::PathId;
/// use layerstack_schemas::{LabelsQuery, Scene, Time};
/// # fn labels(scene: Scene<'_>, path: PathId) {
/// let mut query = LabelsQuery::new(scene, "role", Time::at(10.0)).unwrap();
/// let inherited = query.inherited_labels(path);
/// let is_subject = query.has_inherited_label(path, "subject");
/// # let _ = (inherited, is_subject);
/// # }
/// ```
#[derive(Debug)]
pub struct LabelsQuery<'a> {
    scene: Scene<'a>,
    taxonomy: String,
    time: QueryTime,
    direct: BTreeMap<PathId, Vec<&'a str>>,
}

impl<'a> LabelsQuery<'a> {
    /// Queries labels at a single time, including the default time.
    ///
    /// # Errors
    /// Returns [`LabelQueryError::EmptyTaxonomy`] for an empty taxonomy.
    pub fn new(scene: Scene<'a>, taxonomy: &str, time: Time) -> Result<Self, LabelQueryError> {
        Self::with_time(scene, taxonomy, QueryTime::At(time))
    }

    /// Queries the union of labels over a numeric interval.
    ///
    /// The interval's lower endpoint is always evaluated, even if open,
    /// matching OpenUSD's held-value handling. All sample times inside the
    /// interval are also evaluated; an unbounded lower endpoint evaluates
    /// the earliest-time value. Unlike OpenUSD's diagnostic-and-default
    /// behavior for an empty interval, [`LabelInterval::new`] rejects it.
    /// Sampled blocks use the schema fallback empty label set under AOUSD
    /// Core §12.3.6/§16.2.16.3; labels from other times remain in the union.
    /// OpenUSD26.8 instead discards the entire union after a sampled block
    /// (`sampled-block-drops-fallback`).
    ///
    /// # Errors
    /// Returns [`LabelQueryError::EmptyTaxonomy`] for an empty taxonomy.
    pub fn in_interval(
        scene: Scene<'a>,
        taxonomy: &str,
        interval: LabelInterval,
    ) -> Result<Self, LabelQueryError> {
        Self::with_time(scene, taxonomy, QueryTime::Interval(interval))
    }

    fn with_time(
        scene: Scene<'a>,
        taxonomy: &str,
        time: QueryTime,
    ) -> Result<Self, LabelQueryError> {
        if taxonomy.is_empty() {
            return Err(LabelQueryError::EmptyTaxonomy);
        }
        Ok(Self {
            scene,
            taxonomy: taxonomy.into(),
            time,
            direct: BTreeMap::new(),
        })
    }

    fn populate(&mut self, path: PathId) -> bool {
        if self.direct.contains_key(&path) {
            return true;
        }
        let mut labels = Vec::new();
        if self.scene.parent(path).is_none() {
            return false;
        }
        let Some(api) = SemanticsLabelsApi::get(&self.scene, path, &self.taxonomy) else {
            return false;
        };
        match self.time {
            QueryTime::At(time) => {
                labels = match time {
                    Time::Default => api.labels(),
                    Time::At {
                        code,
                        interpolation,
                    } => api.labels_at(code, interpolation),
                }
                .unwrap_or_default();
            }
            QueryTime::Interval(interval) => {
                let name = alloc::format!("semantics:labels:{}", self.taxonomy);
                let prim = PrimView::new(self.scene, path);
                let mut times = Vec::new();
                if let Some(property) = prim.property_path(&name)
                    && let Some(opinions) = self.scene.stage().explain_property_path(property)
                {
                    // A conservative superset of visible array sample
                    // knots is sufficient for held token arrays: extra
                    // masked knots cannot add a label to the result.
                    // AOUSD Core §12.3.2.1 applies source layer offsets.
                    for opinion in opinions {
                        if let Some(samples) = opinion.value.time_samples() {
                            times.extend(
                                samples
                                    .iter()
                                    .map(|(t, _)| {
                                        t * opinion.layer_offset.scale + opinion.layer_offset.offset
                                    })
                                    .filter(|t| interval.contains(*t)),
                            );
                        }
                    }
                }
                times.push(if interval.start.is_finite() {
                    interval.start
                } else {
                    -f64::MAX
                });
                times.sort_by(f64::total_cmp);
                times.dedup_by(|a, b| *a == *b);
                for time in times {
                    // If any evaluation fails, OpenUSD discards the
                    // complete interval union, rather than a partial set.
                    let Some(at_time) = api.labels_at(time, layerstack::InterpolationType::Held)
                    else {
                        labels.clear();
                        break;
                    };
                    labels.extend(at_time);
                }
            }
        }
        labels.sort_unstable();
        labels.dedup();
        self.direct.insert(path, labels);
        true
    }

    /// The sorted unique labels directly authored on this prim's taxonomy.
    /// A pseudo-root, missing API or unreadable value yields an empty set.
    pub fn direct_labels(&mut self, path: PathId) -> &[&'a str] {
        if self.populate(path) {
            &self.direct[&path]
        } else {
            &[]
        }
    }

    /// The sorted unique union of labels on this prim and its ancestors.
    /// A path absent from the stage yields an empty set.
    pub fn inherited_labels(&mut self, path: PathId) -> Vec<&'a str> {
        if !self.scene.stage().has_prim(path) {
            return Vec::new();
        }
        let mut labels = Vec::new();
        let mut at = Some(path);
        while let Some(path) = at {
            if self.populate(path) {
                labels.extend_from_slice(&self.direct[&path]);
            }
            at = self.scene.parent(path);
        }
        labels.sort_unstable();
        labels.dedup();
        labels
    }

    /// Whether the prim has `label` directly in this taxonomy.
    pub fn has_direct_label(&mut self, path: PathId, label: &str) -> bool {
        self.direct_labels(path).binary_search(&label).is_ok()
    }

    /// Whether the prim or an ancestor has `label` in this taxonomy.
    pub fn has_inherited_label(&mut self, path: PathId, label: &str) -> bool {
        if !self.scene.stage().has_prim(path) {
            return false;
        }
        let mut at = Some(path);
        while let Some(path) = at {
            if self.has_direct_label(path, label) {
                return true;
            }
            at = self.scene.parent(path);
        }
        false
    }
}

impl<'a> Scene<'a> {
    /// Taxonomies applied directly on a prim, in applied-schema order.
    /// OpenUSD: `UsdSemanticsLabelsAPI::GetDirectTaxonomies`.
    #[must_use]
    pub fn direct_taxonomies(&self, path: PathId) -> Vec<&'a str> {
        if self.parent(path).is_none() {
            return Vec::new();
        }
        self.instances(path, SemanticsLabelsApi::SCHEMA)
    }

    /// Sorted unique taxonomies on this prim and its ancestors.
    /// A path absent from the stage yields an empty set.
    /// OpenUSD: `UsdSemanticsLabelsAPI::ComputeInheritedTaxonomies`.
    #[must_use]
    pub fn inherited_taxonomies(&self, path: PathId) -> Vec<&'a str> {
        if !self.stage().has_prim(path) {
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut at = Some(path);
        while let Some(path) = at {
            result.extend(self.direct_taxonomies(path));
            at = self.parent(path);
        }
        result.sort_unstable();
        result.dedup();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlabeled_paths_do_not_fill_the_cache() {
        let mut store = layerstack::InMemoryStore::default();
        let path = store.path("/Empty");
        store.insert_layer(layerstack::Layer::new(layerstack::LayerId(1)));
        let stage = layerstack::Stage::compose(
            &mut store,
            layerstack::LayerId(1),
            layerstack::StageOptions::default(),
        );
        let mut query =
            LabelsQuery::new(Scene::new(&stage, &store), "kind", Time::Default).unwrap();
        assert!(query.direct_labels(path).is_empty());
        assert!(query.inherited_labels(path).is_empty());
        assert!(query.direct.is_empty());
    }

    #[test]
    fn intervals_reject_empty_and_nan() {
        assert_eq!(
            LabelInterval::new(3.0, 2.0, true, true),
            Err(LabelQueryError::EmptyInterval)
        );
        assert_eq!(
            LabelInterval::new(f64::NAN, 2.0, true, true),
            Err(LabelQueryError::EmptyInterval)
        );
        assert_eq!(
            LabelInterval::new(2.0, 2.0, true, false),
            Err(LabelQueryError::EmptyInterval)
        );
        for at in [f64::NEG_INFINITY, f64::INFINITY] {
            assert_eq!(
                LabelInterval::new(at, at, true, true),
                Err(LabelQueryError::EmptyInterval)
            );
        }
        assert!(LabelInterval::new(2.0, 2.0, true, true).is_ok());
        assert!(LabelInterval::new(f64::NEG_INFINITY, f64::INFINITY, false, false).is_ok());
    }
}
