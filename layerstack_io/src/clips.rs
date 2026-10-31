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
/// A demanded clip source that storage cannot supply yet
/// ([`IoErrorKind::Unavailable`]): a host that fetches sources
/// asynchronously, such as a browser, supplies it under
/// [`Self::identifier`] and prepares the query again. It may still arrive,
/// so it is neither a failure nor missing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnavailableClipAsset {
    /// The demanded source, with its anchor, role and clip set.
    pub request: ClipAssetRequest,
    /// The source to supply, as storage named it for the request's
    /// identifier anchored to its layer: the key a host fetches and
    /// supplies bytes under, as [`crate::UnavailableAsset::identifier`].
    pub identifier: String,
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
    /// Demanded sources storage cannot supply yet, each once, in demand
    /// order. They stay pending in [`Self::plan`] until supplied (or
    /// confirmed absent) and prepared again.
    pub unavailable: Vec<UnavailableClipAsset>,
    /// More pending requests remain after consuming the load-attempt budget.
    pub budget_exhausted: bool,
}
impl<B: Storage> StageDocument<B> {
    /// Satisfies a query plan through explicit host I/O, replanning after every
    /// successful load. `max_loads` bounds source requests (each may import layer
    /// arcs); `max_demands` bounds each returned plan. Missing files are confirmed
    /// to the store; malformed files retain typed failures and may be retried on
    /// a later call. Sources storage cannot supply yet are listed as
    /// [`ClipQueryPreparation::unavailable`], not failures: a host that
    /// fetches asynchronously supplies them and calls this again, until the
    /// plan is complete. Already complete queries perform no I/O. Numeric evaluation
    /// remains separate. At [`Time::Default`] it loads what untimed
    /// introspection needs: the clip metadata and single clips that settle
    /// whether the attribute might vary with time
    /// ([`layerstack::Stage::plan_clip_query`]). AOUSD Core §12.3, §12.5.
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
        let mut unavailable = Vec::new();
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
                    unavailable,
                    budget_exhausted: false,
                };
            };
            if attempted.len() == max_loads {
                return ClipQueryPreparation {
                    plan,
                    load_attempts: attempted.len(),
                    failures,
                    unavailable,
                    budget_exhausted: true,
                };
            }
            attempted.insert((request.anchor, request.identifier.clone()));
            if let Err(error) = self.load_asset(&request.identifier, Some(request.anchor)) {
                match error.kind {
                    IoErrorKind::NotFound => {
                        self.store
                            .mark_asset_missing(request.anchor, &request.identifier);
                        self.synchronize();
                    }
                    // Not supplied yet: it stays pending, and the host
                    // supplies it under the identifier storage named.
                    IoErrorKind::Unavailable => {
                        let identifier = error
                            .identifier
                            .unwrap_or_else(|| request.identifier.to_string());
                        if !unavailable
                            .iter()
                            .any(|asset: &UnavailableClipAsset| asset.identifier == identifier)
                        {
                            unavailable.push(UnavailableClipAsset {
                                request,
                                identifier,
                            });
                        }
                    }
                    _ => failures.push(ClipLoadFailure { request, error }),
                }
            }
        }
    }
}
