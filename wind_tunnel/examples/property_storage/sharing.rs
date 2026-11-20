// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{
    hash::{BuildHasher, Hash, Hasher},
    sync::Arc,
};

use hashbrown::DefaultHashBuilder;
use layerstack::{PrimSpec, PropertySpec, Value};

/// Bounded import-time sharing for common scalar values and short explicit lists.
/// Eligibility excludes floating-point values so equality preserves signed zero.
#[derive(Clone, Default)]
pub(super) struct Sharing {
    hasher: DefaultHashBuilder,
    slots: Vec<Option<Arc<PropertySpec>>>,
}

impl Sharing {
    pub(super) fn capture(&mut self, prim: &mut PrimSpec) {
        for entry in &mut prim.properties {
            let s = &entry.spec;
            if Arc::strong_count(s) > 1
                || s.time_samples.is_authored()
                || s.spline.is_some()
                || !s.metadata.is_empty()
            {
                continue;
            }
            let mut hash = self.hasher.build_hasher();
            match s.default.as_ref() {
                None | Some(Value::Blocked) => (),
                Some(Value::Bool(v)) => v.hash(&mut hash),
                Some(Value::String(v) | Value::Asset(v)) if v.len() <= 128 => v.hash(&mut hash),
                Some(Value::Token(v)) => v.hash(&mut hash),
                Some(Value::Int(v)) => v.hash(&mut hash),
                Some(Value::UInt(v)) => v.hash(&mut hash),
                _ => continue,
            }
            if let Some(op) = s.targets.as_deref() {
                let Some(items) = op.explicit.as_deref() else {
                    continue;
                };
                if items.len() > 2 || super::edges::count(op) != items.len() {
                    continue;
                }
                items.hash(&mut hash);
            }
            s.type_name
                .as_ref()
                .map(layerstack::PropertyType::type_name)
                .hash(&mut hash);
            if self.slots.is_empty() {
                self.slots.resize_with(4096, || None);
            }
            let slot = &mut self.slots[(hash.finish() & 4095) as usize];
            if let Some(existing) = slot
                && **existing == **s
            {
                entry.spec = Arc::clone(existing);
            } else {
                *slot = Some(Arc::clone(s));
            }
        }
    }
    pub(super) fn finish(&mut self) {
        self.slots = Vec::new();
    }
}
