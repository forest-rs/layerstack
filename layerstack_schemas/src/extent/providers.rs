// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit host extent evaluation. Geometry and external revisions remain host-owned.

use crate::{PrimView, Scene, Time, bounds::Range3d};
use alloc::{sync::Arc, vec::Vec};
use layerstack::{
    ArrayReadError, HashMap, PathId, PropertyPath, SchemaRegistry, TokenId, TokenInterner, Value,
};

/// A provider cannot produce a complete local extent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtentError {
    /// Retained numeric input failed to decode.
    Decode {
        /// Input property in stage namespace.
        property: PropertyPath,
        /// Adapter failure, including any decode budget limit.
        error: ArrayReadError,
    },
    /// Host evaluation failed; no partial range is accepted.
    Failed {
        /// Boundable being evaluated.
        prim: PathId,
        /// Host-supplied diagnostic and recovery context.
        message: Arc<str>,
    },
}
impl core::fmt::Display for ExtentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Decode { property, error } => {
                write!(f, "extent input {property:?} failed: {error}")
            }
            Self::Failed { prim, message } => {
                write!(f, "extent evaluation for {prim:?} failed: {message}")
            }
        }
    }
}
impl core::error::Error for ExtentError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Decode { error, .. } => Some(error),
            Self::Failed { .. } => None,
        }
    }
}

/// Invalid registration, leaving the registry unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtentRegistrationError {
    /// The schema is unregistered or does not derive from `Boundable`.
    NotBoundable(TokenId),
    /// A provider is already registered for this schema.
    AlreadyRegistered(TokenId),
}
impl core::fmt::Display for ExtentRegistrationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotBoundable(schema) => write!(
                f,
                "extent provider schema {schema:?} is not registered as Boundable"
            ),
            Self::AlreadyRegistered(schema) => {
                write!(f, "extent provider already registered for {schema:?}")
            }
        }
    }
}
impl core::error::Error for ExtentRegistrationError {}

/// Host-owned local geometry computation, with explicit input evidence.
///
/// Return the complete untransformed local range. `BoundsCache` owns transforms,
/// purpose and traversal; the host owns geometry, scheduling and external data.
/// Use [`ExtentContext::read_value`] for automatically tracked composed inputs,
/// and [`ExtentContext::track_prim`] for any other stage reads. Mark time-dependent
/// host geometry with [`ExtentContext::mark_time_varying`]. After external edits,
/// update [`crate::bounds::BoundsCache::set_extent_revision`] before querying again.
pub trait ExtentProvider: core::fmt::Debug + Send + Sync {
    /// Compute a complete local range or explain why evaluation failed.
    fn compute_extent(&self, context: &mut ExtentContext<'_>) -> Result<Range3d, ExtentError>;
}

/// Caller-owned provider dispatch; no discovery, plugin loading or global state.
///
/// Dispatch selects the nearest registered provider in typed-schema inheritance,
/// including built-in providers. A custom provider on a base schema cannot
/// displace a nearer built-in provider. A valid authored `extent` always wins.
/// AOUSD Core §13.3.1 (single inheritance); OpenUSD
/// `boundableComputeExtent.cpp::_FunctionRegistry::GetComputeFunction`.
#[derive(Clone, Debug, Default)]
pub struct ExtentProviders {
    providers: HashMap<TokenId, Arc<dyn ExtentProvider>>,
}
impl ExtentProviders {
    /// Register a provider for a typed schema derived from `Boundable`.
    /// Tokens must belong to the same interner as `schemas` and the scene.
    pub fn register(
        &mut self,
        schemas: &SchemaRegistry,
        tokens: &TokenInterner,
        schema: TokenId,
        provider: Arc<dyn ExtentProvider>,
    ) -> Result<(), ExtentRegistrationError> {
        let boundable = tokens.lookup("Boundable");
        if boundable.is_none_or(|boundable| !schemas.is_a(schema, boundable)) {
            return Err(ExtentRegistrationError::NotBoundable(schema));
        }
        if self.providers.contains_key(&schema) {
            return Err(ExtentRegistrationError::AlreadyRegistered(schema));
        }
        self.providers.insert(schema, provider);
        Ok(())
    }

    /// Whether a provider is explicitly registered for this exact schema.
    #[must_use]
    pub fn contains(&self, schema: TokenId) -> bool {
        self.providers.contains_key(&schema)
    }

    pub(crate) fn select(
        &self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Option<(TokenId, Arc<dyn ExtentProvider>)> {
        let schemas = scene.stage().schemas()?;
        let mut schema = scene.stage().resolve_type_name(path, scene.store())?;
        let mut visited = Vec::new();
        loop {
            if visited.contains(&schema) {
                return None;
            }
            visited.push(schema);
            if let Some(provider) = self.providers.get(&schema) {
                return Some((schema, provider.clone()));
            }
            if super::has_builtin(scene.store().tokens().resolve(schema)) {
                return None;
            }
            schema = schemas.schema(schema)?.parent?;
        }
    }
}

/// Evidence retained for the last host evaluation, including failed evaluations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtentDependencies {
    /// Schema whose registered provider was selected.
    pub schema: TokenId,
    /// Stage prims read, including missing prims and the evaluated prim itself.
    pub prims: Vec<PathId>,
    /// Composed properties read through the context.
    pub properties: Vec<PropertyPath>,
    /// Whether changing query time requires reevaluation.
    pub time_varying: bool,
    /// Host revision supplied to the cache for this evaluation.
    pub external_revision: u64,
}

/// Inputs and dependency tracking for one provider invocation.
#[derive(Debug)]
pub struct ExtentContext<'a> {
    scene: Scene<'a>,
    prim: PathId,
    time: Time,
    pub(crate) dependencies: ExtentDependencies,
    pub(crate) input_error: Option<ExtentError>,
}
impl<'a> ExtentContext<'a> {
    pub(crate) fn new(
        scene: Scene<'a>,
        prim: PathId,
        time: Time,
        schema: TokenId,
        revision: u64,
    ) -> Self {
        Self {
            scene,
            prim,
            time,
            input_error: None,
            dependencies: ExtentDependencies {
                schema,
                prims: alloc::vec![prim],
                properties: Vec::new(),
                time_varying: false,
                external_revision: revision,
            },
        }
    }
    /// Boundable whose local extent is requested.
    #[must_use]
    pub fn prim(&self) -> PathId {
        self.prim
    }
    /// Requested default or numeric stage time.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }
    /// Opaque host revision associated with this evaluation.
    #[must_use]
    pub fn external_revision(&self) -> u64 {
        self.dependencies.external_revision
    }
    /// Scene for additional reads; record each consulted prim with `track_prim`.
    #[must_use]
    pub fn scene(&self) -> &Scene<'a> {
        &self.scene
    }
    /// Record any consulted prim, including an absent source needed for recovery.
    pub fn track_prim(&mut self, prim: PathId) {
        if !self.dependencies.prims.contains(&prim) {
            self.dependencies.prims.push(prim);
        }
    }
    /// Declare temporal dependence of host inputs not read through `read_value`.
    pub fn mark_time_varying(&mut self) {
        self.dependencies.time_varying = true;
    }
    /// Read a composed input with schema fallback, typed selection and checked
    /// retained decoding. Missing or incompatible inputs return `Ok(None)`.
    /// Dependencies are recorded before decoding, so failures remain recoverable.
    /// A decode failure also rejects the final extent if the provider ignores it.
    /// AOUSD Core §12.3–12.5 (attribute values and interpolation).
    pub fn read_value<T>(
        &mut self,
        prim: PathId,
        name: &str,
        decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Result<Option<T>, ExtentError> {
        self.track_prim(prim);
        let view = PrimView::new(self.scene, prim);
        self.dependencies.time_varying |= view.property_might_vary(name);
        if let Some(token) = self.scene.store().tokens().lookup(name) {
            let property = PropertyPath::new(prim, token);
            if !self.dependencies.properties.contains(&property) {
                self.dependencies.properties.push(property);
            }
            let result = view
                .try_read_value(name, self.time, decode)
                .map_err(|error| ExtentError::Decode { property, error });
            if let Err(error) = &result {
                self.input_error.get_or_insert_with(|| error.clone());
            }
            result
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounds::{BoundsCache, BoundsError, BoundsOptions};
    use alloc::vec;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use layerstack::{
        EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec, PropertyType,
        SchemaDefinition, StageOptions, Transaction,
    };

    #[derive(Debug)]
    struct InputProvider {
        source: PathId,
        calls: Arc<AtomicUsize>,
        external: Arc<AtomicUsize>,
        varying: bool,
    }
    impl ExtentProvider for InputProvider {
        fn compute_extent(&self, context: &mut ExtentContext<'_>) -> Result<Range3d, ExtentError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.varying {
                context.mark_time_varying();
            }
            let size = context
                .read_value(self.source, "hostSize", crate::value::read_double)?
                .ok_or_else(|| ExtentError::Failed {
                    prim: context.prim(),
                    message: "missing hostSize".into(),
                })?;
            let time = if self.varying {
                match context.time() {
                    Time::Default => 0.,
                    Time::At { code, .. } => code,
                }
            } else {
                0.
            };
            let radius = size
                + time
                + f64::from(u32::try_from(self.external.load(Ordering::Relaxed)).unwrap());
            Ok(Range3d {
                min: [-radius; 3],
                max: [radius; 3],
            })
        }
    }
    struct Fixture {
        store: InMemoryStore,
        live: LiveStage,
        schemas: Arc<SchemaRegistry>,
        shape: PathId,
        source: PathId,
        base: TokenId,
        calls: Arc<AtomicUsize>,
        external: Arc<AtomicUsize>,
    }
    impl Fixture {
        fn new(source_exists: bool) -> Self {
            let mut store = InMemoryStore::default();
            let shape = store.path("/Root/Shape");
            let root = store.path("/Root");
            let source = store.path("/Inputs");
            let base = store.tokens.intern("HostBoundable");
            let derived = store.tokens.intern("DerivedHostBoundable");
            let size = store.tokens.intern("hostSize");
            let mut builder = SchemaRegistry::builder();
            crate::register(&mut builder, &[crate::Domain::UsdGeom], &mut store.tokens).unwrap();
            let boundable = store.tokens.lookup("Boundable").unwrap();
            builder.register(SchemaDefinition::typed(base).with_parent(boundable));
            builder.register(SchemaDefinition::typed(derived).with_parent(base));
            let schemas = Arc::new(builder.build(&mut store.tokens));
            let mut layer = Layer::new(LayerId(1));
            layer.insert_prim(root, PrimSpec::def());
            layer.insert_prim(shape, PrimSpec::def().with_type_name(derived));
            if source_exists {
                layer.insert_prim(
                    source,
                    PrimSpec::def().with_property(
                        size,
                        PropertySpec::typed_attribute(PropertyType::new(
                            "double",
                            false,
                            Value::Double(0.),
                        ))
                        .with_default(Value::Double(2.)),
                    ),
                );
            }
            store.insert_layer(layer);
            let live = LiveStage::compose(
                &mut store,
                LayerId(1),
                StageOptions {
                    schemas: Some(schemas.clone()),
                    ..StageOptions::default()
                },
            );
            Self {
                store,
                live,
                schemas,
                shape,
                source,
                base,
                calls: Arc::new(AtomicUsize::new(0)),
                external: Arc::new(AtomicUsize::new(0)),
            }
        }
        fn cache(&self, varying: bool) -> BoundsCache {
            self.cache_options(varying, BoundsOptions::default())
        }
        fn cache_options(&self, varying: bool, options: BoundsOptions) -> BoundsCache {
            let mut providers = ExtentProviders::default();
            providers
                .register(
                    &self.schemas,
                    &self.store.tokens,
                    self.base,
                    Arc::new(InputProvider {
                        source: self.source,
                        calls: self.calls.clone(),
                        external: self.external.clone(),
                        varying,
                    }),
                )
                .unwrap();
            BoundsCache::with_extent_providers(Time::Default, options, providers)
        }
        fn query(&self, cache: &mut BoundsCache, path: PathId) -> Result<Range3d, BoundsError> {
            cache
                .world_bound(&Scene::new(self.live.stage(), &self.store), path)
                .map(|bbox| bbox.aligned_range())
        }
        fn edit(&mut self, cache: &mut BoundsCache, transaction: &Transaction) {
            let report = self.live.apply(&mut self.store, transaction).unwrap();
            cache.apply_changes(&Scene::new(self.live.stage(), &self.store), &report.changes);
        }
    }

    #[test]
    fn inherited_dispatch_cross_prim_changes_and_external_revision() {
        let mut f = Fixture::new(true);
        let root = f.store.path("/Root");
        let size = f.store.property_path("/Inputs.hostSize");
        let mut cache = f.cache(false);
        assert_eq!(f.query(&mut cache, root).unwrap().max, [2.; 3]);
        assert_eq!(f.query(&mut cache, root).unwrap().max, [2.; 3]);
        assert_eq!(f.calls.load(Ordering::Relaxed), 1);
        let evidence = cache.extent_dependencies(f.shape).unwrap();
        assert_eq!(evidence.schema, f.base);
        assert_eq!(evidence.properties, vec![size]);
        assert!(evidence.prims.contains(&f.source));
        let mut resize = Transaction::new();
        resize.set_default(
            EditTarget::for_layer(LayerId(1)).property(size),
            Value::Double(5.),
        );
        f.edit(&mut cache, &resize);
        assert_eq!(f.query(&mut cache, root).unwrap().max, [5.; 3]);
        f.external.store(3, Ordering::Relaxed);
        assert_eq!(f.query(&mut cache, root).unwrap().max, [5.; 3]);
        cache.set_extent_revision(7);
        assert_eq!(f.query(&mut cache, root).unwrap().max, [8.; 3]);
        assert_eq!(
            cache
                .extent_dependencies(f.shape)
                .unwrap()
                .external_revision,
            7
        );
        cache.set_extent_revision(0);
        assert_eq!(f.query(&mut cache, root).unwrap().max, [8.; 3]);
        cache.clear();
        f.query(&mut cache, root).unwrap();
        assert_eq!(f.calls.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn host_time_dependence_propagates_to_cached_ancestors() {
        let mut f = Fixture::new(true);
        let root = f.store.path("/Root");
        let mut cache = f.cache(true);
        assert_eq!(f.query(&mut cache, root).unwrap().max, [2.; 3]);
        cache.set_time(Time::At {
            code: 4.,
            interpolation: layerstack::InterpolationType::Held,
        });
        assert_eq!(f.query(&mut cache, root).unwrap().max, [6.; 3]);
        assert!(cache.extent_dependencies(f.shape).unwrap().time_varying);
        assert_eq!(f.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn failed_inputs_are_retained_and_recover_after_creation() {
        let mut f = Fixture::new(false);
        let mut cache = f.cache(false);
        assert!(matches!(
            f.query(&mut cache, f.shape),
            Err(BoundsError::ExtentProvider {
                source: ExtentError::Failed { .. },
                ..
            })
        ));
        assert!(
            cache
                .extent_dependencies(f.shape)
                .unwrap()
                .prims
                .contains(&f.source)
        );
        let size = f.store.property_path("/Inputs.hostSize");
        let target = EditTarget::for_layer(LayerId(1));
        let mut create = Transaction::new();
        create.create_prim(target.prim(f.source), layerstack::Specifier::Def, None);
        create.create_property(
            target.property(size),
            PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.)))
                .with_default(Value::Double(3.)),
        );
        f.edit(&mut cache, &create);
        assert_eq!(f.query(&mut cache, f.shape).unwrap().max, [3.; 3]);
    }

    #[test]
    fn authored_extent_wins_and_missing_provider_is_an_error() {
        let mut f = Fixture::new(true);
        let scene = Scene::new(f.live.stage(), &f.store);
        let mut empty = BoundsCache::new(Time::Default, BoundsOptions::default());
        assert_eq!(
            empty.world_bound(&scene, f.shape),
            Err(BoundsError::ExtentUnavailable(f.shape))
        );
        let mut cache = f.cache(false);
        let extent = f.store.property_path("/Root/Shape.extent");
        let mut authored = Transaction::new();
        authored.create_property(
            EditTarget::for_layer(LayerId(1)).property(extent),
            PropertySpec::attribute().with_default(Value::from(vec![[-9_f32; 3], [9.; 3]])),
        );
        f.edit(&mut cache, &authored);
        assert_eq!(f.query(&mut cache, f.shape).unwrap().max, [9.; 3]);
        assert_eq!(f.calls.load(Ordering::Relaxed), 0);
        assert!(cache.extent_dependencies(f.shape).is_none());
    }

    #[test]
    fn sampled_input_automatically_marks_time_dependence() {
        let mut f = Fixture::new(true);
        let size = f.store.property_path("/Inputs.hostSize");
        let target = EditTarget::for_layer(LayerId(1));
        let mut cache = f.cache(false);
        let mut sampled = Transaction::new();
        sampled.set_time_sample(target.property(size), 1., Value::Double(3.));
        sampled.set_time_sample(target.property(size), 2., Value::Double(6.));
        f.edit(&mut cache, &sampled);
        cache.set_time(Time::At {
            code: 1.,
            interpolation: layerstack::InterpolationType::Held,
        });
        assert_eq!(f.query(&mut cache, f.shape).unwrap().max, [3.; 3]);
        assert!(cache.extent_dependencies(f.shape).unwrap().time_varying);
        cache.set_time(Time::At {
            code: 2.,
            interpolation: layerstack::InterpolationType::Held,
        });
        assert_eq!(f.query(&mut cache, f.shape).unwrap().max, [6.; 3]);
    }

    #[test]
    fn registration_validation_and_nearer_builtin_dispatch() {
        let mut f = Fixture::new(true);
        let boundable = f.store.tokens.lookup("Boundable").unwrap();
        let cube_type = f.store.tokens.lookup("Cube").unwrap();
        let xform = f.store.tokens.lookup("Xform").unwrap();
        let unknown = f.store.tokens.intern("Unregistered");
        let provider = Arc::new(InputProvider {
            source: f.source,
            calls: f.calls.clone(),
            external: f.external.clone(),
            varying: false,
        });
        let mut providers = ExtentProviders::default();
        assert_eq!(
            providers.register(&f.schemas, &f.store.tokens, unknown, provider.clone()),
            Err(ExtentRegistrationError::NotBoundable(unknown))
        );
        assert_eq!(
            providers.register(&f.schemas, &f.store.tokens, xform, provider.clone()),
            Err(ExtentRegistrationError::NotBoundable(xform))
        );
        providers
            .register(&f.schemas, &f.store.tokens, boundable, provider.clone())
            .unwrap();
        assert!(providers.contains(boundable));
        assert_eq!(
            providers.register(&f.schemas, &f.store.tokens, boundable, provider),
            Err(ExtentRegistrationError::AlreadyRegistered(boundable))
        );
        let cube = f.store.path("/Cube");
        let mut cache =
            BoundsCache::with_extent_providers(Time::Default, BoundsOptions::default(), providers);
        let mut create = Transaction::new();
        create.create_prim(
            EditTarget::for_layer(LayerId(1)).prim(cube),
            layerstack::Specifier::Def,
            Some(cube_type),
        );
        f.edit(&mut cache, &create);
        assert_eq!(f.query(&mut cache, cube).unwrap().max, [1.; 3]);
        assert_eq!(f.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn provider_inputs_invalidate_instancers_with_separate_prototype_cache() {
        for ignore_visibility in [false, true] {
            let mut f = Fixture::new(true);
            let instancer = f.store.path("/Instances");
            let target = EditTarget::for_layer(LayerId(1));
            let mut edit = crate::SchemaEdit::new(f.live.stage(), &mut f.store, target.clone());
            let instances = crate::usd_geom::PointInstancer::define(&mut edit, instancer);
            instances.set_prototypes(&mut edit, &[layerstack::TargetPath::Prim(f.shape)]);
            instances.set_proto_indices(&mut edit, &[0]);
            instances.set_positions(&mut edit, &[[0.; 3]]);
            let transaction = edit.finish();
            f.live.apply(&mut f.store, &transaction).unwrap();
            let mut cache = f.cache_options(
                false,
                BoundsOptions {
                    ignore_visibility,
                    ..BoundsOptions::default()
                },
            );
            assert_eq!(f.query(&mut cache, instancer).unwrap().max, [2.; 3]);
            assert!(cache.extent_dependencies(f.shape).is_some());
            let size = f.store.property_path("/Inputs.hostSize");
            let mut resize = Transaction::new();
            resize.set_default(target.property(size), Value::Double(8.));
            f.edit(&mut cache, &resize);
            assert_eq!(f.query(&mut cache, instancer).unwrap().max, [8.; 3]);
            assert_eq!(f.calls.load(Ordering::Relaxed), 2);
        }
    }

    #[derive(Debug)]
    struct CorruptArray(ArrayReadError);
    impl layerstack::DeferredArraySource for CorruptArray {
        fn materialize(&self) -> Result<&layerstack::TypedArray, &ArrayReadError> {
            Err(&self.0)
        }
        fn element_kind(&self) -> Value {
            Value::Vec3f([0.; 3])
        }
    }
    #[derive(Debug)]
    struct PointsProvider(PathId);
    impl ExtentProvider for PointsProvider {
        fn compute_extent(&self, context: &mut ExtentContext<'_>) -> Result<Range3d, ExtentError> {
            let points = context
                .read_value(self.0, "hostPoints", crate::value::read_float3_array_shared)?
                .ok_or_else(|| ExtentError::Failed {
                    prim: context.prim(),
                    message: "missing hostPoints".into(),
                })?;
            let max = points.first().copied().unwrap_or([0.; 3]).map(f64::from);
            Ok(Range3d {
                min: max.map(|v| -v),
                max,
            })
        }
    }
    #[derive(Debug)]
    struct IgnoredDecode(PathId);
    impl ExtentProvider for IgnoredDecode {
        fn compute_extent(&self, context: &mut ExtentContext<'_>) -> Result<Range3d, ExtentError> {
            let _ =
                context.read_value(self.0, "hostPoints", crate::value::read_float3_array_shared);
            Ok(Range3d {
                min: [-1.; 3],
                max: [1.; 3],
            })
        }
    }
    #[test]
    fn provider_decode_failure_is_precise_and_recovers_after_edit() {
        let mut f = Fixture::new(true);
        let points = f.store.property_path("/Inputs.hostPoints");
        let error = ArrayReadError::BudgetExceeded { limit: 17 };
        let mut providers = ExtentProviders::default();
        providers
            .register(
                &f.schemas,
                &f.store.tokens,
                f.base,
                Arc::new(PointsProvider(f.source)),
            )
            .unwrap();
        let mut cache = BoundsCache::with_extent_providers(
            Time::Default,
            BoundsOptions::default(),
            providers.clone(),
        );
        let target = EditTarget::for_layer(LayerId(1));
        let mut corrupt = Transaction::new();
        corrupt.create_property(
            target.property(points),
            PropertySpec::typed_attribute(PropertyType::new("float3", true, Value::Vec3f([0.; 3])))
                .with_default(Value::TypedArray(layerstack::TypedArray::Deferred(
                    Arc::new(CorruptArray(error.clone())),
                ))),
        );
        f.edit(&mut cache, &corrupt);
        let expected = BoundsError::ExtentProvider {
            prim: f.shape,
            source: ExtentError::Decode {
                property: points,
                error,
            },
        };
        assert_eq!(f.query(&mut cache, f.shape), Err(expected.clone()));
        assert_eq!(
            cache.extent_dependencies(f.shape).unwrap().properties,
            vec![points]
        );
        let mut ignoring = ExtentProviders::default();
        ignoring
            .register(
                &f.schemas,
                &f.store.tokens,
                f.base,
                Arc::new(IgnoredDecode(f.source)),
            )
            .unwrap();
        cache.set_extent_providers(ignoring);
        assert_eq!(f.query(&mut cache, f.shape), Err(expected));
        cache.set_extent_providers(providers);
        let mut repair = Transaction::new();
        repair.set_default(target.property(points), Value::from(vec![[7_f32; 3]]));
        f.edit(&mut cache, &repair);
        assert_eq!(f.query(&mut cache, f.shape).unwrap().max, [7.; 3]);
    }

    #[test]
    fn retained_authored_extent_and_intrinsic_points_fail_without_partial_bounds() {
        let mut f = Fixture::new(true);
        let mesh_type = f.store.tokens.lookup("Mesh").unwrap();
        let mesh = f.store.path("/Mesh");
        let extent = f.store.property_path("/Mesh.extent");
        let points = f.store.property_path("/Mesh.points");
        let target = EditTarget::for_layer(LayerId(1));
        let error = ArrayReadError::InvalidData("truncated float3 storage".into());
        let corrupt = Value::TypedArray(layerstack::TypedArray::Deferred(Arc::new(CorruptArray(
            error.clone(),
        ))));
        let mut cache = f.cache(false);
        let mut create = Transaction::new();
        create.create_prim(
            target.prim(mesh),
            layerstack::Specifier::Def,
            Some(mesh_type),
        );
        create.create_property(
            target.property(points),
            PropertySpec::attribute().with_default(corrupt.clone()),
        );
        f.edit(&mut cache, &create);
        assert_eq!(
            f.query(&mut cache, mesh),
            Err(BoundsError::ExtentProvider {
                prim: mesh,
                source: ExtentError::Decode {
                    property: points,
                    error: error.clone()
                }
            })
        );
        let mut author = Transaction::new();
        author.create_property(
            target.property(extent),
            PropertySpec::attribute().with_default(corrupt),
        );
        f.edit(&mut cache, &author);
        assert_eq!(
            f.query(&mut cache, mesh),
            Err(BoundsError::ExtentProvider {
                prim: mesh,
                source: ExtentError::Decode {
                    property: extent,
                    error
                }
            })
        );
        let mut repair = Transaction::new();
        repair.set_default(
            target.property(extent),
            Value::from(vec![[-2_f32; 3], [2.; 3]]),
        );
        f.edit(&mut cache, &repair);
        assert_eq!(f.query(&mut cache, mesh).unwrap().max, [2.; 3]);
    }
}
