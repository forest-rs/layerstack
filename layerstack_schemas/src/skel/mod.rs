// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Skeleton topology, pose evaluation and CPU deformation, without a renderer.
//!
//! Queries retain definition snapshots and read animation at explicit times.
//! Rebuild snapshots after scene edits, or retain evaluation through [`SkelCache`]
//! and pass explicit edit reports. No global cache or implicit edit observation.
//! Matrices use USD row vectors. OpenUSD: `UsdSkelSkeletonQuery` and
//! `UsdSkelBindingAPI`; composition follows AOUSD Core §12.3–12.4.

use crate::{
    PrimView, Scene, Time, gf,
    usd_skel::{SkelAnimation, SkelBindingApi, Skeleton},
};
mod cache;
mod decomposition;
mod dual_quaternion;
pub use cache::{SkelCache, SkelCacheMemory, SkelCacheStats};
pub use dual_quaternion::DualQuaternionJoint;
mod helpers;
pub use helpers::JointTransformComponents;
mod normals;
pub use normals::{
    rigid_skinning_transform, rigid_skinning_transform_with_method, skin_face_varying_normals,
    skin_face_varying_normals_with_method, skin_normals, skin_normals_in_place,
    skin_normals_in_place_with_method, skin_normals_with_method,
};
mod blend_shapes;
pub use blend_shapes::{
    BlendShapeContribution, BlendShapeQuery, BlendShapeSample, apply_blend_shape,
    apply_blend_shape_in_place,
};
mod inputs;
#[cfg(feature = "simd")]
mod simd;
pub use inputs::{
    DeformationInputs, DeformationRevisions, NormalSkinningInputs, SkinningBindingInputs,
};
mod skinning;
pub use skinning::{
    InfluenceInterpolation, JointInfluences, SkinningMethod, SkinningQuery, skin_points,
    skin_points_in_place, skin_points_in_place_with_method, skin_points_with_method,
};

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use layerstack::{HashMap, PathId, PropertyPath, TargetPath, TokenInterner, Value};

/// Invalid skeletal inputs; evaluation returns no partial result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkelError {
    /// A joint is duplicated, malformed or ordered before its parent.
    InvalidTopology {
        /// Offending joint in the supplied order.
        joint: usize,
    },
    /// A required property is missing, blocked or incompatible.
    InvalidInput {
        /// Prim owning the input.
        prim: PathId,
        /// USD property name.
        property: &'static str,
    },
    /// A property has the wrong number of values.
    ArrayLength {
        /// Prim owning the array.
        prim: PathId,
        /// USD property name.
        property: &'static str,
        /// Required length.
        expected: usize,
        /// Resolved length.
        actual: usize,
    },
    /// A binding relationship targets an incompatible or missing prim.
    InvalidTarget {
        /// Prim authoring the relationship.
        prim: PathId,
        /// USD relationship name.
        relationship: &'static str,
        /// Invalid forwarded target.
        target: TargetPath,
    },
    /// Indexed binding data could not be flattened.
    Primvar {
        /// Prim owning the influence primvar.
        prim: PathId,
        /// The precise flattening failure.
        source: crate::primvar::PrimvarError,
    },
    /// A CPU deformation array is malformed.
    InvalidDeformation {
        /// Offending element, if the failure is element-specific.
        element: Option<usize>,
        /// Concrete validation failure.
        reason: &'static str,
    },
    /// The authored token is neither `classicLinear` nor `dualQuaternion`.
    UnsupportedSkinningMethod {
        /// Bound geometry requesting the unsupported method.
        prim: PathId,
    },
    /// A normal transform has a singular or nonfinite linear component.
    SingularNormalTransform {
        /// Joint index, or `None` for the geometry bind transform.
        joint: Option<usize>,
    },
    /// A joint's bind matrix cannot be inverted.
    SingularBind {
        /// Joint index in skeleton order.
        joint: usize,
    },
}
impl core::fmt::Display for SkelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid skeleton input: {self:?}")
    }
}
impl core::error::Error for SkelError {}

/// Validated parent-first forest. Missing intermediate paths attach to the
/// closest listed ancestor, as `UsdSkelTopology` does; multiple roots are valid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JointTopology {
    parents: Vec<Option<usize>>,
}
impl JointTopology {
    /// Derives parents from relative joint paths, rejecting duplicates and
    /// parents appearing after children. Uses nearest listed path ancestors.
    pub fn new(joints: &[&str]) -> Result<Self, SkelError> {
        let mut indices = HashMap::new();
        for (joint, path) in joints.iter().enumerate() {
            if path.split('/').any(|s| {
                s.is_empty() || s == "." || s == ".." || s.contains(['.', '{', '}', ':', '[', ']'])
            }) || indices.insert(*path, joint).is_some()
            {
                return Err(SkelError::InvalidTopology { joint });
            }
        }
        let mut parents = Vec::with_capacity(joints.len());
        for (joint, path) in joints.iter().enumerate() {
            let mut parent = *path;
            let index = loop {
                let Some((prefix, _)) = parent.rsplit_once('/') else {
                    break None;
                };
                if let Some(&index) = indices.get(prefix) {
                    break Some(index);
                }
                parent = prefix;
            };
            if index.is_some_and(|parent| parent >= joint) {
                return Err(SkelError::InvalidTopology { joint });
            }
            parents.push(index);
        }
        Ok(Self { parents })
    }
    /// Parent indices in joint order; `None` denotes a root.
    #[must_use]
    pub fn parents(&self) -> &[Option<usize>] {
        &self.parents
    }
    /// Concatenates local joint matrices into skeleton space. Roots remain
    /// local; each child applies its parent's skeleton-space matrix afterwards.
    pub fn concatenate(&self, local: &[[[f64; 4]; 4]]) -> Option<Vec<[[f64; 4]; 4]>> {
        if local.len() != self.parents.len() {
            return None;
        }
        let mut result = Vec::with_capacity(local.len());
        for (matrix, parent) in local.iter().zip(&self.parents) {
            result.push(parent.map_or(*matrix, |parent| gf::mul(matrix, &result[parent])));
        }
        Some(result)
    }
}

pub(super) fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Option<T> {
    match time {
        Time::Default => prim.read_value(name, decode),
        Time::At {
            code,
            interpolation,
        } => prim.read_value_at(name, code, interpolation, decode),
    }
}
pub(super) fn tokens(prim: &PrimView<'_>, name: &str) -> Option<Vec<String>> {
    prim.read_value(name, |v, t| {
        crate::value::read_array(v, t, crate::value::read_token)
            .map(|v| v.into_iter().map(ToString::to_string).collect())
    })
}
pub(super) fn invalid(prim: PathId, property: &'static str) -> SkelError {
    SkelError::InvalidInput { prim, property }
}
pub(super) fn length(
    prim: PathId,
    property: &'static str,
    expected: usize,
    actual: usize,
) -> Result<(), SkelError> {
    if actual == expected {
        Ok(())
    } else {
        Err(SkelError::ArrayLength {
            prim,
            property,
            expected,
            actual,
        })
    }
}

// Authored empty relationships stop inheritance. Forward relationship chains
// through the ordinary resolver (AOUSD Core §12.4), not a second composition.
fn inherited_target(
    scene: &Scene<'_>,
    path: PathId,
    name: &'static str,
    schema: &str,
) -> Result<Option<PathId>, SkelError> {
    inherited_target_with_root(scene, path, name, schema, None)
}

// Cache population starts with empty binding state at its traversal root.
// Standalone inherited relationship queries continue through all ancestors.
fn inherited_target_with_root(
    scene: &Scene<'_>,
    mut path: PathId,
    name: &'static str,
    schema: &str,
    root: Option<PathId>,
) -> Result<Option<PathId>, SkelError> {
    loop {
        if scene.has_api(path, "SkelBindingAPI", None)
            && let Some(token) = scene.store().tokens().lookup(name)
        {
            let property = PropertyPath::new(path, token);
            let authored = scene
                .stage()
                .explain_property_path(property)
                .is_some_and(|opinions| {
                    opinions
                        .iter()
                        .any(|o| o.value.as_property().is_some_and(|p| p.targets.is_some()))
                });
            let targets = crate::view::forwarded_targets(scene, property);
            if authored || !targets.is_empty() {
                return match targets.first() {
                    None => Ok(None),
                    Some(&TargetPath::Prim(target)) if scene.is_a(target, schema) => {
                        Ok(Some(target))
                    }
                    Some(&target) => Err(SkelError::InvalidTarget {
                        prim: path,
                        relationship: name,
                        target,
                    }),
                };
            }
        }
        if Some(path) == root {
            return Ok(None);
        }
        let Some(parent) = scene.parent(path) else {
            return Ok(None);
        };
        path = parent;
    }
}
impl<'a> SkelBindingApi<'a> {
    /// Closest authored skeleton binding, including an explicit empty binding
    /// that stops inheritance. Only prims applying `SkelBindingAPI` contribute.
    pub fn inherited_skeleton(&self) -> Result<Option<Skeleton<'a>>, SkelError> {
        inherited_target(&self.scene(), self.path(), "skel:skeleton", "Skeleton")
            .map(|path| path.and_then(|p| Skeleton::new(&self.scene(), p)))
    }
    /// Closest authored animation binding; an empty relationship stops lookup.
    pub fn inherited_animation_source(&self) -> Result<Option<SkelAnimation<'a>>, SkelError> {
        inherited_target(
            &self.scene(),
            self.path(),
            "skel:animationSource",
            "SkelAnimation",
        )
        .map(|path| path.and_then(|p| SkelAnimation::new(&self.scene(), p)))
    }
}

/// A skeleton definition snapshot with retained topology and rest/bind arrays.
/// Animation values resolve at each requested time. Rebuild after scene edits.
#[derive(Clone, Debug)]
pub struct SkeletonQuery<'a> {
    skeleton: Skeleton<'a>,
    definition: Arc<SkeletonDefinition>,
    animation: Option<SkelAnimation<'a>>,
}
#[derive(Clone, Debug)]
struct SkeletonDefinition {
    path: PathId,
    joints: Vec<String>,
    topology: JointTopology,
    rest: Option<Vec<gf::Matrix4>>,
    bind: Option<Vec<gf::Matrix4>>,
    animation_path: Option<PathId>,
    animation_joints: Vec<String>,
    blend_shapes: Vec<String>,
    mapping: Vec<Option<usize>>,
}
impl SkeletonDefinition {
    fn query<'a>(self: &Arc<Self>, scene: &Scene<'a>) -> Result<SkeletonQuery<'a>, SkelError> {
        let skeleton =
            Skeleton::new(scene, self.path).ok_or_else(|| invalid(self.path, "joints"))?;
        Ok(SkeletonQuery {
            skeleton,
            definition: Arc::clone(self),
            animation: self
                .animation_path
                .and_then(|p| SkelAnimation::new(scene, p)),
        })
    }
    fn validated_bind_transforms(&self) -> Result<&[gf::Matrix4], SkelError> {
        let bind = self
            .bind
            .as_ref()
            .ok_or_else(|| invalid(self.path, "bindTransforms"))?;
        length(self.path, "bindTransforms", self.joints.len(), bind.len())?;
        Ok(bind)
    }
    fn inverse_bind_transforms(&self) -> Result<Vec<gf::Matrix4>, SkelError> {
        let bind = self.validated_bind_transforms()?;
        bind.iter()
            .enumerate()
            .map(|(joint, m)| {
                let (inverse, determinant) = gf::inverse(m);
                if determinant == 0. {
                    Err(SkelError::SingularBind { joint })
                } else {
                    Ok(inverse)
                }
            })
            .collect()
    }
}
impl<'a> Skeleton<'a> {
    /// Prepares validated topology and a sparse animation-to-skeleton mapping.
    pub fn query(&self) -> Result<SkeletonQuery<'a>, SkelError> {
        SkeletonQuery::new(*self)
    }
}
impl<'a> SkeletonQuery<'a> {
    /// Captures uniform definition inputs and the inherited animation source.
    pub fn new(skeleton: Skeleton<'a>) -> Result<Self, SkelError> {
        let joints =
            tokens(&skeleton, "joints").ok_or_else(|| invalid(skeleton.path(), "joints"))?;
        let topology = JointTopology::new(&joints.iter().map(String::as_str).collect::<Vec<_>>())?;
        let animation = inherited_target(
            &skeleton.scene(),
            skeleton.path(),
            "skel:animationSource",
            "SkelAnimation",
        )?
        .and_then(|p| SkelAnimation::new(&skeleton.scene(), p));
        let animation_joints = animation
            .as_ref()
            .and_then(|a| tokens(a, "joints"))
            .unwrap_or_default();
        // Animation order may differ from hierarchy order; only uniqueness is
        // required. Unmapped animation joints do not affect this skeleton.
        let mut indices = HashMap::new();
        for (i, joint) in animation_joints.iter().enumerate() {
            if indices.insert(joint, i).is_some() {
                return Err(SkelError::InvalidTopology { joint: i });
            }
        }
        let mapping = joints.iter().map(|j| indices.get(j).copied()).collect();
        drop(indices);
        Ok(Self {
            skeleton,
            animation,
            definition: Arc::new(SkeletonDefinition {
                path: skeleton.path(),
                topology,
                joints,
                rest: skeleton.rest_transforms(),
                bind: skeleton.bind_transforms(),
                animation_path: animation.map(|a| a.path()),
                animation_joints,
                blend_shapes: animation
                    .as_ref()
                    .and_then(|a| tokens(a, "blendShapes"))
                    .unwrap_or_default(),
                mapping,
            }),
        })
    }
    /// Skeleton joint names in stable definition order.
    #[must_use]
    pub fn joints(&self) -> &[String] {
        &self.definition.joints
    }
    /// Validated joint hierarchy.
    #[must_use]
    pub fn topology(&self) -> &JointTopology {
        &self.definition.topology
    }
    /// Skeleton path represented by this query.
    #[must_use]
    pub fn skeleton_path(&self) -> PathId {
        self.skeleton.path()
    }
    /// Animation source, when bound; useful for routing external scene edits.
    #[must_use]
    pub fn animation_path(&self) -> Option<PathId> {
        self.animation.map(|a| a.path())
    }
    /// Joint-local rest transforms, validating the required array length.
    pub fn rest_transforms(&self) -> Result<Vec<gf::Matrix4>, SkelError> {
        let rest = self
            .definition
            .rest
            .as_ref()
            .ok_or_else(|| invalid(self.skeleton.path(), "restTransforms"))?;
        length(
            self.skeleton.path(),
            "restTransforms",
            self.definition.joints.len(),
            rest.len(),
        )?;
        Ok(rest.clone())
    }
    /// Joint-local animation with rest fallback and sparse/reordered mapping.
    /// As `UsdSkelSkeletonQuery`, unreadable TRS arrays fall back to rest;
    /// readable arrays with inconsistent lengths are rejected.
    pub fn local_transforms(&self, time: Time) -> Result<Vec<gf::Matrix4>, SkelError> {
        let Some(animation) = self
            .animation
            .filter(|_| self.definition.mapping.iter().any(Option::is_some))
        else {
            return self.rest_transforms();
        };
        let translations = read(
            &animation,
            "translations",
            time,
            crate::value::read_float3_array,
        );
        let rotations = read(
            &animation,
            "rotations",
            time,
            crate::value::read_quatf_array,
        );
        let scales = read(&animation, "scales", time, crate::value::read_half3_array);
        let (Some(t), Some(r), Some(s)) = (translations, rotations, scales) else {
            return self.rest_transforms();
        };
        for (property, actual) in [
            ("translations", t.len()),
            ("rotations", r.len()),
            ("scales", s.len()),
        ] {
            length(
                animation.path(),
                property,
                self.definition.animation_joints.len(),
                actual,
            )?;
        }
        let mut result = if self.definition.mapping.iter().any(Option::is_none) {
            self.rest_transforms()?
        } else {
            alloc::vec![gf::IDENTITY; self.definition.joints.len()]
        };
        for (out, mapped) in result.iter_mut().zip(&self.definition.mapping) {
            if let Some(i) = *mapped {
                *out = compose(t[i], r[i], s[i]);
            }
        }
        Ok(result)
    }
    /// Animated joint transforms in skeleton space, excluding skeleton Xforms.
    pub fn skeleton_transforms(&self, time: Time) -> Result<Vec<gf::Matrix4>, SkelError> {
        Ok(self
            .definition
            .topology
            .concatenate(&self.local_transforms(time)?)
            .expect("validated joint count"))
    }
    /// Inverse bind followed by animated skeleton-space transforms, in joint
    /// order. Singular bind matrices return an error before producing output.
    pub fn skinning_transforms(&self, time: Time) -> Result<Vec<gf::Matrix4>, SkelError> {
        let bind = self
            .definition
            .bind
            .as_ref()
            .ok_or_else(|| invalid(self.skeleton.path(), "bindTransforms"))?;
        length(
            self.skeleton.path(),
            "bindTransforms",
            self.definition.joints.len(),
            bind.len(),
        )?;
        let animated = self.skeleton_transforms(time)?;
        bind.iter()
            .zip(animated)
            .enumerate()
            .map(|(joint, (bind, animated))| {
                let (inverse, determinant) = gf::inverse(bind);
                if determinant == 0. {
                    Err(SkelError::SingularBind { joint })
                } else {
                    Ok(gf::mul(&inverse, &animated))
                }
            })
            .collect()
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "UsdSkelMakeTransform uses float rotations and half scales"
)]
fn compose(t: [f32; 3], r: [f32; 4], s: [f32; 3]) -> gf::Matrix4 {
    // GfMatrix3f rounds each quaternion product/sum before double constants,
    // then rounds each rotation entry before multiplying the half scale.
    let [x, y, z, r] = r;
    let rotation = [
        [
            (1. - 2. * f64::from(y * y + z * z)) as f32,
            (2. * f64::from(x * y + z * r)) as f32,
            (2. * f64::from(z * x - y * r)) as f32,
        ],
        [
            (2. * f64::from(x * y - z * r)) as f32,
            (1. - 2. * f64::from(z * z + x * x)) as f32,
            (2. * f64::from(y * z + x * r)) as f32,
        ],
        [
            (2. * f64::from(z * x + y * r)) as f32,
            (2. * f64::from(y * z - x * r)) as f32,
            (1. - 2. * f64::from(y * y + x * x)) as f32,
        ],
    ];
    let mut result = gf::IDENTITY;
    for (row, rotation) in result.iter_mut().take(3).zip(rotation) {
        row[..3].copy_from_slice(&rotation.map(f64::from));
    }
    for (row, scale) in result.iter_mut().take(3).zip(s) {
        for cell in row.iter_mut().take(3) {
            *cell = f64::from(*cell as f32 * scale);
        }
    }
    result[3][..3].copy_from_slice(&t.map(f64::from));
    result
}
