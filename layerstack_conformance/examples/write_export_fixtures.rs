// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Writes the exporter fixtures for external validation.
//!
//! ```sh
//! cargo run -p layerstack_conformance --example write_export_fixtures -- <dir>
//! ```
//!
//! Prints one `<valid|valid-arkit|invalid:VALIDATOR> <path>` line per
//! fixture; see
//! `layerstack_conformance/scripts/export_interop.sh`.

use layerstack_conformance::export_fixtures::{Expect, write_all};

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/export-interop".into());
    for fixture in write_all(std::path::Path::new(&dir)) {
        match fixture.expect {
            Expect::Valid => println!("valid {}", fixture.path.display()),
            Expect::ValidArkit => println!("valid-arkit {}", fixture.path.display()),
            Expect::Invalid(validator) => {
                println!("invalid:{validator} {}", fixture.path.display());
            }
        }
    }
}
