// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resolve authored render configuration into products and channels.
//!
//! These computations read default values, not a rendering time. They do
//! not render images or execute shaders. Path and token handles belong to
//! the scene's store; retained specs must be recomputed after source edits.

use crate::view::forwarded_targets;
use crate::{
    PrimView, Scene, Time,
    usd_render::{
        RenderProduct, RenderProductProductType, RenderSettings, RenderSettingsBase,
        RenderSettingsBaseAspectRatioConformPolicy, RenderVar, RenderVarSourceType,
    },
};
use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use layerstack::{PathId, PropertyKind, PropertyPath, TargetPath, Value};

/// Extra render configuration: an authored value or ordered target paths.
#[derive(Clone, Debug, PartialEq)]
pub enum RenderSettingValue {
    /// An authored attribute's default value.
    Value(Value),
    /// Authored relationship targets or prims providing a connected output.
    Paths(Vec<TargetPath>),
}

/// Extra authored configuration, keyed by the complete property name.
pub type NamespacedSettings = BTreeMap<Arc<str>, RenderSettingValue>;

/// A resolved render output.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedRenderProduct {
    /// Product prim in the source scene.
    pub path: PathId,
    /// Product type, usually `raster`.
    pub product_type: RenderProductProductType,
    /// Output artifact name.
    pub name: Arc<str>,
    /// Camera prim selected by the product or shared settings.
    pub camera: PathId,
    /// Disable motion blur.
    pub disable_motion_blur: bool,
    /// Disable depth of field.
    pub disable_depth_of_field: bool,
    /// Pixel width and height.
    pub resolution: [i32; 2],
    /// Pixel aspect ratio after conforming to the camera.
    pub pixel_aspect_ratio: f32,
    /// Applied aspect-ratio policy.
    pub aspect_ratio_conform_policy: RenderSettingsBaseAspectRatioConformPolicy,
    /// Camera aperture width and height after conforming.
    pub aperture_size: [f32; 2],
    /// Normalized data window, `[xmin, ymin, xmax, ymax]`.
    pub data_window_ndc: [f32; 4],
    /// Ordered indices into [`RenderSpec::render_vars`].
    pub render_var_indices: Vec<usize>,
    /// Extra product-specific settings; shared settings remain on the spec.
    pub namespaced_settings: NamespacedSettings,
}

/// A resolved channel shared by products referencing the same prim.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedRenderVar {
    /// Channel prim in the source scene.
    pub path: PathId,
    /// USD output value-type name.
    pub data_type: Arc<str>,
    /// Data source name.
    pub source_name: Arc<str>,
    /// How the source name is interpreted.
    pub source_type: RenderVarSourceType,
    /// Extra channel settings.
    pub namespaced_settings: NamespacedSettings,
}

/// Recoverable invalid authored render configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderProblem {
    /// A product camera is absent or does not identify a Camera prim.
    /// That product is omitted, matching OpenUSD's computation.
    InvalidCamera {
        /// Product that cannot be resolved.
        product: PathId,
        /// First composed camera target, if present.
        target: Option<TargetPath>,
    },
    /// An ordered channel target does not identify a `RenderVar` prim.
    InvalidRenderVar {
        /// Product referencing the channel.
        product: PathId,
        /// Invalid composed target.
        target: TargetPath,
    },
}

/// Resolved render outputs and scene filtering settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderSpec {
    /// Valid products, in composed relationship order.
    pub products: Vec<ComputedRenderProduct>,
    /// Unique channels, in first-use order across products.
    pub render_vars: Vec<ComputedRenderVar>,
    /// Included Imageable purposes.
    pub included_purposes: Vec<Arc<str>>,
    /// Material binding purpose preference order.
    pub material_binding_purposes: Vec<Arc<str>>,
    /// Shared extra render settings.
    pub namespaced_settings: NamespacedSettings,
    /// Invalid configuration skipped during resolution.
    pub problems: Vec<RenderProblem>,
}

#[derive(Clone)]
struct Base {
    camera: Option<TargetPath>,
    resolution: [i32; 2],
    pixel_aspect_ratio: f32,
    policy: RenderSettingsBaseAspectRatioConformPolicy,
    data_window: [f32; 4],
    disable_motion_blur: bool,
    disable_depth_of_field: bool,
}

impl Base {
    fn read(base: &RenderSettingsBase<'_>) -> Self {
        let mut out = Self {
            camera: None,
            resolution: [0; 2],
            pixel_aspect_ratio: 1.0,
            policy: RenderSettingsBaseAspectRatioConformPolicy::ExpandAperture,
            data_window: [0.0, 0.0, 1.0, 1.0],
            disable_motion_blur: false,
            disable_depth_of_field: false,
        };
        out.apply(base, true);
        out
    }

    fn apply(&mut self, base: &RenderSettingsBase<'_>, defaults: bool) {
        if let Some(target) = base
            .property_path(RenderSettingsBase::CAMERA)
            .and_then(|p| forwarded_targets(&base.scene(), p).first().copied())
        {
            self.camera = Some(target);
        }
        macro_rules! set {
            ($field:ident, $name:expr, $getter:ident) => {
                if (defaults || base.has_authored_value($name))
                    && let Some(value) = base.$getter()
                {
                    self.$field = value;
                }
            };
        }
        set!(resolution, RenderSettingsBase::RESOLUTION, resolution);
        set!(
            pixel_aspect_ratio,
            RenderSettingsBase::PIXEL_ASPECT_RATIO,
            pixel_aspect_ratio
        );
        set!(
            policy,
            RenderSettingsBase::ASPECT_RATIO_CONFORM_POLICY,
            aspect_ratio_conform_policy
        );
        set!(
            data_window,
            RenderSettingsBase::DATA_WINDOW_NDC,
            data_window_ndc
        );
        set!(
            disable_motion_blur,
            RenderSettingsBase::DISABLE_MOTION_BLUR,
            disable_motion_blur
        );
        set!(
            disable_depth_of_field,
            RenderSettingsBase::DISABLE_DEPTH_OF_FIELD,
            disable_depth_of_field
        );
        // OpenUSD26.8 _ReadSettingsBase checks disableMotionBlur twice;
        // instantaneousShutter is not read by its computed spec.
    }
}

fn conform(product: &mut ComputedRenderProduct) {
    use RenderSettingsBaseAspectRatioConformPolicy as Policy;
    let [width, height] = product.resolution;
    let [ap_width, ap_height] = product.aperture_size;
    if width <= 0 || height <= 0 || ap_width <= 0.0 || ap_height <= 0.0 {
        return;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "OpenUSD computes aspect ratios in float precision"
    )]
    let resolution_aspect = width as f32 / height as f32;
    let image_aspect = product.pixel_aspect_ratio * resolution_aspect;
    if image_aspect <= 0.0 {
        return;
    }
    let aperture_aspect = ap_width / ap_height;
    match product.aspect_ratio_conform_policy {
        Policy::AdjustPixelAspectRatio => {
            product.pixel_aspect_ratio = aperture_aspect / resolution_aspect;
        }
        Policy::AdjustApertureWidth => product.aperture_size[0] = ap_height * image_aspect,
        Policy::AdjustApertureHeight => product.aperture_size[1] = ap_width / image_aspect,
        Policy::ExpandAperture if aperture_aspect > image_aspect => {
            product.aperture_size[1] = ap_width / image_aspect;
        }
        Policy::ExpandAperture => product.aperture_size[0] = ap_height * image_aspect,
        Policy::CropAperture if aperture_aspect > image_aspect => {
            product.aperture_size[0] = ap_height * image_aspect;
        }
        Policy::CropAperture => product.aperture_size[1] = ap_width / image_aspect,
        Policy::Other(_) => {}
    }
}

impl Scene<'_> {
    /// Collects extra settings from authored namespaced attributes and all
    /// authored relationships. Namespace filters match the outermost
    /// namespace before the first `:`, after removing `outputs:`.
    /// An empty filter accepts every namespaced attribute.
    ///
    /// Output settings record the prim paths supplying their values,
    /// following connections through node graphs.
    /// Schema attributes without a namespace are excluded. Relationships
    /// are retained without filtering, as OpenUSD26.8 does.
    ///
    /// OpenUSD: `UsdRenderComputeNamespacedSettings`.
    #[must_use]
    pub fn render_namespaced_settings(
        &self,
        path: PathId,
        namespaces: &[&str],
    ) -> NamespacedSettings {
        let mut out = BTreeMap::new();
        let prim = PrimView::new(*self, path);
        for property in self.stage().authored_property_names(path, self.store()) {
            let name = self.store().tokens().resolve(property);
            let Some(declaration) = self.stage().resolve_property_declaration(path, property)
            else {
                continue;
            };
            if declaration.kind == PropertyKind::Relationship {
                out.insert(
                    Arc::from(name),
                    RenderSettingValue::Paths(prim.read_targets(name)),
                );
                continue;
            }
            let basename = name.strip_prefix("outputs:").unwrap_or(name);
            let Some((namespace, _)) = basename.split_once(':') else {
                continue;
            };
            if !namespaces.is_empty() && !namespaces.contains(&namespace) {
                continue;
            }
            if name.starts_with("outputs:") {
                let sources = self.value_sources(PropertyPath::new(path, property));
                if !sources.sources.is_empty() {
                    out.insert(
                        Arc::from(name),
                        RenderSettingValue::Paths(
                            sources
                                .sources
                                .into_iter()
                                .map(|s| TargetPath::Prim(s.attribute.prim_path()))
                                .collect(),
                        ),
                    );
                    continue;
                }
            }
            if let Some(value) = prim.raw_value(name, Time::Default) {
                out.insert(Arc::from(name), RenderSettingValue::Value(value));
            }
        }
        out
    }
}

impl RenderSettings<'_> {
    /// Computes products, shared channels, camera conforming and scene
    /// filtering from default values. Products override shared settings
    /// only with authored readable values; an empty product camera keeps
    /// the shared camera. Channels are deduplicated by prim path while
    /// each product retains ordered indices.
    ///
    /// Invalid cameras and channels are reported in [`RenderSpec::problems`].
    /// Missing or wrong-type product prims are ignored, matching OpenUSD.
    /// No implicit default product is manufactured when `products` is empty.
    /// Relationship forwarding skips cyclic branches while retaining valid
    /// targets from other branches; those cycles are not separate problems.
    ///
    /// ```
    /// use layerstack::PathId;
    /// use layerstack_schemas::{Scene, usd_render::RenderSettings};
    /// # fn specification(scene: Scene<'_>, path: PathId) {
    /// let settings = RenderSettings::new(&scene, path).unwrap();
    /// let spec = settings.compute_spec(&["ri"]);
    /// for product in &spec.products {
    ///     let channels = product.render_var_indices.iter()
    ///         .map(|index| &spec.render_vars[*index]);
    ///     // Configure this output using product.camera, resolution,
    ///     // conformed aperture_size, data_window_ndc and its channels.
    ///     # let _ = channels;
    /// }
    /// # }
    /// ```
    ///
    /// OpenUSD: `UsdRenderComputeSpec` in `usdRender/spec.cpp`.
    /// AOUSD Core §12.3, §12.4 govern authored values and relationships.
    #[must_use]
    pub fn compute_spec(&self, namespaces: &[&str]) -> RenderSpec {
        let scene = self.scene();
        let mut spec = RenderSpec {
            included_purposes: self
                .included_purposes()
                .unwrap_or_default()
                .into_iter()
                .map(Arc::from)
                .collect(),
            material_binding_purposes: self
                .material_binding_purposes()
                .unwrap_or_default()
                .into_iter()
                .map(Arc::from)
                .collect(),
            namespaced_settings: scene.render_namespaced_settings(self.path(), namespaces),
            ..RenderSpec::default()
        };
        let base = Base::read(self);
        let targets = self
            .property_path(Self::PRODUCTS)
            .map(|p| forwarded_targets(&scene, p))
            .unwrap_or_default();
        let mut indices = BTreeMap::new();
        for target in targets {
            let TargetPath::Prim(path) = target else {
                continue;
            };
            let Some(product) = RenderProduct::new(&scene, path) else {
                continue;
            };
            let mut own = base.clone();
            own.apply(&product, false);
            let camera = match own.camera {
                Some(TargetPath::Prim(camera)) if scene.is_a(camera, "Camera") => camera,
                target => {
                    spec.problems.push(RenderProblem::InvalidCamera {
                        product: path,
                        target,
                    });
                    continue;
                }
            };
            let camera_view = PrimView::new(scene, camera);
            let mut computed = ComputedRenderProduct {
                path,
                product_type: product
                    .product_type()
                    .unwrap_or(RenderProductProductType::Raster),
                name: product
                    .product_name()
                    .map(Arc::from)
                    .unwrap_or_else(|| Arc::from("")),
                camera,
                resolution: own.resolution,
                pixel_aspect_ratio: own.pixel_aspect_ratio,
                aspect_ratio_conform_policy: own.policy,
                data_window_ndc: own.data_window,
                disable_motion_blur: own.disable_motion_blur,
                disable_depth_of_field: own.disable_depth_of_field,
                aperture_size: [
                    camera_view
                        .read_value("horizontalAperture", crate::value::read_float)
                        .unwrap_or(0.0),
                    camera_view
                        .read_value("verticalAperture", crate::value::read_float)
                        .unwrap_or(0.0),
                ],
                render_var_indices: Vec::new(),
                namespaced_settings: scene.render_namespaced_settings(path, namespaces),
            };
            conform(&mut computed);
            let vars = product
                .property_path(RenderProduct::ORDERED_VARS)
                .map(|p| forwarded_targets(&scene, p))
                .unwrap_or_default();
            for target in vars {
                let TargetPath::Prim(var_path) = target else {
                    spec.problems.push(RenderProblem::InvalidRenderVar {
                        product: path,
                        target,
                    });
                    continue;
                };
                if let Some(index) = indices.get(&var_path) {
                    computed.render_var_indices.push(*index);
                    continue;
                }
                let Some(var) = RenderVar::new(&scene, var_path) else {
                    spec.problems.push(RenderProblem::InvalidRenderVar {
                        product: path,
                        target,
                    });
                    continue;
                };
                let index = spec.render_vars.len();
                spec.render_vars.push(ComputedRenderVar {
                    path: var_path,
                    data_type: var
                        .data_type()
                        .map(Arc::from)
                        .unwrap_or_else(|| Arc::from("")),
                    source_name: var.source_name().unwrap_or_else(|| Arc::from("")),
                    source_type: var.source_type().unwrap_or(RenderVarSourceType::Raw),
                    namespaced_settings: scene.render_namespaced_settings(var_path, namespaces),
                });
                indices.insert(var_path, index);
                computed.render_var_indices.push(index);
            }
            spec.products.push(computed);
        }
        spec
    }
}
