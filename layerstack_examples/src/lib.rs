// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Downstream generated shader interfaces compiled without the standard prelude.
//! Matrix inputs/outputs and array ports exercise allocation during authoring,
//! while runnable examples remain separate binaries in this crate.
#![no_std]
include!(concat!(env!("OUT_DIR"), "/shader_modules.rs"));
