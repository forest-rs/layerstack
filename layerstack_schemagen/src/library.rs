// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Loading local shader-definition libraries with ordinary USD composition.
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset,
    TokenInterner,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Default)]
struct Files {
    paths: Vec<String>,
    ids: BTreeMap<PathBuf, LayerId>,
}
impl AssetResolver for Files {
    fn resolve(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let path = anchor.map_or_else(
            || PathBuf::from(asset),
            |id| {
                Path::new(&self.paths[usize::try_from(id.0).expect("allocated layer index")])
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(asset)
            },
        );
        let path = path
            .canonicalize()
            .map_err(|_| AssetResolveError::NotFound)?;
        let id = if let Some(id) = self.ids.get(&path) {
            *id
        } else {
            let id = LayerId(self.paths.len() as u64);
            self.paths
                .push(path.to_str().ok_or(AssetResolveError::NotFound)?.to_owned());
            self.ids.insert(path, id);
            id
        };
        Ok(ResolvedAsset {
            layer_id: id,
            resolved_path: Arc::from(
                self.paths[usize::try_from(id.0).expect("allocated layer index")].as_str(),
            ),
            layer: None,
        })
    }
    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.paths
            .get(usize::try_from(id.0).ok()?)
            .map(String::as_str)
    }
}

pub(crate) fn load(path: &Path, store: &mut InMemoryStore) -> Result<LayerId, String> {
    let mut resolver = Files::default();
    let root = resolver
        .resolve(
            path.to_str().ok_or("non-UTF8 definition path")?,
            None,
            &mut store.tokens,
            &mut store.paths,
        )
        .map_err(|e| format!("{}: {e:?}", path.display()))?
        .layer_id;
    let mut cursor = 0;
    while cursor < resolver.paths.len() {
        let file = resolver.paths[cursor].clone();
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
        let parsed = layerstack_usda::parser::parse(&text);
        let emitted = layerstack_usda::emit::emit(
            &parsed.layer,
            LayerId(cursor as u64),
            &mut store.tokens,
            &mut store.paths,
            &mut resolver,
        );
        if !parsed.diagnostics.is_empty() || !emitted.diagnostics.is_empty() {
            return Err(format!(
                "{file}: {:?} {:?}",
                parsed.diagnostics, emitted.diagnostics
            ));
        }
        store.insert_layer(emitted.layer);
        for layer in emitted.resolved_layers {
            store.insert_layer(layer);
        }
        cursor += 1;
    }
    Ok(root)
}
