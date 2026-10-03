// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit scene-to-engine link membership capture; GPU adapters consume the decisions.
use super::LightCaptureError;
use crate::{
    CollectionIdentity, MembershipCache, MembershipSample, Scene,
    collection::{MembershipProblem, MembershipQuery},
    usd_lux::{LightApi, LightFilter},
};
use alloc::vec::Vec;
use layerstack::{PathId, TargetPath};

/// Owned link decisions in caller-supplied target order, including duplicates.
/// Decisions describe exactly the scene used to capture them. Recompute after
/// scene changes, including predicate inputs on geometry when using expressions.
/// This is a CPU capture step; evaluating USD collections inside shaders is unnecessary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkMembership {
    /// Source light or filter, scoped to the originating store.
    pub owner: PathId,
    /// Candidate geometry/attribute identities in caller order.
    pub targets: Vec<TargetPath>,
    /// One decision per target; suitable for engine-owned masks/compacted indices.
    pub included: Vec<bool>,
    /// Collection resolution failures, even when some targets were included.
    pub problems: Vec<MembershipProblem>,
}
fn capture(
    scene: &Scene<'_>,
    owner: PathId,
    query: MembershipQuery,
    targets: &[TargetPath],
) -> LinkMembership {
    LinkMembership {
        owner,
        targets: targets.to_vec(),
        included: targets
            .iter()
            .map(|p| query.is_included(scene, *p).is_included())
            .collect(),
        problems: query.problems().to_vec(),
    }
}
/// Illumination and shadow link decisions captured against one scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightLinkMembership {
    /// Which targets receive illumination.
    pub illumination: LinkMembership,
    /// Which targets cast shadows for this light.
    pub shadows: LinkMembership,
}
impl LightLinkMembership {
    /// Captures both collections, including schema includeRoot=true fallbacks.
    /// AOUSD Core §15 (collections); OpenUSD `UsdLuxLightAPI` linking collections.
    pub fn read(
        scene: &Scene<'_>,
        light: PathId,
        targets: &[TargetPath],
    ) -> Result<Self, LightCaptureError> {
        if !scene.stage().has_prim(light) {
            return Err(LightCaptureError::MissingPrim(light));
        }
        let Some(api) = LightApi::get(scene, light) else {
            return Err(LightCaptureError::NotLight(light));
        };
        Ok(Self {
            illumination: capture(
                scene,
                light,
                api.light_link_collection().membership_query(),
                targets,
            ),
            shadows: capture(
                scene,
                light,
                api.shadow_link_collection().membership_query(),
                targets,
            ),
        })
    }
}
impl LightFilter<'_> {
    /// Captures filterLink decisions in caller order against this filter's scene.
    #[must_use]
    pub fn capture_link_membership(&self, targets: &[TargetPath]) -> LinkMembership {
        capture(
            &self.scene(),
            self.path(),
            self.filter_link_collection().membership_query(),
            targets,
        )
    }
}

/// Retained illumination and shadow decisions with independent query and
/// decision revisions. Consumers choose their own masks or index buffers.
#[derive(Clone, Copy, Debug)]
pub struct LightLinkSample<'a> {
    /// Targets receiving illumination.
    pub illumination: MembershipSample<'a>,
    /// Targets casting shadows for the light.
    pub shadows: MembershipSample<'a>,
}
impl MembershipCache {
    /// Retains both USD light-link collections, including schema fallbacks.
    /// Feed edit reports to `MembershipCache::apply_changes` before recapturing.
    /// Query problems remain available through each sample's `query`.
    /// AOUSD Core §15; OpenUSD `UsdLuxLightAPI` linking collections.
    pub fn capture_light_links<'a>(
        &'a mut self,
        scene: &Scene<'_>,
        light: PathId,
        targets: &[TargetPath],
    ) -> Result<LightLinkSample<'a>, LightCaptureError> {
        if !scene.stage().has_prim(light) {
            return Err(LightCaptureError::MissingPrim(light));
        }
        if LightApi::get(scene, light).is_none() {
            return Err(LightCaptureError::NotLight(light));
        }
        let illumination = CollectionIdentity {
            owner: light,
            name: "lightLink".into(),
        };
        let shadows = CollectionIdentity {
            owner: light,
            name: "shadowLink".into(),
        };
        let light_evaluated = self
            .prepare(scene, &illumination, targets)
            .expect("existing light and valid builtin collection name");
        let shadow_evaluated = self
            .prepare(scene, &shadows, targets)
            .expect("existing light and valid builtin collection name");
        Ok(LightLinkSample {
            illumination: self.sample(&illumination, light_evaluated),
            shadows: self.sample(&shadows, shadow_evaluated),
        })
    }
    /// Retains the filter's `filterLink` collection. As with fresh filter
    /// captures, an unapplied collection uses its schema defaults.
    /// AOUSD Core §15; OpenUSD `UsdLuxLightFilter` linking collection.
    pub fn capture_filter_links<'a>(
        &'a mut self,
        filter: &LightFilter<'_>,
        targets: &[TargetPath],
    ) -> Result<MembershipSample<'a>, crate::MembershipCacheError> {
        self.capture(&filter.scene(), filter.path(), "filterLink", targets)
    }
}
