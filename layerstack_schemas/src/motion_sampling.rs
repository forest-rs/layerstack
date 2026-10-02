// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared motion sample anchoring; composed value resolution stays in Stage.
use crate::{PrimView, Scene, Time};
use alloc::vec::Vec;
use layerstack::{TokenInterner, Value};

pub(crate) fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Option<T> {
    match time {
        Time::Default => prim.read_value(name, decode),
        Time::At {
            code,
            interpolation,
        } => prim.read_value_at(name, code, interpolation, decode),
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Anchor {
    pub(crate) time: Time,
    pub(crate) sample: Option<f64>,
    pub(crate) bracket: Option<[f64; 2]>,
}
pub(crate) fn anchor(
    prim: &PrimView<'_>,
    name: &'static str,
    base: Time,
) -> Result<Anchor, &'static str> {
    let mut result = Anchor {
        time: Time::Default,
        sample: None,
        bracket: None,
    };
    let Time::At { code, .. } = base else {
        return Ok(result);
    };
    // AOUSD Core §12.3–12.5: dense arrays terminate value-source search;
    // generic sparse edits compose over weaker values. Only sparse opinions
    // active at this base time expose weaker sample grids. Value resolution at
    // the selected stage-time anchor still belongs to Stage, including offsets.
    let opinions = prim
        .property_path(name)
        .and_then(|p| prim.scene().stage().explain_property_path(p))
        .unwrap_or_default();
    let mut times = Vec::new();
    for opinion in opinions {
        if let Some(samples) = opinion.value.time_samples().filter(|s| !s.is_empty()) {
            let mut mapped: Vec<_> = samples
                .iter()
                .map(|(t, v)| {
                    (
                        t * opinion.layer_offset.scale + opinion.layer_offset.offset,
                        v,
                    )
                })
                .collect();
            mapped.sort_by(|a, b| a.0.total_cmp(&b.0));
            times.extend(mapped.iter().map(|&(t, _)| t));
            let active = mapped
                .iter()
                .rfind(|&&(t, _)| t <= code)
                .unwrap_or(&mapped[0])
                .1;
            if active.array_edit_ref().is_none() {
                break;
            }
        } else if opinion.value.spline().is_some() {
            // USD splines are scalar-valued, so an array motion source cannot
            // be anchored as discrete point/rotation samples.
            return Err(name);
        } else if let Some(value) = opinion.value.default_value()
            && value.array_edit_ref().is_none()
        {
            break;
        }
    }
    if !times.is_empty() {
        times.sort_by(f64::total_cmp);
        times.dedup_by(|a, b| *a == *b);
        let lower = times
            .iter()
            .copied()
            .take_while(|&t| t <= code)
            .last()
            .unwrap_or(times[0]);
        let upper = times
            .iter()
            .copied()
            .find(|&t| t > code)
            .unwrap_or(*times.last().expect("nonempty samples"));
        result = Anchor {
            time: Time::held(lower),
            sample: Some(lower),
            bracket: Some([lower, upper]),
        };
    }
    Ok(result)
}
pub(crate) fn aligned(a: Anchor, b: Anchor) -> bool {
    // UsdGeom samplingUtils::_CheckSampleAlignment uses GfIsClose with an
    // absolute double epsilon for the anchor and both bracket times. Layer
    // offsets can introduce tiny differences into equivalent authored grids.
    // AOUSD Core §12.5 (time samples and layer offsets).
    match (a.sample, b.sample, a.bracket, b.bracket) {
        (Some(a), Some(b), Some(ab), Some(bb)) => [(a, b), (ab[0], bb[0]), (ab[1], bb[1])]
            .into_iter()
            .all(|(a, b)| (a - b).abs() < f64::EPSILON),
        _ => false,
    }
}
pub(crate) fn vectors(prim: &PrimView<'_>, name: &str, time: Time) -> Option<Vec<[f32; 3]>> {
    read(prim, name, time, crate::value::read_float3_array)
}
pub(crate) fn rate(scene: &Scene<'_>) -> f64 {
    ["timeCodesPerSecond", "framesPerSecond"]
        .into_iter()
        .find_map(|name| {
            let key = scene.store().tokens().lookup(name)?;
            match scene.stage().layer_metadata(key, scene.store())? {
                Value::Double(v) => Some(v),
                Value::Float(v) => Some(f64::from(v)),
                _ => None,
            }
        })
        .unwrap_or(24.0)
}
