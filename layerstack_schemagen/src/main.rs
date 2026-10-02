// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Command-line entry point for schema and shader-library generation.
use std::process::ExitCode;

fn main() -> ExitCode {
    match layerstack_schemagen::run() {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("layerstack_schemagen: {message}");
            ExitCode::FAILURE
        }
    }
}
