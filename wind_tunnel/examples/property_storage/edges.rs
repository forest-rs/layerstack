// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use hashbrown::HashMap;
use layerstack::{ListOp, PropertyPath, TargetPath};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Native,
    Packed,
    Inline,
}

#[derive(Clone)]
enum Edge {
    Empty,
    One(TargetPath),
    Two([TargetPath; 2]),
    Run(Range<u32>),
    Mixed(Box<[Option<Range<u32>>; 6]>),
    Owned(Box<ListOp<TargetPath>>),
}

#[derive(Clone, Default)]
pub(super) struct Edges {
    entries: HashMap<PropertyPath, Edge>,
    values: Arc<Vec<TargetPath>>,
}

fn lists(op: &ListOp<TargetPath>) -> [Option<&[TargetPath]>; 6] {
    [
        op.explicit.as_deref(),
        Some(&op.prepend),
        Some(&op.append),
        Some(&op.delete),
        Some(&op.add),
        Some(&op.reorder),
    ]
}

pub(super) fn count(op: &ListOp<TargetPath>) -> usize {
    lists(op).into_iter().flatten().map(<[_]>::len).sum()
}

impl Edges {
    fn append(&mut self, values: &[TargetPath]) -> Range<u32> {
        let arena = Arc::make_mut(&mut self.values);
        let start = u32::try_from(arena.len()).unwrap();
        arena.extend_from_slice(values);
        start..u32::try_from(arena.len()).unwrap()
    }

    pub(super) fn insert(&mut self, key: PropertyPath, op: Box<ListOp<TargetPath>>, mode: Mode) {
        let slots = lists(&op);
        let edge = if let Some(items) = slots[0]
            && slots[1..].iter().flatten().all(|v| v.is_empty())
        {
            match (mode, items) {
                (Mode::Inline, []) => Edge::Empty,
                (Mode::Inline, [one]) => Edge::One(*one),
                (Mode::Inline, [a, b]) => Edge::Two([*a, *b]),
                (Mode::Inline, _) => Edge::Owned(op),
                _ => Edge::Run(self.append(items)),
            }
        } else if mode == Mode::Packed {
            Edge::Mixed(Box::new(slots.map(|v| v.map(|values| self.append(values)))))
        } else {
            Edge::Owned(op)
        };
        self.entries.insert(key, edge);
    }

    pub(super) fn count(&self, key: PropertyPath) -> Option<usize> {
        self.entries.get(&key).map(|v| match v {
            Edge::Empty => 0,
            Edge::One(_) => 1,
            Edge::Two(_) => 2,
            Edge::Run(r) => r.len(),
            Edge::Mixed(slots) => slots.iter().flatten().map(|r| r.len()).sum(),
            Edge::Owned(op) => count(op),
        })
    }

    pub(super) fn expand(&self, key: PropertyPath) -> Option<Box<ListOp<TargetPath>>> {
        let slice = |r: &Range<u32>| self.values[r.start as usize..r.end as usize].to_vec();
        let edge = self.entries.get(&key)?;
        let explicit = match edge {
            Edge::Empty => vec![],
            Edge::One(a) => vec![*a],
            Edge::Two(v) => v.to_vec(),
            Edge::Run(r) => slice(r),
            Edge::Owned(op) => return Some(op.clone()),
            Edge::Mixed(slots) => {
                let [explicit, prepend, append, delete, add, reorder] =
                    slots.each_ref().map(|r| r.as_ref().map(slice));
                let mut op = ListOp::new();
                op.explicit = explicit;
                op.prepend = prepend.unwrap();
                op.append = append.unwrap();
                op.delete = delete.unwrap();
                op.add = add.unwrap();
                op.reorder = reorder.unwrap();
                return Some(Box::new(op));
            }
        };
        let mut op = ListOp::new();
        op.explicit = Some(explicit);
        Some(Box::new(op))
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = PropertyPath> + '_ {
        self.entries.keys().copied()
    }
}
