// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reading a USDC layer on several threads reads the layer one does.
//!
//! The small parts library (with `tags` long enough to be retained) and the
//! small record registry are written as USDC and each layer read, eagerly
//! and retaining its arrays, on 1, 2 and 8 threads. Each read interns into
//! interners of its own, in an order the threads decide, so the layers are
//! compared by what they say: saved as USDA text, which must be the same
//! byte for byte, with the same diagnostics.

use std::path::Path;
use std::sync::Arc;

use layerstack::parallel::Threads;
use layerstack::{
    AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, TokenInterner,
};
use layerstack_conformance::library_scene::{LibraryFormat, LibraryScene, LibraryShape};
use layerstack_conformance::registry_scene::{RegistryScene, RegistryShape};
use layerstack_usdc::DecodeBudget;

/// A resolver that resolves nothing.
struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// Every USDC layer of the small library and registry, by file name.
fn layers() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("threaded_usdc_read");
    let shape = LibraryShape {
        average_tags: 40,
        ..LibraryShape::SMALL
    };
    let library = LibraryScene::generate(&shape)
        .write(&dir.join("library"), LibraryFormat::Usdc)
        .expect("the library writes");
    let (registry, _) = RegistryScene::generate(&RegistryShape::SMALL)
        .write(&dir.join("registry"), LibraryFormat::Usdc)
        .expect("the registry writes");
    let read = |path: &Path| {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        (name, std::fs::read(path).expect("a written layer"))
    };
    let mut layers: Vec<_> = library.files.iter().map(|(path, _)| read(path)).collect();
    layers.push(read(&registry));
    layers
}

/// `bytes` read on `threads` threads, retaining arrays or not, saved as
/// USDA text, with its diagnostics.
fn render(bytes: &[u8], threads: usize, retained: bool) -> (String, Vec<String>) {
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let budget = DecodeBudget::for_input(bytes.len());
    let result = if retained {
        let bytes: Arc<[u8]> = bytes.into();
        let read = layerstack_usdc::read_usdc_lazy_within(
            bytes,
            LayerId(1),
            &tokens,
            &paths,
            &mut NoAssets,
            budget,
            &Threads::new(threads),
        );
        read.expect("the layer reads").assembled
    } else {
        let mut budget = budget;
        let read = layerstack_usdc::read_usdc_within(
            bytes,
            LayerId(1),
            &tokens,
            &paths,
            &mut NoAssets,
            &mut budget,
            &Threads::new(threads),
        );
        read.expect("the layer reads")
    };
    let text = layerstack_usda::save_usda(&result.layer, &tokens, &paths).expect("the layer saves");
    let diagnostics = result
        .diagnostics
        .iter()
        .map(|d| format!("{d:?}"))
        .collect();
    (text, diagnostics)
}

#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
fn layers_read_on_any_number_of_threads_alike() {
    for (name, bytes) in layers() {
        for retained in [false, true] {
            let one = render(&bytes, 1, retained);
            assert!(one.0.len() > 100, "{name} saves its prims");
            for threads in [2, 8] {
                let many = render(&bytes, threads, retained);
                assert!(
                    many == one,
                    "{name}, {threads} threads, retained {retained}"
                );
            }
        }
    }
}
