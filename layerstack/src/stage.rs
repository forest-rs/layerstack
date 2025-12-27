//! Stage facade and value resolution.
//!
//! Spec: AOUSD Core §11–§12 (stage population and value resolution).

use alloc::{vec, vec::Vec};

use hashbrown::HashMap;

use crate::{
    doc::{FieldValue, LayerId, LayerStore, Value},
    interner::TokenId,
    listop::{ListOp, resolve_list_chain},
    path::PathId,
    prim_index::{Opinion, PrimIndex},
};

/// Provenance information for resolved values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// The layer whose opinion was strongest.
    pub layer: LayerId,
    /// The spec path in that layer.
    pub spec_path: PathId,
    /// The field that was resolved.
    pub field: TokenId,
}

/// A resolved value (optionally with provenance).
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved<T> {
    /// The resolved value.
    pub value: T,
    /// Optional provenance for inspectors.
    pub provenance: Option<Provenance>,
}

/// A resolved field value.
///
/// Spec: AOUSD Core §12 (value resolution), including §12.4 for `ListOps`.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedValue {
    /// A scalar value (strongest wins).
    Scalar(Value),
    /// A token list value resolved by chaining `ListOps`.
    TokenList(Vec<TokenId>),
    /// A path list value resolved by chaining `ListOps`.
    PathList(Vec<PathId>),
}

/// Controls partial population.
#[derive(Clone, Debug, Default)]
pub struct PopulationMask {
    /// Include these prim paths (and their ancestors).
    pub include: Vec<PathId>,
}

/// Options for stage composition and population.
#[derive(Clone, Debug, Default)]
pub struct StageOptions {
    /// Optional population mask.
    pub mask: Option<PopulationMask>,
    /// Whether resolution APIs return provenance.
    pub with_provenance: bool,
}

/// A composed stage: read-only facade over composition results.
#[derive(Debug)]
pub struct Stage {
    prims: HashMap<PathId, PrimIndex>,
    children: HashMap<PathId, Vec<PathId>>,
    with_provenance: bool,
}

impl Stage {
    /// Composes a stage from a root layer.
    pub fn compose(store: &mut dyn LayerStore, root: LayerId, options: StageOptions) -> Self {
        crate::compose::compose_stage(store, root, options)
    }

    pub(crate) fn from_parts(
        prims: HashMap<PathId, PrimIndex>,
        children: HashMap<PathId, Vec<PathId>>,
        with_provenance: bool,
    ) -> Self {
        Self {
            prims,
            children,
            with_provenance,
        }
    }

    /// Resolves a field on a prim.
    ///
    /// Note: this returns only scalar (`Value`) fields. For `ListOp` fields, use
    /// [`Stage::resolve_token_list`].
    #[must_use]
    pub fn resolve_field(&self, prim: PathId, field: TokenId) -> Option<Resolved<Value>> {
        let resolved = self.resolve_value(prim, field)?;
        match resolved.value {
            ResolvedValue::Scalar(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::TokenList(_) | ResolvedValue::PathList(_) => None,
        }
    }

    /// Resolves a token `ListOp` field on a prim.
    #[must_use]
    pub fn resolve_token_list(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<Resolved<Vec<TokenId>>> {
        let resolved = self.resolve_value(prim, field)?;
        match resolved.value {
            ResolvedValue::TokenList(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::Scalar(_) | ResolvedValue::PathList(_) => None,
        }
    }

    /// Resolves a path `ListOp` field on a prim.
    #[must_use]
    pub fn resolve_path_list(&self, prim: PathId, field: TokenId) -> Option<Resolved<Vec<PathId>>> {
        let resolved = self.resolve_value(prim, field)?;
        match resolved.value {
            ResolvedValue::PathList(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::Scalar(_) | ResolvedValue::TokenList(_) => None,
        }
    }

    /// Resolves a field on a prim.
    ///
    /// - Scalar fields return the strongest scalar opinion.
    /// - Token list fields chain `ListOps` across all contributing opinions.
    ///
    /// Spec: AOUSD Core §12 (value resolution).
    #[must_use]
    pub fn resolve_value(&self, prim: PathId, field: TokenId) -> Option<Resolved<ResolvedValue>> {
        let index = self.prims.get(&prim)?;
        let opinions = index.opinions_by_field.get(&field)?;
        let strongest = opinions.first()?;

        match &strongest.value {
            FieldValue::Value(v) => Some(Resolved {
                value: ResolvedValue::Scalar(v.clone()),
                provenance: self.provenance_for(field, strongest),
            }),
            FieldValue::TokenListOp(_) => {
                let ops: Vec<ListOp<TokenId>> = opinions
                    .iter()
                    .filter_map(|op| match &op.value {
                        FieldValue::TokenListOp(list) => Some(list.clone()),
                        FieldValue::Value(_) | FieldValue::PathListOp(_) => None,
                    })
                    .collect();
                Some(Resolved {
                    value: ResolvedValue::TokenList(resolve_list_chain::<TokenId>(&[], ops)),
                    provenance: self.provenance_for(field, strongest),
                })
            }
            FieldValue::PathListOp(_) => {
                let ops: Vec<ListOp<PathId>> = opinions
                    .iter()
                    .filter_map(|op| match &op.value {
                        FieldValue::PathListOp(list) => Some(list.clone()),
                        FieldValue::Value(_) | FieldValue::TokenListOp(_) => None,
                    })
                    .collect();
                Some(Resolved {
                    value: ResolvedValue::PathList(resolve_list_chain::<PathId>(&[], ops)),
                    provenance: self.provenance_for(field, strongest),
                })
            }
        }
    }

    /// Returns the sorted opinion stack for `(prim, field)` (strongest-first).
    ///
    /// This is intended for inspection/debugging and mirrors the "stack of
    /// opinions" described by the spec.
    ///
    /// Spec: AOUSD Core §12 (value resolution) and §10.4 (strength ordering).
    #[must_use]
    pub fn explain_field(&self, prim: PathId, field: TokenId) -> Option<&[Opinion]> {
        let index = self.prims.get(&prim)?;
        let opinions = index.opinions_by_field.get(&field)?;
        Some(opinions.as_slice())
    }

    /// Traverses prims in a deterministic preorder.
    pub fn traverse(&self, root: PathId) -> Traverse<'_> {
        Traverse::new(self, root)
    }

    /// Returns the direct children of `prim` in deterministic order.
    ///
    /// This is an inspection API intended for conformance and debugging.
    ///
    /// Spec: AOUSD Core §11 (stage population) requires deterministic traversal.
    #[must_use]
    pub fn children_of(&self, prim: PathId) -> Option<&[PathId]> {
        self.children.get(&prim).map(|v| v.as_slice())
    }

    /// Returns the composed prim stack as `(layer_id, spec_path)` pairs (strongest-first).
    ///
    /// This is an inspection API intended for conformance and debugging.
    ///
    /// Spec: AOUSD Core §11 (stage population) and §10.4 (strength ordering).
    #[must_use]
    pub fn prim_stack(&self, prim: PathId) -> Option<Vec<(LayerId, PathId)>> {
        use hashbrown::HashSet;

        let index = self.prims.get(&prim)?;
        let mut out = Vec::new();
        let mut seen_pairs = HashSet::<(LayerId, PathId)>::new();
        for key in &index.sources {
            let pair = (key.layer_id, key.spec_path);
            if seen_pairs.insert(pair) {
                out.push(pair);
            }
        }
        Some(out)
    }

    /// Returns `true` if the stage contains a prim at `path`.
    #[must_use]
    pub fn has_prim(&self, path: PathId) -> bool {
        self.prims.contains_key(&path)
    }

    fn provenance_for(&self, field: TokenId, strongest: &Opinion) -> Option<Provenance> {
        self.with_provenance.then_some(Provenance {
            layer: strongest.key.layer_id,
            spec_path: strongest.key.spec_path,
            field,
        })
    }
}

/// An iterator for deterministic stage traversal.
#[derive(Debug)]
pub struct Traverse<'a> {
    stage: &'a Stage,
    stack: Vec<PathId>,
}

impl<'a> Traverse<'a> {
    fn new(stage: &'a Stage, root: PathId) -> Self {
        Self {
            stage,
            stack: vec![root],
        }
    }
}

impl Iterator for Traverse<'_> {
    type Item = PathId;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.stack.pop()?;
        if let Some(children) = self.stage.children.get(&next) {
            for child in children.iter().rev() {
                self.stack.push(*child);
            }
        }
        Some(next)
    }
}
