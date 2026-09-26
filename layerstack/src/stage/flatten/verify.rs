// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checking a flattened layer against the stage it was flattened from.

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::fmt::Write as _;

use super::{FindingKind, FlattenReport, Loss, ObjectPath, Transformation};
use crate::{
    doc::{InterpolationType, LayerStore, Value},
    interner::{TokenId, TokenInterner},
    path::{PathId, PathInterner, PropertyPath, TargetPath},
    prim_index::FieldKey,
    property::Variability,
    stage::{ResolvedValue, Stage, stage_time::map_leaves},
};

/// The outcome of [`Stage::verify_flattened`]: what was compared, and every
/// difference found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlattenVerification {
    /// What was compared.
    pub scope: VerifiedScope,
    /// Every difference, in traversal order.
    pub mismatches: Vec<Mismatch>,
}

impl FlattenVerification {
    /// Whether the flattened stage composes what the stage composes,
    /// everywhere [`FlattenVerification::scope`] covers.
    #[must_use]
    pub fn is_equivalent(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// What [`Stage::verify_flattened`] compared.
///
/// Every prim of the stage is compared: its specifier, type name, children
/// in order, every composed metadata field, and the fields with dedicated
/// members: `active`, `instanceable`, `reorder nameChildren` (as
/// `primOrder`) and `reorder properties` (as `propertyOrder`). Every composed property of
/// each prim is compared: its declaration (kind, type, variability,
/// `custom`), every metadata field, its targets or connections, its value
/// at the default time, and its value at each of its sample times in stage
/// time and at each of [`VerifiedScope::times`], interpolated linearly.
/// Nothing else is: splines are compared through their values at those
/// times only, and the prototypes a flatten adds are compared through the
/// instances that reference them. An asset path the report records as
/// anchored ([`Transformation::AssetPathAnchored`]) is expected anchored,
/// and a property declared as OpenUSD's flatten declares it
/// ([`Transformation::CustomFromWeakestOpinion`],
/// [`Transformation::DefinedBySchema`]) is expected declared so.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VerifiedScope {
    /// Prims compared.
    pub prims: usize,
    /// Properties compared.
    pub properties: usize,
    /// Prim and property metadata fields compared.
    pub metadata_fields: usize,
    /// Property values compared, one per property and time, the default
    /// time included.
    pub values: usize,
    /// Distinct sample times, in stage time, that some property was
    /// compared at, beside [`VerifiedScope::times`].
    pub sample_times: usize,
    /// The numeric times every property was compared at, as given.
    pub times: Vec<f64>,
    /// What was deliberately not compared, and why.
    pub skipped: Vec<Skipped>,
}

/// Something [`Stage::verify_flattened`] did not compare.
#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    /// The composed prim or property.
    pub path: ObjectPath,
    /// Why it was not compared.
    pub reason: SkipReason,
}

/// Why [`Stage::verify_flattened`] did not compare something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The report records it as lost, so the flattened layer does not hold
    /// it; its values and metadata are not compared.
    Lost(Loss),
}

/// A difference between the stage and the flattened stage.
#[derive(Clone, Debug, PartialEq)]
pub struct Mismatch {
    /// The composed prim or property.
    pub path: ObjectPath,
    /// What differs.
    pub kind: MismatchKind,
    /// What the stage composes, as text.
    pub expected: String,
    /// What the flattened stage composes, as text.
    pub found: String,
}

/// What a [`Mismatch`] is about.
#[derive(Clone, Debug, PartialEq)]
pub enum MismatchKind {
    /// A prim the stage composes and the flattened stage does not, or the
    /// reverse.
    Prim,
    /// The specifier.
    Specifier,
    /// The type name.
    TypeName,
    /// The children, in order.
    Children,
    /// A prim metadata field.
    Metadata {
        /// The field.
        field: String,
    },
    /// A property one stage composes and the other does not.
    Property,
    /// How a property is declared: kind, type, variability or `custom`.
    Declaration,
    /// A property metadata field.
    PropertyMetadata {
        /// The field.
        field: String,
    },
    /// Relationship targets or attribute connections.
    Targets,
    /// The value at the default time.
    Default,
    /// The value at a numeric time.
    ValueAt {
        /// The stage time.
        time: f64,
    },
}

impl Stage {
    /// Checks that `flattened`, a stage composed from the layer
    /// [`Stage::flatten`] wrote (as it is, or saved and read back), composes
    /// what this stage composes, and reports what was compared.
    ///
    /// Both stages must share `store`'s interners. `report` is the flatten's
    /// report: the prototypes it added are not prims of this stage, and
    /// what it lost is skipped. Every property is also compared at `times`
    /// (stage times) beside its own sample times. See [`VerifiedScope`] for
    /// exactly what is compared.
    ///
    /// ```
    /// use layerstack::{
    ///     InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions,
    ///     stage::flatten::FlattenRequirements,
    /// };
    ///
    /// let mut store = InMemoryStore::default();
    /// let tree = store.path("/Tree");
    /// let mut root = Layer::new(LayerId(1));
    /// root.insert_prim(tree, PrimSpec::def());
    /// store.insert_layer(root);
    /// let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    ///
    /// let flat = stage
    ///     .flatten(&mut store, LayerId(1), LayerId(2), &FlattenRequirements::default())
    ///     .unwrap();
    /// // Compose the flattened layer on its own (or a USDA or USDC copy of it,
    /// // read back into the same store).
    /// store.insert_layer(flat.layer);
    /// let flattened = Stage::compose(&mut store, LayerId(2), StageOptions::default());
    ///
    /// let verification = stage.verify_flattened(&flattened, &store, &flat.report, &[0.0, 10.0]);
    /// assert!(verification.is_equivalent());
    /// assert_eq!(verification.scope.prims, 1);
    /// assert!(verification.scope.skipped.is_empty());
    /// ```
    ///
    /// Spec: AOUSD Core §11 (stage population), §12 (value resolution).
    #[must_use]
    pub fn verify_flattened(
        &self,
        flattened: &Self,
        store: &dyn LayerStore,
        report: &FlattenReport,
        times: &[f64],
    ) -> FlattenVerification {
        let mut verifier = Verifier {
            source: self,
            flattened,
            paths: store.paths(),
            tokens: store.tokens(),
            store,
            prototypes: report.prototypes().map(String::from).collect(),
            skips: skips(report),
            anchors: anchors(report),
            declarations: declarations(report),
            out: FlattenVerification::default(),
            sample_times: Vec::new(),
        };
        verifier.out.scope.times = times.to_vec();
        verifier.run(times);
        let mut sample_times = verifier.sample_times;
        sample_times.sort_by(f64::total_cmp);
        sample_times.dedup();
        verifier.out.scope.sample_times = sample_times.len();
        verifier.out
    }
}

/// The asset paths the report records as anchored, by object path.
fn anchors(report: &FlattenReport) -> Vec<(String, Arc<str>, Arc<str>)> {
    report
        .findings
        .iter()
        .filter_map(|finding| match &finding.kind {
            FindingKind::Transformed(Transformation::AssetPathAnchored { authored, anchored }) => {
                Some((
                    finding.path.to_string(),
                    Arc::from(authored.as_str()),
                    Arc::from(anchored.as_str()),
                ))
            }
            _ => None,
        })
        .collect()
}

/// The properties the report records as declared other than the stage
/// declares them.
fn declarations(report: &FlattenReport) -> Vec<(String, bool, Option<Variability>)> {
    report
        .findings
        .iter()
        .filter_map(|finding| match &finding.kind {
            FindingKind::Transformed(Transformation::CustomFromWeakestOpinion { custom }) => {
                Some((finding.path.to_string(), *custom, None))
            }
            FindingKind::Transformed(Transformation::DefinedBySchema { variability }) => {
                Some((finding.path.to_string(), false, Some(*variability)))
            }
            _ => None,
        })
        .collect()
}

/// The paths the report says not to compare, with why.
fn skips(report: &FlattenReport) -> Vec<(String, SkipReason)> {
    let mut skips: Vec<(String, SkipReason)> = Vec::new();
    for finding in &report.findings {
        let reason = match &finding.kind {
            FindingKind::Lost(Loss::UnanchoredAssetPath) => continue,
            FindingKind::Lost(loss) => SkipReason::Lost(*loss),
            _ => continue,
        };
        let path = finding.path.to_string();
        if !skips.iter().any(|(skipped, _)| *skipped == path) {
            skips.push((path, reason));
        }
    }
    skips
}

struct Verifier<'a> {
    source: &'a Stage,
    flattened: &'a Stage,
    store: &'a dyn LayerStore,
    paths: &'a PathInterner,
    tokens: &'a TokenInterner,
    prototypes: Vec<String>,
    skips: Vec<(String, SkipReason)>,
    /// Each object's anchored asset paths: as authored, and as written.
    anchors: Vec<(String, Arc<str>, Arc<str>)>,
    /// Each property declared other than the stage declares it: its
    /// `custom` and, from a schema, its variability.
    declarations: Vec<(String, bool, Option<Variability>)>,
    out: FlattenVerification,
    sample_times: Vec<f64>,
}

impl Verifier<'_> {
    fn display(&self, path: PathId) -> String {
        self.paths.display(path, self.tokens)
    }

    fn is_prototype(&self, path: &str) -> bool {
        self.prototypes.iter().any(|prototype| {
            path == prototype
                || path
                    .strip_prefix(prototype.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }

    /// `value` of the stage at `path` as the flattened layer should hold
    /// it: with the asset paths the report records as anchored anchored.
    fn expected_value(&self, path: &str, value: Option<Value>) -> Option<Value> {
        let value = value?;
        let anchors: Vec<&(String, Arc<str>, Arc<str>)> =
            self.anchors.iter().filter(|(p, ..)| p == path).collect();
        if anchors.is_empty() {
            return Some(value);
        }
        let mapped = map_leaves(&value, &mut |leaf| match leaf {
            Value::Asset(asset) => anchors
                .iter()
                .find(|(_, authored, _)| authored == asset)
                .map(|(_, _, anchored)| Value::Asset(anchored.clone())),
            _ => None,
        });
        Some(mapped.unwrap_or(value))
    }

    fn expected(&self, path: &str, value: Option<ResolvedValue>) -> Option<ResolvedValue> {
        match value? {
            ResolvedValue::Scalar(value) => self
                .expected_value(path, Some(value))
                .map(ResolvedValue::Scalar),
            ResolvedValue::Dictionary(entries) => {
                match self.expected_value(path, Some(Value::Dictionary(entries)))? {
                    Value::Dictionary(entries) => Some(ResolvedValue::Dictionary(entries)),
                    other => Some(ResolvedValue::Scalar(other)),
                }
            }
            other => Some(other),
        }
    }

    fn skip_reason(&self, path: &str) -> Option<SkipReason> {
        self.skips
            .iter()
            .find(|(skipped, _)| skipped == path)
            .map(|(_, reason)| *reason)
    }

    fn mismatch(&mut self, path: &str, kind: MismatchKind, expected: String, found: String) {
        self.out.mismatches.push(Mismatch {
            path: ObjectPath::from_composed(path),
            kind,
            expected,
            found,
        });
    }

    fn compare(&mut self, path: &str, kind: MismatchKind, expected: String, found: String) {
        if expected != found {
            self.mismatch(path, kind, expected, found);
        }
    }

    fn run(&mut self, times: &[f64]) {
        let Some(root) = self.paths.lookup(&crate::path::Path::root()) else {
            return;
        };
        let source_prims: Vec<PathId> = self.source.traverse(root).filter(|&p| p != root).collect();
        for &prim in &source_prims {
            self.prim(prim, times);
        }
        let extra: Vec<PathId> = self
            .flattened
            .traverse(root)
            .filter(|&p| p != root && !self.source.has_prim(p))
            .collect();
        for prim in extra {
            let path = self.display(prim);
            if !self.is_prototype(&path) {
                self.mismatch(&path, MismatchKind::Prim, "no prim".into(), "a prim".into());
            }
        }
    }

    fn children(&self, stage: &Stage, prim: PathId) -> String {
        let names: Vec<String> = stage
            .children_of(prim)
            .unwrap_or_default()
            .iter()
            .map(|&child| self.display(child))
            .filter(|child| !self.is_prototype(child))
            .collect();
        format!("{names:?}")
    }

    /// The prim fields with dedicated members, as `stage` composes them:
    /// the strongest opinion of `active`, `instanceable` and
    /// `reorder nameChildren`, and the resolved `reorder properties`.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution), §7.6.2.2
    /// (`primChildren`, `propertyChildren` orders).
    fn dedicated(&self, stage: &Stage, prim: PathId) -> [(&'static str, String); 4] {
        let (mut active, mut instanceable, mut prim_order) = (None, None, None);
        if let Some(index) = stage.prims.get(&prim) {
            for source in &index.sources {
                let Some(spec) = self.store.layer(source.layer_id).and_then(|layer| {
                    layer.source_prim_spec(source.lookup_path, &source.spec_path, self.paths)
                }) else {
                    continue;
                };
                active = active.or(spec.active);
                instanceable = instanceable.or(spec.instanceable);
                if prim_order.is_none() {
                    prim_order.clone_from(&spec.prim_order);
                }
            }
        }
        let names = |order: Option<Vec<TokenId>>| {
            let names: Option<Vec<&str>> =
                order.map(|order| order.iter().map(|&t| self.tokens.resolve(t)).collect());
            format!("{names:?}")
        };
        [
            ("active", format!("{active:?}")),
            ("instanceable", format!("{instanceable:?}")),
            ("primOrder", names(prim_order)),
            (
                "propertyOrder",
                names(stage.resolve_property_order(prim, self.store)),
            ),
        ]
    }

    fn prim(&mut self, prim: PathId, times: &[f64]) {
        let path = self.display(prim);
        if !self.flattened.has_prim(prim) {
            self.mismatch(&path, MismatchKind::Prim, "a prim".into(), "no prim".into());
            return;
        }
        self.out.scope.prims += 1;
        let (source, flattened, store) = (self.source, self.flattened, self.store);
        self.compare(
            &path,
            MismatchKind::Specifier,
            format!("{:?}", source.resolve_specifier(prim, store)),
            format!("{:?}", flattened.resolve_specifier(prim, store)),
        );
        let type_name = |stage: &Stage| {
            stage
                .resolve_type_name(prim, store)
                .map_or_else(|| "none".into(), |t| String::from(self.tokens.resolve(t)))
        };
        let (want, got) = (type_name(source), type_name(flattened));
        self.compare(&path, MismatchKind::TypeName, want, got);
        let (want, got) = (self.children(source, prim), self.children(flattened, prim));
        self.compare(&path, MismatchKind::Children, want, got);
        let (want, got) = (
            self.dedicated(source, prim),
            self.dedicated(flattened, prim),
        );
        for ((field, want), (_, got)) in want.into_iter().zip(got) {
            self.out.scope.metadata_fields += 1;
            let field = String::from(field);
            self.compare(&path, MismatchKind::Metadata { field }, want, got);
        }

        let skipped_prim = self.skip_reason(&path);
        if let Some(reason) = skipped_prim {
            self.out.scope.skipped.push(Skipped {
                path: ObjectPath::from_composed(&path),
                reason,
            });
        } else {
            for key in metadata_keys(source, flattened, prim) {
                self.out.scope.metadata_fields += 1;
                let want = self.resolved(
                    self.expected(&path, source.resolve_value(prim, key).map(|r| r.value)),
                );
                let got = self.resolved(flattened.resolve_value(prim, key).map(|r| r.value));
                let field = String::from(self.tokens.resolve(key));
                self.compare(&path, MismatchKind::Metadata { field }, want, got);
            }
        }

        let names = source.property_names(prim, store);
        for &name in &names {
            self.property(prim, name, times);
        }
        for name in flattened.property_names(prim, store) {
            if !names.contains(&name) {
                let path = PropertyPath::new(prim, name).display(self.paths, self.tokens);
                self.mismatch(
                    &path,
                    MismatchKind::Property,
                    "none".into(),
                    "a property".into(),
                );
            }
        }
    }

    fn property(&mut self, prim: PathId, name: TokenId, times: &[f64]) {
        let property = PropertyPath::new(prim, name);
        let path = property.display(self.paths, self.tokens);
        let (source, flattened) = (self.source, self.flattened);
        if let Some(reason) = self.skip_reason(&path) {
            self.out.scope.skipped.push(Skipped {
                path: ObjectPath::from_composed(&path),
                reason,
            });
            return;
        }
        let Some(declaration) = source.resolve_property_declaration(prim, name) else {
            return;
        };
        let Some(found) = flattened.resolve_property_declaration(prim, name) else {
            self.mismatch(
                &path,
                MismatchKind::Property,
                "a property".into(),
                "none".into(),
            );
            return;
        };
        self.out.scope.properties += 1;
        let mut declaration = declaration;
        for (declared, custom, variability) in &self.declarations {
            if *declared == path {
                declaration.custom = *custom;
                if let Some(variability) = variability {
                    declaration.variability = *variability;
                }
            }
        }
        self.compare(
            &path,
            MismatchKind::Declaration,
            format!("{declaration:?}"),
            format!("{found:?}"),
        );

        for key in property_metadata_keys(source, flattened, prim, name) {
            self.out.scope.metadata_fields += 1;
            let want = self.resolved(
                self.expected(
                    &path,
                    source
                        .resolve_property_metadata(prim, name, key)
                        .map(|r| r.value),
                ),
            );
            let got = self.resolved(
                flattened
                    .resolve_property_metadata(prim, name, key)
                    .map(|r| r.value),
            );
            let field = String::from(self.tokens.resolve(key));
            self.compare(&path, MismatchKind::PropertyMetadata { field }, want, got);
        }

        // An empty composed list is written as no list.
        let targets = |stage: &Stage| {
            let targets = stage
                .resolve_target_list_path(property)
                .map(|r| r.value)
                .unwrap_or_default();
            self.targets(&targets)
        };
        let (want, got) = (targets(source), targets(flattened));
        self.compare(&path, MismatchKind::Targets, want, got);

        self.out.scope.values += 1;
        let want = self.resolved(self.expected(
            &path,
            source.resolve_property_path(property).map(|r| r.value),
        ));
        let got = self.resolved(flattened.resolve_property_path(property).map(|r| r.value));
        self.compare(&path, MismatchKind::Default, want, got);

        let mut at: Vec<f64> = sample_times(source, property);
        at.extend(sample_times(flattened, property));
        self.sample_times.extend(at.iter().copied());
        at.extend_from_slice(times);
        at.sort_by(f64::total_cmp);
        at.dedup();
        for time in at {
            self.out.scope.values += 1;
            let value = |stage: &Stage| {
                stage
                    .resolve_property_path_at_time(property, time, InterpolationType::Linear)
                    .map(|r| r.value)
            };
            let want = self.value(self.expected_value(&path, value(source)));
            let got = self.value(value(flattened));
            self.compare(&path, MismatchKind::ValueAt { time }, want, got);
        }
    }

    fn targets(&self, targets: &[TargetPath]) -> String {
        let names: Vec<String> = targets
            .iter()
            .map(|target| target.display(self.paths, self.tokens))
            .collect();
        format!("{names:?}")
    }

    fn resolved(&self, value: Option<ResolvedValue>) -> String {
        let Some(value) = value else {
            return "none".into();
        };
        match value {
            ResolvedValue::Scalar(value) => self.value(Some(value)),
            ResolvedValue::Dictionary(entries) => self.value(Some(Value::Dictionary(entries))),
            ResolvedValue::TokenList(tokens) => {
                let names: Vec<&str> = tokens.iter().map(|&t| self.tokens.resolve(t)).collect();
                format!("{names:?}")
            }
            ResolvedValue::PathList(targets) => self.targets(&targets),
            ResolvedValue::ValueList(values) => {
                let mut out = String::from("[");
                for value in values {
                    out.push_str(&self.value(Some(value)));
                    out.push_str(", ");
                }
                out.push(']');
                out
            }
        }
    }

    /// A value as text, tokens by name.
    fn value(&self, value: Option<Value>) -> String {
        fn write(out: &mut String, value: &Value, tokens: &TokenInterner) {
            match value {
                Value::Token(token) => {
                    let _ = write!(out, "Token({:?})", tokens.resolve(*token));
                }
                Value::Array(items) => {
                    out.push('[');
                    for item in items {
                        write(out, item, tokens);
                        out.push_str(", ");
                    }
                    out.push(']');
                }
                Value::Dictionary(entries) => {
                    out.push('{');
                    for (key, item) in entries {
                        let _ = write!(out, "{key:?}: ");
                        write(out, item, tokens);
                        out.push_str(", ");
                    }
                    out.push('}');
                }
                other => {
                    let _ = write!(out, "{other:?}");
                }
            }
        }
        let Some(value) = value else {
            return "none".into();
        };
        let mut out = String::new();
        write(&mut out, &value, self.tokens);
        out
    }
}

/// The prim metadata fields either stage composes for `prim`, sorted.
fn metadata_keys(source: &Stage, flattened: &Stage, prim: PathId) -> Vec<TokenId> {
    let mut keys: Vec<TokenId> = [source, flattened]
        .iter()
        .filter_map(|stage| stage.prims.get(&prim))
        .flat_map(|index| index.opinions_by_field.keys())
        .filter_map(|key| match key {
            FieldKey::Metadata(key) => Some(*key),
            FieldKey::Property(_) => None,
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// The metadata fields either stage composes for a property, sorted.
fn property_metadata_keys(
    source: &Stage,
    flattened: &Stage,
    prim: PathId,
    name: TokenId,
) -> Vec<TokenId> {
    let mut keys: Vec<TokenId> = [source, flattened]
        .iter()
        .filter_map(|stage| stage.prims.get(&prim)?.property_opinions(name))
        .flatten()
        .filter_map(|opinion| opinion.value.as_property())
        .flat_map(|spec| spec.metadata.iter().map(|entry| entry.name))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// The authored sample times of a property in stage time, over every
/// opinion.
///
/// Spec: AOUSD Core §12.3.2.1 (a layer's time `t` is stage time
/// `t * scale + offset`).
fn sample_times(stage: &Stage, property: PropertyPath) -> Vec<f64> {
    stage
        .explain_property_path(property)
        .unwrap_or_default()
        .iter()
        .flat_map(|opinion| {
            let offset = opinion.layer_offset;
            opinion
                .value
                .time_samples()
                .unwrap_or_default()
                .iter()
                .map(move |(time, _)| super::super::stage_time::to_stage_time(offset, *time))
        })
        .collect()
}
