// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Writes the generated record registry as USDA and USDC.
//!
//! ```sh
//! cargo run -p layerstack_conformance --example write_registry_scene -- <dir> [full|small]
//! ```
//!
//! Prints the scene's counts, then one `<format> <bytes> <path>` line per
//! format. See `layerstack_conformance::registry_scene`.

use layerstack_conformance::library_scene::LibraryFormat;
use layerstack_conformance::registry_scene::{RegistryScene, RegistryShape};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .unwrap_or_else(|| "target/registry-scene".into());
    let shape = match args.next().as_deref() {
        Some("small") => RegistryShape::SMALL,
        _ => RegistryShape::FULL,
    };
    let scene = RegistryScene::generate(&shape);
    println!("{}", scene.summary());
    for format in [LibraryFormat::Usda, LibraryFormat::Usdc] {
        let dir = std::path::Path::new(&dir).join(format.extension());
        let (path, bytes) = scene.write(&dir, format)?;
        println!("{} {bytes} {}", format.extension(), path.display());
    }
    Ok(())
}
