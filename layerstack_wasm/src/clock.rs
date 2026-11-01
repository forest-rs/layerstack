// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The clock a stage's loads are timed with
//! ([`layerstack_io::LoadOptions::clock`]): `performance.now()` in a
//! JavaScript host, so the times a load reports sit on the page's own
//! timeline; `std::time::Instant` natively with `std`; none otherwise.

use layerstack_io::LoadClock;

/// The clock loads are timed with on this target, if it has one.
#[must_use]
pub(crate) fn load_clock() -> Option<LoadClock> {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    return Some(LoadClock::new(performance::Performance));
    #[cfg(all(
        not(all(target_arch = "wasm32", target_os = "unknown")),
        feature = "std"
    ))]
    return Some(LoadClock::new(native::Native(std::time::Instant::now())));
    #[cfg(all(
        not(all(target_arch = "wasm32", target_os = "unknown")),
        not(feature = "std")
    ))]
    return None;
}

/// Milliseconds on the clock's timeline, as JavaScript gives times.
#[must_use]
pub(crate) fn millis(reading: core::time::Duration) -> f64 {
    reading.as_secs_f64() * 1000.0
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod performance {
    use core::time::Duration;
    use layerstack_io::Clock;
    use wasm_bindgen::prelude::wasm_bindgen;

    #[wasm_bindgen]
    extern "C" {
        /// `performance.now()`: milliseconds since the page's time origin,
        /// in browsers, workers and Node alike.
        #[wasm_bindgen(js_namespace = performance, js_name = now)]
        fn performance_now() -> f64;
    }

    /// The host's `performance.now()`.
    pub(super) struct Performance;

    impl Clock for Performance {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "whole nanoseconds; `as` saturates, and maps a reading that is not a number to zero"
        )]
        fn now(&self) -> Duration {
            Duration::from_nanos((performance_now() * 1_000_000.0) as u64)
        }
    }
}

#[cfg(all(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    feature = "std"
))]
mod native {
    use core::time::Duration;
    use layerstack_io::Clock;

    /// Time since the stage opened.
    pub(super) struct Native(pub(super) std::time::Instant);

    impl Clock for Native {
        fn now(&self) -> Duration {
            self.0.elapsed()
        }
    }
}
