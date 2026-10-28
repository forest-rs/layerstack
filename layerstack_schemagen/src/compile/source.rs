// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The schema stage: `schema.usda` composed with the layers it sublayers,
//! resolved as usdGenSchema's `InitializeResolver` resolves them.

use std::path::{Path, PathBuf};

use layerstack::{CompositionError, InMemoryStore, LayerId, Stage};
use layerstack_io::{Filesystem, ImportPolicy, LoadOptions, OpenOptions, StageDocument};

use super::CompileError;

/// A composed schema stage, with the store holding its layers.
pub(crate) struct SchemaStage {
    document: StageDocument<Filesystem>,
    /// The schema file's layer.
    pub(crate) root: LayerId,
}

impl SchemaStage {
    /// Opens `path` with every layer it sublayers. A sublayer's asset path
    /// is looked up next to the layer naming it, then in each of `search`,
    /// as `ArDefaultResolver` looks up a search path with the search paths
    /// usdGenSchema sets.
    ///
    /// Spec: AOUSD Core §9 (asset resolution), §10.3.1 (sublayers).
    pub(crate) fn open(path: &Path, search: &[PathBuf]) -> Result<Self, CompileError> {
        let shown = path.display().to_string();
        let read = |message: String| CompileError::Read {
            layer: shown.clone(),
            message,
        };
        let path = std::fs::canonicalize(path).map_err(|e| read(e.to_string()))?;
        let directory = path.parent().unwrap_or(Path::new("/"));
        let storage =
            Filesystem::new(directory, search.iter().cloned()).map_err(|e| read(e.to_string()))?;
        let asset = path
            .to_str()
            .ok_or_else(|| read("not a UTF-8 path".into()))?;
        // Recovery keeps going past a missing sublayer, so the error can
        // name it; parse errors are still errors below.
        let document = StageDocument::open(
            storage,
            asset,
            OpenOptions {
                loading: LoadOptions {
                    import_policy: ImportPolicy::AllowRecovery,
                    ..LoadOptions::default()
                },
                ..OpenOptions::default()
            },
        )
        .map_err(|e| read(e.to_string()))?;
        let root = document
            .stage()
            .stage()
            .root_layer()
            .ok_or_else(|| read("no root layer".into()))?;
        let stage = Self { document, root };
        stage.check_loaded(&shown)?;
        Ok(stage)
    }

    /// Fails on what the load could not read: a sublayer that resolved to
    /// nothing, or a layer with syntax errors.
    fn check_loaded(&self, shown: &str) -> Result<(), CompileError> {
        let report = self.document.load_report();
        if let Some(failure) = report.asset_failures.first() {
            return Err(CompileError::UnresolvedSublayer {
                layer: failure
                    .anchor
                    .map_or_else(|| shown.to_owned(), |layer| self.identifier(layer)),
                asset: failure.asset_path.clone(),
            });
        }
        for error in self.stage().composition_errors() {
            if let CompositionError::UnresolvedSublayer(sublayer) = error {
                return Err(CompileError::UnresolvedSublayer {
                    layer: self.identifier(sublayer.layer),
                    asset: sublayer.asset_path.clone(),
                });
            }
        }
        if let Some(diagnostic) = report.diagnostics.iter().find(|d| d.diagnostic.is_error()) {
            return Err(CompileError::Read {
                layer: self.identifier(diagnostic.layer_id),
                message: format!("{:?}", diagnostic.diagnostic),
            });
        }
        Ok(())
    }

    /// The composed stage.
    pub(crate) fn stage(&self) -> &Stage {
        self.document.stage().stage()
    }

    /// The composed stage bound to its store.
    pub(crate) fn view(&self) -> layerstack::StageView<'_> {
        self.document.view()
    }

    /// The store holding the stage's layers.
    pub(crate) fn store(&self) -> &InMemoryStore {
        self.document.store()
    }

    /// The store and the stage, for flattening into the store.
    pub(crate) fn parts_mut(&mut self) -> (&mut InMemoryStore, &Stage) {
        let (store, live) = self.document.parts_mut();
        (store, live.stage())
    }

    /// The layer stack, strongest first.
    pub(crate) fn layer_stack(&self) -> Vec<LayerId> {
        self.stage().layer_stack().to_vec()
    }

    /// Where `layer` was read from.
    pub(crate) fn identifier(&self, layer: LayerId) -> String {
        self.document
            .identifier(layer)
            .map_or_else(|| format!("{layer:?}"), str::to_owned)
    }
}
