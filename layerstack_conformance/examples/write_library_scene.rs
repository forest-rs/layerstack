// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Writes the generated parts library as USDA and USDC.
//!
//! ```sh
//! cargo run -p layerstack_conformance --example write_library_scene -- <dir> [full|small]
//! ```
//!
//! Prints the scene's counts, then one `<format> <bytes> <root>` line per
//! format. See `layerstack_conformance::library_scene`.

use layerstack_conformance::library_scene::{LibraryFormat, LibraryScene, LibraryShape};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "target/library-scene".into());
    let shape = match args.next().as_deref() {
        Some("small") => LibraryShape::SMALL,
        _ => LibraryShape::FULL,
    };
    let scene = LibraryScene::generate(&shape);
    println!("{}", scene.summary());
    for format in [LibraryFormat::Usda, LibraryFormat::Usdc] {
        let dir = std::path::Path::new(&dir).join(format.extension());
        let written = scene.write(&dir, format)?;
        println!(
            "{} {} {}",
            format.extension(),
            written.bytes(),
            written.root.display()
        );
    }
    Ok(())
}
