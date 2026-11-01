// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where a load's work and time went: each layer it read, with the bytes
//! it decoded and the prim specs it made, and each composition after a
//! batch of reads ([`crate::LoadReport::reads`],
//! [`crate::LoadReport::compositions`]).
//!
//! Work units are always counted. Time needs a clock, and this crate has
//! none: the host supplies one ([`crate::LoadOptions::clock`]), such as
//! `std::time::Instant` natively or `performance.now()` in a browser.
//! Without one every `time` is `None`, never zero. Readings stay on the
//! host's clock, so a host can place them beside its own marks.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::time::Duration;

use layerstack::{Layer, LayerId, Path, PathInterner};

/// A monotonic clock the host supplies to time loads
/// ([`crate::LoadOptions::clock`]).
pub trait Clock: Send + Sync {
    /// The time now, from the clock's own fixed origin: never earlier than
    /// a reading before it.
    fn now(&self) -> Duration;
}

/// The clock loads are timed with, shared by the document and its loads.
///
/// Two are equal when they share one [`Clock`].
#[derive(Clone)]
pub struct LoadClock(Arc<dyn Clock>);

impl LoadClock {
    /// Times loads with `clock`.
    pub fn new(clock: impl Clock + 'static) -> Self {
        Self(Arc::new(clock))
    }

    /// The clock's reading now.
    #[must_use]
    pub fn now(&self) -> Duration {
        self.0.now()
    }
}

impl core::fmt::Debug for LoadClock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("LoadClock")
    }
}

impl PartialEq for LoadClock {
    /// The same clock, by identity.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for LoadClock {}

/// From one clock reading to a later one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSpan {
    /// The clock's reading when it began.
    pub start: Duration,
    /// The clock's reading when it ended.
    pub end: Duration,
}

impl TimeSpan {
    /// How long it took.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }
}

/// One layer a load read: decoding the bytes storage gave for it into
/// specs. Fetching them ([`crate::Storage::read_encoded`]) is not part of
/// it.
///
/// A read resolves the layer's arcs as it goes, and reads a dependency
/// storage already holds before it returns, so reads nest: `time.span`
/// covers the nested reads, `time.own` does not. The `own` times of a
/// load's reads add up to the time it spent reading.
///
/// A USDZ package's first read covers the members it reads with its root
/// layer: its bytes are the archive's, its prims the root layer's. A member
/// read later, through an arc into the package, is a read of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayerRead {
    /// The layer read; a read that failed names a layer the load did not
    /// keep (not among [`crate::LoadReport::layers`]).
    pub layer: LayerId,
    /// The encoded bytes decoded.
    pub bytes: u64,
    /// The prim specs the layer holds outside variant branches
    /// ([`layerstack::Layer::prims`]), not counting the pseudo-root's; 0
    /// for a read that failed.
    pub prims: u64,
    /// When it ran; `None` without a clock.
    pub time: Option<ReadTime>,
}

/// When a [`LayerRead`] ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadTime {
    /// From the read's start to its end, nested reads included.
    pub span: TimeSpan,
    /// The span less the reads nested in it: this layer's own decoding.
    pub own: Duration,
}

/// One composition a load ran after a batch of reads: the layers read
/// entering the store, and the stage recomposing the prims that use them.
/// A batch whose assets were all still unavailable composes too, adding
/// none.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Composition {
    /// The layers the batch added.
    pub layers: u64,
    /// When it ran; `None` without a clock.
    pub time: Option<TimeSpan>,
}

/// The prim specs `layer` holds outside variant branches, not counting
/// the pseudo-root's ([`LayerRead::prims`]).
pub(crate) fn prim_count(layer: &Layer, paths: &PathInterner) -> usize {
    let prims = layer.prims();
    let root = paths
        .lookup(&Path::root())
        .is_some_and(|root| prims.contains_key(&root));
    prims.len() - usize::from(root)
}

/// Counts a reader's work and, with a clock, times it: the reads in the
/// order they began, with the time nested reads took off each one's own.
#[derive(Debug, Default)]
pub(crate) struct ReadLog {
    clock: Option<LoadClock>,
    reads: Vec<LayerRead>,
    /// For each read under way, outermost first: the time the reads
    /// nested in it took so far.
    nested: Vec<Duration>,
}

/// A read [`ReadLog::begin`] started: its place in the log and, with a
/// clock, its start.
pub(crate) struct ReadStarted {
    at: usize,
    start: Option<Duration>,
}

impl ReadLog {
    /// A log timing reads with `clock`, if any.
    pub(crate) fn new(clock: Option<LoadClock>) -> Self {
        Self {
            clock,
            ..Self::default()
        }
    }

    /// Starts the read of `layer` from `bytes` encoded bytes.
    pub(crate) fn begin(&mut self, layer: LayerId, bytes: usize) -> ReadStarted {
        let at = self.reads.len();
        self.reads.push(LayerRead {
            layer,
            bytes: bytes as u64,
            prims: 0,
            time: None,
        });
        let start = self.clock.as_ref().map(|clock| {
            self.nested.push(Duration::ZERO);
            clock.now()
        });
        ReadStarted { at, start }
    }

    /// Ends `read`, which made `prims` prim specs.
    pub(crate) fn end(&mut self, read: ReadStarted, prims: usize) {
        let entry = &mut self.reads[read.at];
        entry.prims = prims as u64;
        let (Some(start), Some(clock)) = (read.start, &self.clock) else {
            return;
        };
        let end = clock.now();
        let nested = self.nested.pop().unwrap_or_default();
        let span = TimeSpan { start, end };
        entry.time = Some(ReadTime {
            span,
            own: span.duration().saturating_sub(nested),
        });
        if let Some(outer) = self.nested.last_mut() {
            *outer += span.duration();
        }
    }

    /// The clock reads are timed with, if any.
    #[cfg(feature = "std")]
    pub(crate) fn clock(&self) -> Option<&LoadClock> {
        self.clock.as_ref()
    }

    /// Logs a read made elsewhere, such as on another thread.
    #[cfg(feature = "std")]
    pub(crate) fn push(&mut self, read: LayerRead) {
        self.reads.push(read);
    }

    /// The reads logged, in the order they began.
    pub(crate) fn into_reads(self) -> Vec<LayerRead> {
        self.reads
    }
}

/// The clock's reading as a composition starts, with `clock`.
pub(crate) fn composition_start(clock: Option<&LoadClock>) -> Option<Duration> {
    clock.map(LoadClock::now)
}

/// The composition that started at `start` ([`composition_start`]) and
/// ends now, having added `layers` layers.
pub(crate) fn composition(
    clock: Option<&LoadClock>,
    start: Option<Duration>,
    layers: usize,
) -> Composition {
    Composition {
        layers: layers as u64,
        time: start.zip(clock).map(|(start, clock)| TimeSpan {
            start,
            end: clock.now(),
        }),
    }
}
