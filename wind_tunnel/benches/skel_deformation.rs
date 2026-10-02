// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Equivalent validated CPU kernels: scalar, glam and explicit safe SIMD.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    reason = "bounded deterministic benchmark inputs and USD float rounding"
)]
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fearless_simd::{Level, dispatch, f32x4, f64x4, prelude::*};
use layerstack_schemas::skel::{
    InfluenceInterpolation, JointInfluences, apply_blend_shape, apply_blend_shape_in_place,
    skin_points, skin_points_in_place,
};
use std::{hint::black_box, time::Duration};
type Matrix = [[f64; 4]; 4];
const IDENTITY: Matrix = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];
fn skin_glam(
    bind: &Matrix,
    joints: &[Matrix],
    inf: JointInfluences<'_>,
    points: &[[f32; 3]],
) -> Vec<[f32; 3]> {
    inf.validate(points.len(), joints.len()).unwrap();
    let transform = |p: [f32; 3], m: &Matrix| {
        (glam::DVec4::from_array(m[0]) * f64::from(p[0])
            + glam::DVec4::from_array(m[1]) * f64::from(p[1])
            + glam::DVec4::from_array(m[2]) * f64::from(p[2])
            + glam::DVec4::from_array(m[3]))
        .to_array()
        .map(|v| v as f32)
    };
    points
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let p = transform(p, bind);
            let mut result = glam::Vec4::ZERO;
            for j in 0..inf.element_size {
                let index = i * inf.element_size + j;
                if inf.weights[index] != 0. {
                    result += glam::Vec4::from_array(transform(
                        [p[0], p[1], p[2]],
                        &joints[inf.indices[index] as usize],
                    )) * inf.weights[index];
                }
            }
            let a = result.to_array();
            [a[0], a[1], a[2]]
        })
        .collect()
}
#[inline(always)]
fn skin_simd<S: Simd>(
    simd: S,
    bind: &Matrix,
    joints: &[Matrix],
    inf: JointInfluences<'_>,
    points: &[[f32; 3]],
) -> Vec<[f32; 3]> {
    inf.validate(points.len(), joints.len()).unwrap();
    let transform = |p: [f32; 3], m: &Matrix| {
        let value = f64x4::from_slice(simd, &m[0]) * f64::from(p[0])
            + f64x4::from_slice(simd, &m[1]) * f64::from(p[1])
            + f64x4::from_slice(simd, &m[2]) * f64::from(p[2])
            + f64x4::from_slice(simd, &m[3]);
        let value: [f64; 4] = value.into();
        value.map(|v| v as f32)
    };
    points
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let p = transform(p, bind);
            let mut result = f32x4::splat(simd, 0.);
            for j in 0..inf.element_size {
                let index = i * inf.element_size + j;
                if inf.weights[index] != 0. {
                    result += f32x4::from_slice(
                        simd,
                        &transform([p[0], p[1], p[2]], &joints[inf.indices[index] as usize]),
                    ) * inf.weights[index];
                }
            }
            let a: [f32; 4] = result.into();
            [a[0], a[1], a[2]]
        })
        .collect()
}
fn blend_glam(weight: f32, offsets: &[[f32; 3]], points: &[[f32; 3]]) -> Vec<[f32; 3]> {
    assert!(weight.is_finite(), "blend weight must be finite");
    assert_eq!(
        offsets.len(),
        points.len(),
        "dense blend offsets must match points"
    );
    let mut result = points.to_vec();
    let dst = result.as_flattened_mut();
    let src = offsets.as_flattened();
    let (out, tail) = dst.as_chunks_mut::<4>();
    let (input, rest) = src.as_chunks::<4>();
    for (o, v) in out.iter_mut().zip(input) {
        (glam::Vec4::from_slice(o) + glam::Vec4::from_slice(v) * weight).write_to_slice(o);
    }
    for (o, v) in tail.iter_mut().zip(rest) {
        *o += v * weight;
    }
    result
}
#[inline(always)]
#[allow(
    clippy::chunks_exact_to_as_chunks,
    reason = "associated SIMD lane count cannot be used as a stable const generic"
)]
fn blend_simd<S: Simd>(
    simd: S,
    weight: f32,
    offsets: &[[f32; 3]],
    points: &[[f32; 3]],
) -> Vec<[f32; 3]> {
    assert!(weight.is_finite(), "blend weight must be finite");
    assert_eq!(
        offsets.len(),
        points.len(),
        "dense blend offsets must match points"
    );
    let mut result = points.to_vec();
    let dst = result.as_flattened_mut();
    let src = offsets.as_flattened();
    let mut out = dst.chunks_exact_mut(S::f32s::LEN);
    let mut input = src.chunks_exact(S::f32s::LEN);
    for (o, v) in (&mut out).zip(&mut input) {
        (S::f32s::from_slice(simd, o) + S::f32s::from_slice(simd, v) * weight).store_slice(o);
    }
    for (o, v) in out.into_remainder().iter_mut().zip(input.remainder()) {
        *o += v * weight;
    }
    result
}
fn bench(c: &mut Criterion) {
    let level = Level::new();
    let mut group = c.benchmark_group("skel_deformation");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for count in [32, 1_000, 100_000] {
        let points: Vec<_> = (0..count)
            .map(|i| [i as f32 * 0.01, (i % 31) as f32, (i % 7) as f32])
            .collect();
        let offsets: Vec<_> = (0..count)
            .map(|i| [(i % 5) as f32 * 0.1, -0.25, 0.3])
            .collect();
        let joints: Vec<_> = (0..128)
            .map(|i| {
                let mut m = IDENTITY;
                m[0][0] = 1.1;
                m[1][0] = 0.15;
                m[3][0] = i as f64 * 0.1;
                m
            })
            .collect();
        let indices: Vec<_> = (0..count * 4)
            .map(|i| ((i * 17 + 3) % 128) as i32)
            .collect();
        let weights = vec![0.25; count * 4];
        let influences = JointInfluences {
            indices: &indices,
            weights: &weights,
            element_size: 4,
            interpolation: InfluenceInterpolation::Vertex,
        };
        let baseline = skin_points(&IDENTITY, &joints, influences, &points).unwrap();
        assert_eq!(
            baseline,
            skin_glam(&IDENTITY, &joints, influences, &points),
            "glam skinning preserves scalar rounding"
        );
        assert_eq!(
            baseline,
            dispatch!(level, simd => skin_simd(simd, &IDENTITY, &joints, influences, &points)),
            "SIMD skinning preserves scalar rounding"
        );
        let baseline = apply_blend_shape(0.7, &offsets, &[], &points).unwrap();
        assert_eq!(
            baseline,
            blend_glam(0.7, &offsets, &points),
            "glam blend offsets preserve scalar rounding"
        );
        assert_eq!(
            baseline,
            dispatch!(level, simd => blend_simd(simd, 0.7, &offsets, &points)),
            "SIMD blend offsets preserve scalar rounding"
        );
        group.throughput(Throughput::Elements(count as u64));
        let mut buffer = points.clone();
        group.bench_with_input(BenchmarkId::new("lbs_reuse", count), &count, |b, _| {
            b.iter(|| {
                buffer.copy_from_slice(&points);
                skin_points_in_place(
                    black_box(&IDENTITY),
                    black_box(&joints),
                    black_box(influences),
                    black_box(&mut buffer),
                )
                .unwrap();
                black_box(&buffer);
            });
        });
        group.bench_with_input(BenchmarkId::new("blend_reuse", count), &count, |b, _| {
            b.iter(|| {
                buffer.copy_from_slice(&points);
                apply_blend_shape_in_place(
                    black_box(0.7),
                    black_box(&offsets),
                    &[],
                    black_box(&mut buffer),
                )
                .unwrap();
                black_box(&buffer);
            });
        });
        group.bench_with_input(
            BenchmarkId::new(
                if cfg!(feature = "simd") {
                    "lbs_accelerated"
                } else {
                    "lbs_scalar"
                },
                count,
            ),
            &count,
            |b, _| {
                b.iter(|| {
                    black_box(
                        skin_points(
                            black_box(&IDENTITY),
                            black_box(&joints),
                            black_box(influences),
                            black_box(&points),
                        )
                        .unwrap(),
                    )
                });
            },
        );
        group.bench_with_input(BenchmarkId::new("lbs_glam", count), &count, |b, _| {
            b.iter(|| {
                black_box(skin_glam(
                    black_box(&IDENTITY),
                    black_box(&joints),
                    black_box(influences),
                    black_box(&points),
                ))
            });
        });
        group.bench_with_input(BenchmarkId::new("lbs_simd", count), &count, |b, _| b.iter(|| black_box(dispatch!(level, simd => skin_simd(simd, black_box(&IDENTITY), black_box(&joints), black_box(influences), black_box(&points))))));
        group.bench_with_input(BenchmarkId::new("blend_scalar", count), &count, |b, _| {
            b.iter(|| {
                black_box(
                    apply_blend_shape(black_box(0.7), black_box(&offsets), &[], black_box(&points))
                        .unwrap(),
                )
            });
        });
        group.bench_with_input(BenchmarkId::new("blend_glam", count), &count, |b, _| {
            b.iter(|| {
                black_box(blend_glam(
                    black_box(0.7),
                    black_box(&offsets),
                    black_box(&points),
                ))
            });
        });
        group.bench_with_input(BenchmarkId::new("blend_simd", count), &count, |b, _| b.iter(|| black_box(dispatch!(level, simd => blend_simd(simd, black_box(0.7), black_box(&offsets), black_box(&points))))));
    }
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
