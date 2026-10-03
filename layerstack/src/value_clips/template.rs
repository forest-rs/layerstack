// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{Arc, Value, Vec, get};
use alloc::{format, string::String};

pub(super) struct Expansion {
    pub(super) candidates: Vec<(f64, String)>,
    pub(super) active_offset: f64,
    pub(super) front: Option<f64>,
    pub(super) back: Option<f64>,
}
fn number(dictionary: &[(Arc<str>, Value)], field: &str) -> Result<f64, ()> {
    match get(dictionary, field) {
        Some(Value::Double(v)) if v.is_finite() => Ok(*v),
        _ => Err(()),
    }
}
/// Bounded pure filename enumeration; host availability later compacts the
/// schedule. Native `clipSetDefinition.cpp::_DeriveClipInfo` accumulates in
/// 1/10000 units before formatting, and uses truncation for the integer group.
pub(super) fn expand(dictionary: &[(Arc<str>, Value)], pattern: &str) -> Result<Expansion, ()> {
    let start = number(dictionary, "templateStartTime")?;
    let end = number(dictionary, "templateEndTime")?;
    let stride = number(dictionary, "templateStride")?;
    let offset = get(dictionary, "templateActiveOffset")
        .map(|_| number(dictionary, "templateActiveOffset"))
        .transpose()?;
    if start > end || stride <= 0. || offset.is_some_and(|v| v.abs() > stride) {
        return Err(());
    }
    // SdfLayer identifiers separate format arguments before filename expansion.
    let (pattern, arguments) = pattern
        .find(":SDF_FORMAT_ARGS:")
        .map_or((pattern, ""), |i| pattern.split_at(i));
    let slash = pattern.rfind('/').map_or(0, |i| i + 1);
    let directory = &pattern[..slash];
    let mut tokens: Vec<String> = pattern[slash..]
        .split('.')
        .filter(|token| !token.is_empty())
        .map(String::from)
        .collect();
    let groups: Vec<_> = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| !token.is_empty() && token.bytes().all(|b| b == b'#'))
        .map(|(index, token)| (index, token.len()))
        .collect();
    if !(groups.len() == 1 || (groups.len() == 2 && groups[1].0 == groups[0].0 + 1))
        || groups.iter().any(|(_, width)| *width > 32)
        || groups.get(1).is_some_and(|(_, width)| *width > 18)
    {
        return Err(());
    }
    let mut promoted = start * 10000.;
    let limit = end * 10000.;
    let step = stride * 10000.;
    if !promoted.is_finite() || !limit.is_finite() || !step.is_finite() || step <= 0. {
        return Err(());
    }
    let mut candidates = Vec::new();
    while promoted <= limit {
        if candidates.len() == 100_000 {
            return Err(());
        }
        let time = promoted / 10000.;
        if time < f64::from(i32::MIN) || time > f64::from(i32::MAX) {
            return Err(());
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "native integer filename formatting truncates, range checked"
        )]
        let integer = time as i32;
        tokens[groups[0].0] = format!("{integer:0width$}", width = groups[0].1);
        if let Some(&(index, precision)) = groups.get(1) {
            let decimal = format!("{time:.precision$}");
            tokens[index] = decimal.split_once('.').ok_or(())?.1.into();
        }
        candidates.push((time, format!("{directory}{}{arguments}", tokens.join("."))));
        let next = promoted + step;
        if !next.is_finite() || next <= promoted {
            return Err(());
        }
        promoted = next;
    }
    let front = offset.map(|v| start - v.abs());
    let back = offset.map(|v| end + v.abs());
    if front.into_iter().chain(back).any(|v| !v.is_finite()) {
        return Err(());
    }
    Ok(Expansion {
        candidates,
        active_offset: offset.unwrap_or(0.),
        front,
        back,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn metadata(start: f64, end: f64, stride: f64) -> Vec<(Arc<str>, Value)> {
        vec![
            ("templateStartTime".into(), Value::Double(start)),
            ("templateEndTime".into(), Value::Double(end)),
            ("templateStride".into(), Value::Double(stride)),
        ]
    }

    #[test]
    fn native_fractional_filename_schedule_and_identifier_arguments() {
        let result = expand(
            &metadata(-1.25, -0.75, 0.25),
            "dir/clip.###.##.usda:SDF_FORMAT_ARGS:target=usd",
        )
        .unwrap();
        assert_eq!(
            result.candidates,
            vec![
                (
                    -1.25,
                    "dir/clip.-01.25.usda:SDF_FORMAT_ARGS:target=usd".into()
                ),
                (
                    -1.,
                    "dir/clip.-01.00.usda:SDF_FORMAT_ARGS:target=usd".into()
                ),
                (
                    -0.75,
                    "dir/clip.000.75.usda:SDF_FORMAT_ARGS:target=usd".into()
                ),
            ]
        );
        assert_eq!((result.front, result.back), (None, None));
    }

    #[test]
    fn explicitly_authored_zero_offset_adds_endpoint_knots() {
        let mut dictionary = metadata(1., 2., 1.);
        dictionary.push(("templateActiveOffset".into(), Value::Double(0.)));
        let result = expand(&dictionary, "clip.##.usd").unwrap();
        assert_eq!(
            (result.front, result.back, result.active_offset),
            (Some(1.), Some(2.), 0.)
        );
        dictionary.last_mut().unwrap().1 = Value::Double(-0.5);
        let result = expand(&dictionary, "clip.##.usd").unwrap();
        assert_eq!(
            (result.front, result.back, result.active_offset),
            (Some(0.5), Some(2.5), -0.5)
        );
    }

    #[test]
    fn malformed_or_excessive_templates_fail_before_asset_lookup() {
        for pattern in [
            "clip.usd",
            "clip##.usd",
            "clip.#.part.#.usd",
            "clip.#.#.#.usd",
        ] {
            assert!(expand(&metadata(0., 1., 1.), pattern).is_err());
        }
        for dictionary in [
            metadata(2., 1., 1.),
            metadata(0., 1., 0.),
            metadata(f64::INFINITY, 1., 1.),
            metadata(0., 100_000., 1.),
            metadata(1e30, 1e30, 1.),
        ] {
            assert!(expand(&dictionary, "clip.#.usd").is_err());
        }
        let mut dictionary = metadata(0., 1., 1.);
        dictionary.push(("templateActiveOffset".into(), Value::Double(2.)));
        assert!(expand(&dictionary, "clip.#.usd").is_err());
    }
}
