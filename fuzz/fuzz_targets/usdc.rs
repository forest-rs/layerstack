// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fuzz target: [`layerstack_fuzz::usdc`].

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| layerstack_fuzz::usdc(data));
