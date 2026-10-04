// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Clip definitions are part of runtime prototype identity, even before assets
//! become resident. OpenUSD `Usd_InstanceKey`, `Usd_ClipSetDefinition`.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClipInstanceKey {
    stack: LayerId,
    site: SpecPath,
    layer_strength: u16,
    offset: (u64, u64),
    parameters: Vec<(Arc<str>, ClipParameter)>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ClipParameter {
    Bool(bool),
    Double(u64),
    String(Arc<str>),
    Asset(Arc<str>),
    Pair([u64; 2]),
    Array(Vec<Self>),
    Invalid,
}
impl ClipParameter {
    fn capture(value: &Value) -> Self {
        match value {
            Value::Bool(v) => Self::Bool(*v),
            Value::Double(v) => Self::Double(v.to_bits()),
            Value::String(v) => Self::String(v.clone()),
            Value::Asset(v) => Self::Asset(v.clone()),
            Value::Vec2d(v) => Self::Pair(v.map(f64::to_bits)),
            Value::Array(_) | Value::TypedArray(_) => {
                value.array_ref().map_or(Self::Invalid, |array| {
                    Self::Array(array.iter().map(|v| Self::capture(&v)).collect())
                })
            }
            _ => Self::Invalid,
        }
    }
}

pub(crate) fn instance_keys(
    instance: PathId,
    index: &PrimIndex,
    store: &dyn LayerStore,
) -> Vec<ClipInstanceKey> {
    if !store
        .tokens()
        .lookup("clips")
        .is_some_and(|field| index.metadata_opinions(field).is_some())
    {
        return Vec::new();
    }
    definitions(instance, index, store, &mut Vec::new())
        .into_iter()
        .map(|definition| {
            let stack = index
                .graph
                .node(definition.anchor.node)
                .map_or(definition.anchor.layer_id, |node| node.layer_stack());
            ClipInstanceKey {
                stack,
                site: definition.anchor.spec_path,
                layer_strength: definition.anchor.layer_strength,
                offset: (
                    definition.offset.offset.to_bits(),
                    definition.offset.scale.to_bits(),
                ),
                parameters: definition
                    .dictionary
                    .into_iter()
                    .filter(|(name, _)| {
                        matches!(
                            &**name,
                            "assetPaths"
                                | "manifestAssetPath"
                                | "primPath"
                                | "active"
                                | "times"
                                | "interpolateMissingClipValues"
                                | "templateAssetPath"
                                | "templateStartTime"
                                | "templateEndTime"
                                | "templateStride"
                                | "templateActiveOffset"
                        )
                    })
                    .map(|(name, value)| (name, ClipParameter::capture(&value)))
                    .collect(),
            }
        })
        .collect()
}
