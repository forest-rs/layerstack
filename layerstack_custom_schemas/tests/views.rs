// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The generated views of a schema library of its own, read and authored
//! on a stage of a USDA layer as a downstream crate would: only through
//! `layerstack_schemas`' public API and the generated modules.

#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::{
    AssetResolveError, AssetResolver, CannotApply, EditTarget, InMemoryStore, LayerId, LiveStage,
    PathInterner, ResolvedAsset, SchemaRegistry, StageOptions, TargetPath, TokenInterner,
};
use layerstack_custom_schemas::lab::{
    CalibrationApi, CalibrationApiCalibrationMethod, Centrifuge, ChannelApi, ChannelApiMode,
    ImagingSpectrometer, ImagingSpectrometerImagingInterleave, Instrument,
    InstrumentInstrumentStatus, Spectrometer,
};
use layerstack_custom_schemas::observatory::{MountedOnApi, Telescope, TelescopeTelescopeMount};
use layerstack_custom_schemas::{lab, observatory};
use layerstack_schemas::usd_geom::{ImageablePurpose, Xformable};
use layerstack_schemas::{ApplyError, Domain, Scene, SchemaEdit, Time, XformOpPrecision};

/// The scene: every lab and observatory schema, authored in USDA.
const SCENE: &str = r#"#usda 1.0
def Spectrometer "Spectro" (
    prepend apiSchemas = ["ChannelAPI:green", "ChannelAPI:blue"]
)
{
    string instrument:serialNumber = "SN-0042"
    uniform token instrument:status = "fault"
    float spectrometer:maxWavelength.timeSamples = {
        1: 700,
        2: 800,
    }
    rel spectrometer:lightSource = </Lamp>
    float channel:green:gain = 2.5
    bool channel:blue = 0
    token channel:blue:mode = "squareRoot"
}

def ImagingSpectrometer "Imager" (
    prepend apiSchemas = ["ChannelAPI:red"]
)
{
    uniform token imaging:interleave = "bsq"
}

def Centrifuge "Spinner"
{
}

def Telescope "Scope"
{
    float telescope:aperture = 1.1
    double3 xformOp:translate = (1, 2, 3)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}

def Xform "Rack" (
    prepend apiSchemas = ["MountedOnAPI"]
)
{
    rel mount:telescope = </Scope>
}

def Scope "Lamp"
{
}
"#;

struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// One registry of OpenUSD's schemas the two libraries build on, and of
/// theirs.
fn registry(tokens: &mut TokenInterner) -> SchemaRegistry {
    let mut builder = SchemaRegistry::builder();
    let domains: Vec<Domain> = lab::DEPENDENCIES
        .iter()
        .chain(observatory::DEPENDENCIES)
        .copied()
        .collect();
    layerstack_schemas::register(&mut builder, &domains, tokens).expect("OpenUSD's schemas");
    lab::register(&mut builder, tokens).expect("the lab schemas");
    observatory::register(&mut builder, tokens).expect("the observatory schemas");
    builder.build(tokens)
}

fn stage() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let schemas = registry(&mut store.tokens);
    assert!(schemas.issues().is_empty(), "{:?}", schemas.issues());
    let read = layerstack_usda::read_usda(
        SCENE,
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    );
    assert!(
        read.emitted.diagnostics.is_empty(),
        "{:?}",
        read.emitted.diagnostics
    );
    store.insert_layer(read.emitted.layer);
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(Arc::new(schemas)),
            ..StageOptions::default()
        },
    );
    (store, live)
}

#[test]
fn each_library_registers_with_its_dependencies() {
    let tokens = TokenInterner::default();
    let labs = lab::registry(&tokens).expect("no metadata conflicts");
    assert!(labs.issues().is_empty(), "{:?}", labs.issues());
    let scopes = observatory::registry(&tokens).expect("no metadata conflicts");
    assert!(scopes.issues().is_empty(), "{:?}", scopes.issues());
    assert_eq!(lab::LIBRARY, "labInstruments");
    assert_eq!(lab::DEPENDENCIES, [Domain::Usd]);
    assert_eq!(observatory::DEPENDENCIES, [Domain::UsdGeom]);
    let [scope_type, xformable] = ["Telescope", "Xformable"].map(|n| tokens.intern(n));
    assert!(scopes.is_a(scope_type, xformable));
}

#[test]
fn views_are_had_for_prims_of_their_schemas() {
    let (store, live) = stage();
    let [spectro, imager, spinner, scope, rack, lamp] = [
        "/Spectro", "/Imager", "/Spinner", "/Scope", "/Rack", "/Lamp",
    ]
    .map(|p| store.path(p));
    let scene = Scene::new(live.stage(), &store);
    // `IsA`, through derived and abstract schemas.
    assert!(Spectrometer::new(&scene, spectro).is_some());
    assert!(Spectrometer::new(&scene, imager).is_some());
    assert!(Instrument::new(&scene, spinner).is_some());
    assert!(ImagingSpectrometer::new(&scene, spectro).is_none());
    assert!(Centrifuge::new(&scene, spectro).is_none());
    assert!(Instrument::new(&scene, lamp).is_none());
    assert!(Telescope::new(&scene, scope).is_some());
    assert!(Xformable::new(&scene, scope).is_some());
    assert!(Telescope::new(&scene, rack).is_none());
    // `HasAPI`: a built-in, an auto-applied schema, an authored one.
    assert!(CalibrationApi::get(&scene, spectro).is_some());
    assert!(CalibrationApi::get(&scene, spinner).is_some());
    assert!(CalibrationApi::get(&scene, lamp).is_none());
    assert!(MountedOnApi::get(&scene, rack).is_some());
    assert!(MountedOnApi::get(&scene, scope).is_none());
}

#[test]
fn getters_read_authored_values_fallbacks_and_samples() {
    let (store, live) = stage();
    let [spectro, imager, spinner, lamp] =
        ["/Spectro", "/Imager", "/Spinner", "/Lamp"].map(|p| store.path(p));
    let scene = Scene::new(live.stage(), &store);
    let spectrometer = Spectrometer::new(&scene, spectro).expect("a spectrometer");
    // Inherited getters, through `Deref` to `Instrument`'s view.
    assert_eq!(
        spectrometer.instrument_serial_number().as_deref(),
        Some("SN-0042")
    );
    assert_eq!(
        spectrometer.instrument_status(),
        Some(InstrumentInstrumentStatus::Fault)
    );
    assert_eq!(spectrometer.instrument_temperature(), Some(21.5));
    // Fallbacks from the registry.
    assert_eq!(spectrometer.spectrometer_channels(), Some(1024));
    assert_eq!(spectrometer.spectrometer_min_wavelength(), Some(380.0));
    // A time-sampled attribute: the fallback at the default time, samples
    // at time codes.
    assert_eq!(spectrometer.spectrometer_max_wavelength(), Some(780.0));
    assert_eq!(
        spectrometer.spectrometer_max_wavelength_at(Time::at(1.5)),
        Some(750.0)
    );
    assert_eq!(
        spectrometer.spectrometer_max_wavelength_at(Time::held(1.5)),
        Some(700.0)
    );
    assert_eq!(
        spectrometer.spectrometer_light_source(),
        [TargetPath::Prim(lamp)]
    );
    // The built-in, whose `calibration:interval` the spectrometer
    // overrides, and an auto-applied one, which keeps the API's.
    let calibration = spectrometer.calibration_api();
    assert_eq!(calibration.calibration_interval(), Some(7.0));
    assert_eq!(
        calibration.calibration_method(),
        Some(CalibrationApiCalibrationMethod::Reference)
    );
    let centrifuge = Centrifuge::new(&scene, spinner).expect("a centrifuge");
    // `centrifuge:maxSpeed`'s accessors are named for its `apiName`.
    assert_eq!(centrifuge.max_rpm(), Some(15000.0));
    assert_eq!(centrifuge.centrifuge_refrigerated(), Some(false));
    assert_eq!(
        CalibrationApi::get(&scene, spinner)
            .expect("auto-applied")
            .calibration_interval(),
        Some(30.0)
    );
    // A derived schema's fallback for the property it redeclares.
    let imaging = ImagingSpectrometer::new(&scene, imager).expect("an imager");
    assert_eq!(
        imaging.instrument_status(),
        Some(InstrumentInstrumentStatus::Running)
    );
    assert_eq!(
        imaging.imaging_interleave(),
        Some(ImagingSpectrometerImagingInterleave::Bsq)
    );
    assert_eq!(imaging.imaging_resolution(), Some([640, 480]));
}

#[test]
fn multiple_apply_instances_read_their_own_properties() {
    let (store, live) = stage();
    let [spectro, imager] = ["/Spectro", "/Imager"].map(|p| store.path(p));
    let scene = Scene::new(live.stage(), &store);
    let names: Vec<&str> = ChannelApi::instances(&scene, spectro)
        .iter()
        .map(|channel| channel.instance())
        .collect();
    assert_eq!(names, ["green", "blue"]);
    let green = ChannelApi::get(&scene, spectro, "green").expect("green is applied");
    assert_eq!(green.gain(), Some(2.5));
    // `channel:__INSTANCE_NAME__`'s accessors are named for its `apiName`.
    assert_eq!(green.enabled(), Some(true));
    assert_eq!(green.mode(), Some(ChannelApiMode::Linear));
    let blue = ChannelApi::get(&scene, spectro, "blue").expect("blue is applied");
    assert_eq!(blue.gain(), Some(1.0));
    assert_eq!(blue.enabled(), Some(false));
    // A token the schema does not allow reads as `Other`.
    assert_eq!(
        blue.mode(),
        Some(ChannelApiMode::Other("squareRoot".into()))
    );
    assert!(ChannelApi::get(&scene, spectro, "red").is_none());
    assert!(ChannelApi::get(&scene, imager, "red").is_some());
}

#[test]
fn inherited_openusd_views_read_through_deref() {
    let (store, live) = stage();
    let [scope_path, rack] = ["/Scope", "/Rack"].map(|p| store.path(p));
    let scene = Scene::new(live.stage(), &store);
    let scope = Telescope::new(&scene, scope_path).expect("a telescope");
    assert_eq!(scope.telescope_aperture(), Some(1.1));
    assert_eq!(
        scope.telescope_mount(),
        Some(TelescopeTelescopeMount::Equatorial)
    );
    // `Xformable`'s and `Imageable`'s getters and computations.
    assert_eq!(
        scope.xform_op_order().as_deref(),
        Some(&["xformOp:translate"][..])
    );
    assert_eq!(
        scope.local_transform(Time::Default).matrix[3],
        [1.0, 2.0, 3.0, 1.0]
    );
    assert_eq!(scope.purpose(), Some(ImageablePurpose::Default));
    // The built-in `VisibilityAPI` from `usdGeom`.
    assert!(scope.visibility_api().guide_visibility().is_some());
    let mount = MountedOnApi::get(&scene, rack).expect("mounted");
    assert_eq!(mount.mount_telescope(), [TargetPath::Prim(scope_path)]);
}

#[test]
fn token_enums_round_trip_their_tokens() {
    for token in ChannelApiMode::TOKENS {
        assert_eq!(ChannelApiMode::from_token(token).as_str(), *token);
    }
    assert_eq!(ChannelApiMode::TOKENS, ["linear", "logarithmic"]);
    assert_eq!(
        ChannelApiMode::from_token("cubic"),
        ChannelApiMode::Other("cubic".into())
    );
    assert_eq!(ChannelApiMode::from_token("cubic").as_str(), "cubic");
}

#[test]
fn setters_author_through_a_schema_edit() {
    let (mut store, mut live) = stage();
    let [lamp, centrifuge, spectro, scope_path, rack, missing] = [
        "/Lamp",
        "/NewCentrifuge",
        "/NewSpectro",
        "/NewScope",
        "/NewRack",
        "/Missing",
    ]
    .map(|p| store.path(p));
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Centrifuge::define(&mut edit, centrifuge)
        .expect("outside any instance")
        .set_max_rpm(&mut edit, 9000.0)
        .set_max_rpm_at(&mut edit, 4.0, 12000.0)
        .set_centrifuge_refrigerated(&mut edit, true)
        .set_instrument_status(&mut edit, InstrumentInstrumentStatus::Running);
    let spectrometer = Spectrometer::define(&mut edit, spectro).expect("outside any instance");
    spectrometer.set_spectrometer_light_source(&mut edit, &[TargetPath::Prim(lamp)]);
    spectrometer
        .calibration_api()
        .set_calibration_method(&mut edit, CalibrationApiCalibrationMethod::SelfTest);
    ChannelApi::apply(&mut edit, spectro, "green")
        .expect("a spectrometer may have a green channel")
        .set_gain(&mut edit, 4.0)
        .set_mode(&mut edit, ChannelApiMode::Other("custom".into()));
    assert_eq!(
        ChannelApi::apply(&mut edit, spectro, "red").map(|_| ()),
        Err(ApplyError::CannotApply(CannotApply::PrimType {
            allowed: vec![edit.tokens().intern("ImagingSpectrometer")]
        })),
        "only an imaging spectrometer may have a red channel"
    );
    assert!(ChannelApi::apply(&mut edit, missing, "green").is_err());
    let scope = Telescope::define(&mut edit, scope_path).expect("outside any instance");
    scope
        .set_telescope_aperture(&mut edit, 0.75)
        .set_telescope_mount(&mut edit, TelescopeTelescopeMount::Fork);
    // `XformableEdit`'s op authoring, through `Deref`.
    scope
        .add_translate_op(&mut edit, XformOpPrecision::Double)
        .expect("a new op")
        .set(&mut edit, [0.0, 0.5, 0.0])
        .expect("a translation");
    assert_eq!(
        MountedOnApi::apply(&mut edit, rack).map(|_| ()),
        Err(ApplyError::CannotApply(CannotApply::NoSuchPrim)),
        "no edit authors an `over` for a prim that does not exist"
    );
    edit.define_prim(rack, "Xform")
        .expect("outside any instance");
    MountedOnApi::apply(&mut edit, rack)
        .expect("an Xform may be mounted")
        .set_mount_telescope(&mut edit, &[TargetPath::Prim(scope_path)]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).expect("applies");

    let scene = Scene::new(live.stage(), &store);
    let centrifuge = Centrifuge::new(&scene, centrifuge).expect("defined");
    assert_eq!(centrifuge.max_rpm(), Some(9000.0));
    assert_eq!(centrifuge.max_rpm_at(Time::held(4.0)), Some(12000.0));
    assert_eq!(centrifuge.centrifuge_refrigerated(), Some(true));
    assert_eq!(
        centrifuge.instrument_status(),
        Some(InstrumentInstrumentStatus::Running)
    );
    let spectrometer = Spectrometer::new(&scene, spectro).expect("defined");
    assert_eq!(
        spectrometer.spectrometer_light_source(),
        [TargetPath::Prim(lamp)]
    );
    assert_eq!(
        spectrometer.calibration_api().calibration_method(),
        Some(CalibrationApiCalibrationMethod::SelfTest)
    );
    let green = ChannelApi::get(&scene, spectro, "green").expect("applied");
    assert_eq!(green.gain(), Some(4.0));
    assert_eq!(green.mode(), Some(ChannelApiMode::Other("custom".into())));
    let scope = Telescope::new(&scene, scope_path).expect("defined");
    assert_eq!(scope.telescope_aperture(), Some(0.75));
    assert_eq!(scope.telescope_mount(), Some(TelescopeTelescopeMount::Fork));
    assert_eq!(
        scope.local_transform(Time::Default).matrix[3],
        [0.0, 0.5, 0.0, 1.0]
    );
    let mount = MountedOnApi::get(&scene, rack).expect("applied");
    assert_eq!(mount.mount_telescope(), [TargetPath::Prim(scope_path)]);
}

/// The kinds the lab plugin's `plugInfo.json` declares, registered from the
/// generated module, drive the model hierarchy as OpenUSD's `KindRegistry`
/// has them drive it once the plugin is registered.
#[test]
fn library_kinds_drive_model_queries() {
    use layerstack_schemas::kind::{KindError, KindRegistry};

    let mut kinds = KindRegistry::new();
    assert!(lab::register_kinds(&mut kinds).is_empty());
    assert!(kinds.is_assembly("lab_station") && kinds.is_component("lab_bench"));
    assert!(kinds.issues().is_empty(), "{:?}", kinds.issues());
    assert!(observatory::KINDS.is_empty());
    // Registered twice, as two plugins declaring them would be: refused.
    let again = lab::register_kinds(&mut kinds);
    assert_eq!(again.len(), lab::KINDS.len());
    assert!(again.contains(&KindError::AlreadyRegistered("lab_bench".into())));

    let mut store = InMemoryStore::default();
    let read = layerstack_usda::read_usda(
        r#"#usda 1.0
def Xform "Site" (kind = "lab_station")
{
    def Xform "Bench" (kind = "lab_bench")
    {
        def Xform "Probe" (kind = "lab_bench")
        {
        }
    }
}
"#,
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    );
    store.insert_layer(read.emitted.layer);
    let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let scene = Scene::new(live.stage(), &store).with_kinds(&kinds);
    let [site, bench, probe] = ["/Site", "/Site/Bench", "/Site/Bench/Probe"].map(|p| store.path(p));
    assert!(scene.is_group(site) && scene.is_model(site));
    assert!(scene.is_model(bench) && !scene.is_group(bench));
    // A component's children are outside the model hierarchy.
    assert!(!scene.is_model(probe));
    // Without the plugin's kinds, they are no kinds.
    let plain = Scene::new(live.stage(), &store);
    assert!(!plain.is_model(site) && !plain.is_model(bench));
}
