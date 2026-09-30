// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Visibility and purpose, which inherit down namespace.
//!
//! Each computation is a pure step from a prim's own inputs and its
//! parent's result, as OpenUSD's `UsdGeomImageable` defines it: only
//! `Imageable` prims' opinions count, and any other prim passes its
//! parent's result through. The inputs are the stage reads, gathered for
//! each prim in one place ([`VisibilityInputs::read`],
//! [`PurposeInputs::read`]), so a caller that tracks dependencies (an
//! incremental graph) can record them and rerun the steps. The views'
//! `compute_*` methods fold the steps from the pseudo-root down.

use alloc::vec::Vec;

use layerstack::{PathId, Value};

use crate::usd_geom::{Imageable, ImageablePurpose, VisibilityApi};
use crate::{
    Time,
    view::{PrimView, Scene},
};

/// A computed visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Visibility {
    /// Visible if its ancestors are: no opinion makes it invisible.
    Inherited,
    /// Invisible.
    Invisible,
    /// Visible.
    Visible,
}

impl Visibility {
    /// Its token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherited => "inherited",
            Self::Invisible => "invisible",
            Self::Visible => "visible",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "inherited" => Some(Self::Inherited),
            "invisible" => Some(Self::Invisible),
            "visible" => Some(Self::Visible),
            _ => None,
        }
    }

    /// A prim's visibility from its parent's (`Inherited` for the
    /// pseudo-root) and its own inputs: invisible if its parent is, or if
    /// it is `Imageable` and its `visibility` is `invisible`.
    ///
    /// OpenUSD: `_ComputeVisibility` in `pxr/usd/usdGeom/imageable.cpp`.
    #[must_use]
    pub fn inherit(parent: Self, own: &VisibilityInputs) -> Self {
        if parent == Self::Invisible || own.visibility == Some(Self::Invisible) {
            Self::Invisible
        } else {
            Self::Inherited
        }
    }

    /// The visibility for `purpose` from a prim's visibility and its
    /// purpose visibility ([`VisibilityInputs::purpose_visibility`] folded
    /// down to the prim): invisible if the prim is; visible for `default`;
    /// else the purpose visibility, or with none authored anywhere
    /// `invisible` for `guide` and `inherited` for `proxy` and `render`.
    ///
    /// OpenUSD: `UsdGeomImageable::ComputeEffectiveVisibility`.
    #[must_use]
    pub fn effective(
        visibility: Self,
        purpose: &ImageablePurpose,
        purpose_visibility: Option<Self>,
    ) -> Self {
        if visibility == Self::Invisible {
            return Self::Invisible;
        }
        match purpose {
            ImageablePurpose::Default => Self::Visible,
            ImageablePurpose::Guide => purpose_visibility.unwrap_or(Self::Invisible),
            ImageablePurpose::Proxy | ImageablePurpose::Render => {
                purpose_visibility.unwrap_or(Self::Inherited)
            }
            // OpenUSD reports a coding error.
            ImageablePurpose::Other(_) => Self::Invisible,
        }
    }
}

/// What one prim contributes to visibility at a time: every stage read the
/// visibility computations make for it.
///
/// It reads the prim's type (`Imageable` or not), its `apiSchemas`
/// (`VisibilityAPI` or not), and at the time its `visibility`, fallback
/// included, and each of `guideVisibility`, `proxyVisibility` and
/// `renderVisibility` that an opinion authors at any time, fallback
/// included ([`VisibilityInputs::PROPERTIES`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisibilityInputs {
    /// Whether the prim is `Imageable`; if not, the rest is `None`.
    pub imageable: bool,
    /// Its `visibility`.
    pub visibility: Option<Visibility>,
    /// Its `guideVisibility` at the time, with `VisibilityAPI` applied and
    /// an opinion authoring it.
    pub guide: Option<Visibility>,
    /// Its `proxyVisibility`, likewise.
    pub proxy: Option<Visibility>,
    /// Its `renderVisibility`, likewise.
    pub render: Option<Visibility>,
}

impl VisibilityInputs {
    /// The properties it reads, besides the prim's type and `apiSchemas`.
    pub const PROPERTIES: [&'static str; 4] = [
        Imageable::VISIBILITY,
        VisibilityApi::GUIDE_VISIBILITY,
        VisibilityApi::PROXY_VISIBILITY,
        VisibilityApi::RENDER_VISIBILITY,
    ];

    /// Reads the inputs of the prim at `path` at `time`.
    #[must_use]
    pub fn read(scene: &Scene<'_>, path: PathId, time: Time) -> Self {
        if !scene.is_a(path, Imageable::SCHEMA) {
            return Self::default();
        }
        let prim = PrimView::new(*scene, path);
        let visibility = local_visibility(&prim, time);
        if !scene.has_api(path, VisibilityApi::SCHEMA, None) {
            return Self {
                imageable: true,
                visibility,
                ..Self::default()
            };
        }
        // OpenUSD reads a purpose visibility when any opinion authors it,
        // fallback included: an attribute with only time samples reads its
        // fallback at the default time.
        let authored = |name| {
            if !prim.has_authored_value(name) {
                return None;
            }
            prim.raw_value(name, time)
                .and_then(|v| visibility_token(scene, &v))
        };
        Self {
            imageable: true,
            visibility,
            guide: authored(VisibilityApi::GUIDE_VISIBILITY),
            proxy: authored(VisibilityApi::PROXY_VISIBILITY),
            render: authored(VisibilityApi::RENDER_VISIBILITY),
        }
    }

    /// A prim's visibility for `purpose` (`guide`, `proxy` or `render`)
    /// from its parent's (`None` for the pseudo-root): its own authored
    /// opinion, else its parent's; `None` when nothing authors one.
    ///
    /// OpenUSD: `_ComputePurposeVisibility` in
    /// `pxr/usd/usdGeom/imageable.cpp`.
    #[must_use]
    pub fn purpose_visibility(
        &self,
        purpose: &ImageablePurpose,
        parent: Option<Visibility>,
    ) -> Option<Visibility> {
        let own = match purpose {
            ImageablePurpose::Guide => self.guide,
            ImageablePurpose::Proxy => self.proxy,
            ImageablePurpose::Render => self.render,
            _ => None,
        };
        own.or(parent)
    }
}

/// What one prim contributes to purpose: every stage read the purpose
/// computation makes for it.
///
/// It reads the prim's type (`Imageable` or not) and its `purpose` at the
/// default time: whether an opinion authors it, and its value with the
/// fallback. Purpose is `uniform`, so it does not depend on time. An
/// authored empty `purpose` counts as none for inheritance, as OpenUSD's
/// does, but is the prim's value when it inherits nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurposeInputs {
    /// Whether the prim is `Imageable`.
    pub imageable: bool,
    /// Its authored `purpose`, if any and not empty.
    pub authored: Option<ImageablePurpose>,
    /// Its `purpose` with the fallback (`default` when not `Imageable`).
    pub fallback: ImageablePurpose,
}

impl PurposeInputs {
    /// Reads the inputs of the prim at `path`.
    #[must_use]
    pub fn read(scene: &Scene<'_>, path: PathId) -> Self {
        if !scene.is_a(path, Imageable::SCHEMA) {
            return Self {
                imageable: false,
                authored: None,
                fallback: ImageablePurpose::Default,
            };
        }
        let prim = PrimView::new(*scene, path);
        let purpose = |value: Option<Value>| {
            value.and_then(|v| token(scene, &v).map(ImageablePurpose::from_token))
        };
        let value = purpose(prim.raw_value(Imageable::PURPOSE, Time::Default));
        // An authored empty token is no purpose to inherit, though it is
        // the prim's own value (OpenUSD's `_ComputeAuthoredPurpose` returns
        // it and `ComputePurposeInfo` treats it as none).
        let authored = if prim.has_authored_value(Imageable::PURPOSE) {
            value.clone().filter(|p| !p.as_str().is_empty())
        } else {
            None
        };
        Self {
            imageable: true,
            authored,
            fallback: value.unwrap_or(ImageablePurpose::Default),
        }
    }
}

/// A prim's computed purpose, and whether its descendants inherit it.
///
/// OpenUSD: `UsdGeomImageable::PurposeInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurposeInfo {
    /// The purpose.
    pub purpose: ImageablePurpose,
    /// Whether it was authored (on the prim or an ancestor), and so is
    /// inherited by descendants that author none; a fallback purpose is
    /// not.
    pub inheritable: bool,
    /// The prim whose authored `purpose` it is; `None` for a fallback.
    pub authored_on: Option<PathId>,
}

impl PurposeInfo {
    /// The purpose info of the prim at `path` from its parent's (`None`
    /// for the pseudo-root) and its own inputs: its own authored purpose,
    /// else its parent's if inheritable, else its fallback, which is not.
    ///
    /// OpenUSD: `UsdGeomImageable::ComputePurposeInfo(parentPurposeInfo)`.
    #[must_use]
    pub fn inherit(parent: Option<&Self>, own: &PurposeInputs, path: PathId) -> Self {
        if let Some(purpose) = &own.authored {
            return Self {
                purpose: purpose.clone(),
                inheritable: true,
                authored_on: Some(path),
            };
        }
        match parent {
            Some(parent) if parent.inheritable => parent.clone(),
            _ => Self {
                purpose: own.fallback.clone(),
                inheritable: false,
                authored_on: None,
            },
        }
    }
}

fn token<'a>(scene: &Scene<'a>, value: &Value) -> Option<&'a str> {
    match value {
        Value::Token(token) => Some(scene.store().tokens().resolve(*token)),
        _ => None,
    }
}

fn visibility_token(scene: &Scene<'_>, value: &Value) -> Option<Visibility> {
    token(scene, value).and_then(Visibility::from_token)
}

// The prim is already known to be Imageable. Bounds need only ordinary
// visibility, not VisibilityAPI's independent purpose-visibility inputs.
pub(crate) fn local_visibility(prim: &PrimView<'_>, time: Time) -> Option<Visibility> {
    prim.raw_value(Imageable::VISIBILITY, time)
        .and_then(|value| visibility_token(&prim.scene(), &value))
}

/// The prims from the pseudo-root's children down to `path`, root first.
fn lineage(scene: &Scene<'_>, path: PathId) -> Vec<PathId> {
    let mut out: Vec<PathId> = core::iter::successors(Some(path), |at| scene.parent(*at))
        .filter(|at| scene.parent(*at).is_some())
        .collect();
    out.reverse();
    out
}

impl<'a> Imageable<'a> {
    /// The prim's visibility at `time`: [`Visibility::Invisible`] if it or
    /// an ancestor is `invisible`, else [`Visibility::Inherited`].
    ///
    /// OpenUSD: `UsdGeomImageable::ComputeVisibility`.
    #[must_use]
    pub fn compute_visibility(&self, time: Time) -> Visibility {
        let scene = self.scene();
        lineage(&scene, self.path())
            .into_iter()
            .fold(Visibility::Inherited, |parent, at| {
                Visibility::inherit(parent, &VisibilityInputs::read(&scene, at, time))
            })
    }

    /// The prim's visibility for `purpose` at `time` ([`Visibility::effective`]):
    ///
    /// - invisible if [`Imageable::compute_visibility`] is;
    /// - for the `default` purpose, else visible;
    /// - otherwise the nearest authored `guideVisibility`,
    ///   `proxyVisibility` or `renderVisibility` of a prim with
    ///   `VisibilityAPI` applied, on the prim or an ancestor;
    /// - with none, `invisible` for `guide` and `inherited` for `proxy` and
    ///   `render`.
    ///
    /// OpenUSD: `UsdGeomImageable::ComputeEffectiveVisibility`.
    #[must_use]
    pub fn compute_effective_visibility(
        &self,
        purpose: &ImageablePurpose,
        time: Time,
    ) -> Visibility {
        let scene = self.scene();
        let (visibility, purpose_visibility) = lineage(&scene, self.path()).into_iter().fold(
            (Visibility::Inherited, None),
            |(visibility, purpose_visibility), at| {
                let own = VisibilityInputs::read(&scene, at, time);
                (
                    Visibility::inherit(visibility, &own),
                    own.purpose_visibility(purpose, purpose_visibility),
                )
            },
        );
        Visibility::effective(visibility, purpose, purpose_visibility)
    }

    /// The prim's purpose, and whether descendants inherit it: its own
    /// authored `purpose`, else the nearest ancestor `Imageable` prim's
    /// authored `purpose`, else its `purpose` fallback (`default`), which
    /// is not inherited ([`PurposeInfo::inherit`]).
    ///
    /// OpenUSD: `UsdGeomImageable::ComputePurposeInfo`.
    #[must_use]
    pub fn compute_purpose_info(&self) -> PurposeInfo {
        let scene = self.scene();
        let mut info: Option<PurposeInfo> = None;
        for at in lineage(&scene, self.path()) {
            info = Some(PurposeInfo::inherit(
                info.as_ref(),
                &PurposeInputs::read(&scene, at),
                at,
            ));
        }
        info.unwrap_or(PurposeInfo {
            purpose: ImageablePurpose::Default,
            inheritable: false,
            authored_on: None,
        })
    }

    /// The prim's purpose ([`Imageable::compute_purpose_info`]).
    ///
    /// OpenUSD: `UsdGeomImageable::ComputePurpose`.
    #[must_use]
    pub fn compute_purpose(&self) -> ImageablePurpose {
        self.compute_purpose_info().purpose
    }
}
