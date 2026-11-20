// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use hashbrown::HashMap;
use layerstack::property::{PropertyEntry, PropertySpec};
use layerstack::{Layer, PathId, PropertyPath, TokenId};
use std::sync::Arc;

const ROWS: usize = 64;
type Block = Arc<Vec<Arc<PropertySpec>>>;

#[derive(Clone)]
struct Group {
    names: Arc<[TokenId]>,
    blocks: Vec<Block>,
    rows: usize,
}

/// Share an ordered layout while retaining the reader's original property handles.
#[derive(Clone, Default)]
pub(super) struct Handles {
    layouts: HashMap<Arc<[TokenId]>, usize>,
    groups: Vec<Group>,
    rows: HashMap<PathId, (usize, usize)>,
    scratch: Vec<TokenId>,
}

impl Handles {
    pub(super) fn insert(&mut self, path: PathId, entries: Vec<PropertyEntry>) {
        if entries.is_empty() {
            return;
        }
        self.scratch.clear();
        self.scratch.extend(entries.iter().map(|e| e.name));
        let group = if let Some(&group) = self.layouts.get(self.scratch.as_slice()) {
            group
        } else {
            let group = self.groups.len();
            let names: Arc<[TokenId]> = Arc::from(self.scratch.as_slice());
            self.layouts.insert(Arc::clone(&names), group);
            self.groups.push(Group {
                names,
                blocks: Vec::new(),
                rows: 0,
            });
            group
        };
        let records = &mut self.groups[group];
        let row = records.rows;
        if row.is_multiple_of(ROWS) {
            records.blocks.push(Arc::default());
        }
        let block = Arc::get_mut(records.blocks.last_mut().unwrap()).unwrap();
        block.extend(entries.into_iter().map(|e| e.spec));
        self.rows.insert(path, (group, row));
        records.rows += 1;
    }
    pub(super) fn finish(&mut self) {
        self.layouts = HashMap::new();
        self.scratch = Vec::new();
    }
    fn cell(&self, key: PropertyPath) -> (usize, usize, usize) {
        let (group, row) = self.rows[&key.prim_path()];
        let names = &self.groups[group].names;
        let col = names
            .iter()
            .position(|&name| name == key.property())
            .unwrap();
        (group, row / ROWS, (row % ROWS) * names.len() + col)
    }
    pub(super) fn get(&self, key: PropertyPath) -> &PropertySpec {
        let (group, block, slot) = self.cell(key);
        &self.groups[group].blocks[block][slot]
    }
    pub(super) fn get_mut(&mut self, key: PropertyPath) -> &mut PropertySpec {
        let (group, block, slot) = self.cell(key);
        Arc::make_mut(&mut Arc::make_mut(&mut self.groups[group].blocks[block])[slot])
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = PropertyPath> + '_ {
        self.rows.iter().flat_map(|(&p, &(group, _))| {
            self.groups[group]
                .names
                .iter()
                .map(move |&n| PropertyPath::new(p, n))
        })
    }
    pub(super) fn restore(&self, layer: &mut Layer) {
        let prims = layer.prims_mut();
        for (&p, &(group, row)) in &self.rows {
            let group = &self.groups[group];
            let start = (row % ROWS) * group.names.len();
            prims.get_mut(&p).unwrap().properties = group
                .names
                .iter()
                .zip(&group.blocks[row / ROWS][start..])
                .map(|(&name, spec)| PropertyEntry {
                    name,
                    spec: Arc::clone(spec),
                })
                .collect();
        }
    }
}
