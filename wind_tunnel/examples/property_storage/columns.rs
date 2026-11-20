// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use hashbrown::HashMap;
use layerstack::property::{PropertyEntry, PropertyKind, PropertySpec, Variability};
use layerstack::{PathId, PropertyPath, PropertyType, TokenId, Value};
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Clone, PartialEq)]
struct Decl {
    name: TokenId,
    kind: PropertyKind,
    custom: bool,
    variability: Variability,
    ty: Option<PropertyType>,
}
impl Eq for Decl {}
impl Hash for Decl {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.name.hash(h);
        self.kind.hash(h);
        self.custom.hash(h);
        self.variability.hash(h);
        self.ty.as_ref().map(PropertyType::type_name).hash(h);
    }
}
type Column = Arc<Vec<Option<Value>>>;

#[derive(Clone)]
struct Table {
    decls: Arc<[Decl]>,
    blocks: Vec<Vec<Column>>,
    rows: usize,
}

#[derive(Clone)]
pub(super) struct Columns {
    shapes: HashMap<Arc<[Decl]>, usize>,
    tables: Vec<Table>,
    rows: HashMap<PathId, (usize, usize)>,
    rare: HashMap<PropertyPath, Arc<PropertySpec>>,
    scratch: Vec<Decl>,
    block_size: usize,
}

impl Columns {
    pub(super) fn new(block_size: usize) -> Self {
        Self {
            shapes: HashMap::new(),
            tables: vec![],
            rows: HashMap::new(),
            rare: HashMap::new(),
            scratch: vec![],
            block_size,
        }
    }
    pub(super) fn finish(&mut self) {
        self.shapes = HashMap::new();
        self.scratch = Vec::new();
    }
    pub(super) fn insert(&mut self, path: PathId, entries: Vec<PropertyEntry>) {
        if entries.is_empty() {
            return;
        }
        self.scratch.clear();
        self.scratch.extend(entries.iter().map(|e| Decl {
            name: e.name,
            kind: e.spec.kind,
            custom: e.spec.custom,
            variability: e.spec.variability,
            ty: e.spec.type_name.clone(),
        }));
        let id = if let Some(id) = self.shapes.get(self.scratch.as_slice()) {
            *id
        } else {
            let decls: Arc<[Decl]> = self.scratch.as_slice().into();
            let id = self.tables.len();
            self.shapes.insert(decls.clone(), id);
            self.tables.push(Table {
                decls,
                blocks: vec![],
                rows: 0,
            });
            id
        };
        let table = &mut self.tables[id];
        let row = table.rows;
        if row.is_multiple_of(self.block_size) {
            table
                .blocks
                .push((0..entries.len()).map(|_| Arc::new(Vec::new())).collect());
        }
        let block = table.blocks.last_mut().unwrap();
        for (entry, column) in entries.into_iter().zip(block) {
            let mut spec = Arc::unwrap_or_clone(entry.spec);
            Arc::get_mut(column).unwrap().push(spec.default.take());
            if spec.time_samples.is_authored()
                || spec.spline.is_some()
                || !spec.metadata.is_empty()
                || spec.targets.is_some()
            {
                self.rare
                    .insert(PropertyPath::new(path, entry.name), Arc::new(spec));
            }
        }
        table.rows += 1;
        self.rows.insert(path, (id, row));
    }
    pub(super) fn row(
        &self,
        path: PathId,
    ) -> impl Iterator<Item = layerstack_usda::save::PropertyView<'_>> {
        self.rows
            .get(&path)
            .into_iter()
            .flat_map(move |&(shape, row)| {
                let table = &self.tables[shape];
                let values = &table.blocks[row / self.block_size];
                table.decls.iter().zip(values).map(move |(decl, col)| {
                    let extra = self
                        .rare
                        .get(&PropertyPath::new(path, decl.name))
                        .map(AsRef::as_ref);
                    layerstack_usda::save::PropertyView {
                        name: decl.name,
                        kind: decl.kind,
                        custom: decl.custom,
                        variability: decl.variability,
                        type_name: decl.ty.as_ref(),
                        default: col[row % self.block_size].as_ref(),
                        targets: extra
                            .and_then(|s: &PropertySpec| s.targets.as_deref())
                            .map(layerstack_usda::save::Targets::Native),
                        extra,
                    }
                })
            })
    }
    fn cell(&self, key: PropertyPath) -> Option<(usize, usize, usize)> {
        let &(shape, row) = self.rows.get(&key.prim_path())?;
        let slot = self.tables[shape]
            .decls
            .iter()
            .position(|d| d.name == key.property())?;
        Some((shape, row, slot))
    }
    pub(super) fn get(
        &self,
        key: PropertyPath,
    ) -> Option<(PropertyKind, Option<&Value>, Option<&PropertySpec>)> {
        let (shape, row, slot) = self.cell(key)?;
        let t = &self.tables[shape];
        Some((
            t.decls[slot].kind,
            t.blocks[row / self.block_size][slot][row % self.block_size].as_ref(),
            self.rare.get(&key).map(AsRef::as_ref),
        ))
    }
    pub(super) fn set_default(&mut self, key: PropertyPath, value: Value) {
        let (shape, row, slot) = self.cell(key).unwrap();
        Arc::make_mut(&mut self.tables[shape].blocks[row / self.block_size][slot])
            [row % self.block_size] = Some(value);
    }
    pub(super) fn set_targets(
        &mut self,
        key: PropertyPath,
        targets: Box<layerstack::ListOp<layerstack::TargetPath>>,
    ) {
        let slot = self
            .rare
            .entry(key)
            .or_insert_with(|| Arc::new(PropertySpec::relationship()));
        Arc::make_mut(slot).targets = Some(targets);
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = PropertyPath> + '_ {
        self.rows.iter().flat_map(|(&path, &(shape, _))| {
            self.tables[shape]
                .decls
                .iter()
                .map(move |d| PropertyPath::new(path, d.name))
        })
    }
    pub(super) fn restore(&self, layer: &mut layerstack::Layer) {
        let prims = layer.prims_mut();
        for (&path, &(shape, row)) in &self.rows {
            let t = &self.tables[shape];
            let prim = prims.get_mut(&path).unwrap();
            prim.properties = t
                .decls
                .iter()
                .enumerate()
                .map(|(slot, d)| {
                    let key = PropertyPath::new(path, d.name);
                    let mut spec = self
                        .rare
                        .get(&key)
                        .map(|v| (**v).clone())
                        .unwrap_or_default();
                    spec.kind = d.kind;
                    spec.custom = d.custom;
                    spec.variability = d.variability;
                    spec.type_name = d.ty.clone();
                    spec.default =
                        t.blocks[row / self.block_size][slot][row % self.block_size].clone();
                    PropertyEntry {
                        name: d.name,
                        spec: Arc::new(spec),
                    }
                })
                .collect();
        }
    }
}
