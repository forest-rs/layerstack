// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit bounded host orchestration for clip query demands.
use super::*;
use layerstack::{
    PropertyPath, Time,
    value_clips::{ClipAssetRequest, ClipQueryPlan},
};

/// One failed demanded source, retaining its exact anchor and transport failure.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ClipLoadFailure {
    /// Requested source and preparation availability.
    pub request: ClipAssetRequest,
    /// Original load error. A malformed source is never recorded as missing.
    pub error: IoError,
}
/// Explicit load work and the final resumable query plan.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ClipQueryPreparation {
    /// Plan after successful loads and confirmed missing results.
    pub plan: ClipQueryPlan,
    /// Actual source load attempts, including failures and confirmed misses.
    pub load_attempts: usize,
    /// Typed failures; malformed resources keep the query incomplete.
    pub failures: Vec<ClipLoadFailure>,
    /// More pending requests remain after consuming the load-attempt budget.
    pub budget_exhausted: bool,
}
impl<B: Storage> StageDocument<B> {
    /// Satisfies a query plan through explicit host I/O, replanning after every
    /// successful load. `max_loads` bounds source requests (each may import layer
    /// arcs); `max_demands` bounds each returned plan. Missing files are confirmed
    /// to the store; malformed files retain typed failures and may be retried on
    /// a later call. Already complete queries perform no I/O. Numeric evaluation
    /// remains separate. Clips hold time samples only: at [`Time::Default`] the
    /// plan demands nothing and no load is attempted. AOUSD Core §12.3, §12.5.
    ///
    /// ```no_run
    /// # #[cfg(feature = "std")]
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use layerstack::{StageOptions, Time};
    /// use layerstack_io::{Filesystem, StageDocument};
    /// let mut document = StageDocument::open(
    ///     Filesystem::new(".", [])?, "motion.usda", StageOptions::default())?;
    /// let property = document.store_mut().property_path("/Model.height");
    /// let prepared = document.prepare_clip_query(property, Time::at(5.), 8, 32);
    /// if !prepared.plan.is_complete() {
    ///     // Inspect failures, pending anchored demands and budget exhaustion;
    ///     // retry after fixing transport or increasing the explicit budget.
    ///     println!("{prepared:?}");
    /// } else {
    ///     let height = document.view().attribute(property);
    ///     let value = height.map(|height| height.try_get(Time::at(5.))).transpose()?;
    ///     println!("{value:?}"); // Complete gaps remain `None`.
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn prepare_clip_query(
        &mut self,
        property: PropertyPath,
        time: Time,
        max_loads: usize,
        max_demands: usize,
    ) -> ClipQueryPreparation {
        self.synchronize();
        let mut attempted = BTreeSet::new();
        let mut failures = Vec::new();
        loop {
            let plan = self
                .stage
                .stage()
                .plan_clip_query(property, time, max_demands);
            let next = plan
                .pending()
                .find(|d| !attempted.contains(&(d.request.anchor, d.request.identifier.clone())))
                .map(|d| d.request.clone());
            let Some(request) = next else {
                return ClipQueryPreparation {
                    plan,
                    load_attempts: attempted.len(),
                    failures,
                    budget_exhausted: false,
                };
            };
            if attempted.len() == max_loads {
                return ClipQueryPreparation {
                    plan,
                    load_attempts: attempted.len(),
                    failures,
                    budget_exhausted: true,
                };
            }
            attempted.insert((request.anchor, request.identifier.clone()));
            if let Err(error) = self.load_asset(&request.identifier, Some(request.anchor)) {
                if error.kind == IoErrorKind::NotFound {
                    self.store
                        .mark_asset_missing(request.anchor, &request.identifier);
                    self.synchronize();
                } else {
                    failures.push(ClipLoadFailure { request, error });
                }
            }
        }
    }
}
