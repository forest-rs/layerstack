// Copyright 2016, 2021 Pixar
// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: LicenseRef-TOST-1.0
// Rust adaptation of UsdPhysicsDiagonalize and GfMatrix4f rotation extraction;
// see LICENSE-TOST-1.0 and NOTICE.
#![allow(
    clippy::cast_possible_truncation,
    reason = "match Gf float32 rounding after double square roots and quaternion conversions"
)]
use super::{Matrix3, mul, rotation, transpose};

pub(super) fn diagonalize(inertia: Matrix3) -> ([f32; 3], [f32; 4]) {
    let mut q = [0., 0., 0., 1.];
    let mut d = inertia;
    for _ in 0..24 {
        let axes = rotation(q);
        d = mul(mul(axes, inertia), transpose(axes));
        let [d0, d1, d2] = [d[1][2].abs(), d[0][2].abs(), d[0][1].abs()];
        let axis = if d0 > d1 && d0 > d2 {
            0
        } else if d1 > d2 {
            1
        } else {
            2
        };
        let a = (axis + 1) % 3;
        let b = (a + 1) % 3;
        if d[a][b] == 0.
            || f64::from((d[a][a] - d[b][b]).abs()) > 2e6 * (2. * f64::from(d[a][b])).abs()
        {
            break;
        }
        let w = (d[a][a] - d[b][b]) / (2. * d[a][b]);
        let (s, c) = if w.abs() > 1000. {
            (1. / (4. * w), 1.)
        } else {
            let t = (1. / (f64::from(w.abs()) + libm::sqrt(f64::from(w * w + 1.)))) as f32;
            let h = (1. / libm::sqrt(f64::from(t * t + 1.))) as f32;
            (
                libm::sqrtf((1. - h) / 2.) * if w >= 0. { 1. } else { -1. },
                libm::sqrtf((1. + h) / 2.),
            )
        };
        let mut r = [0., 0., 0., c];
        r[axis] = s;
        let [x, y, z, w] = q;
        let [a, b, c, d] = r;
        q = [
            w * a + x * d + y * c - z * b,
            w * b + y * d + z * a - x * c,
            w * c + z * d + x * b - y * a,
            w * d - x * a - y * b - z * c,
        ];
        let length = libm::sqrtf(q.iter().map(|v| v * v).sum());
        q = q.map(|v| v / length);
    }
    ([d[0][0], d[1][1], d[2][2]], q)
}

/// `GfMatrix4f::ExtractRotation().GetQuat()`, including the float32
/// extraction followed by `GfRotation`'s normalized double round trip.
pub(super) fn extracted_orientation(orientation: [f32; 4]) -> [f32; 4] {
    let m = rotation(orientation);
    let i = if m[0][0] > m[1][1] {
        if m[0][0] > m[2][2] { 0 } else { 2 }
    } else if m[1][1] > m[2][2] {
        1
    } else {
        2
    };
    let trace = m[0][0] + m[1][1] + m[2][2];
    let mut q = [0.; 4];
    if trace > m[i][i] {
        q[3] = (0.5 * libm::sqrt(f64::from(trace + 1.))) as f32;
        q[0] = (f64::from(m[1][2] - m[2][1]) / (4. * f64::from(q[3]))) as f32;
        q[1] = (f64::from(m[2][0] - m[0][2]) / (4. * f64::from(q[3]))) as f32;
        q[2] = (f64::from(m[0][1] - m[1][0]) / (4. * f64::from(q[3]))) as f32;
    } else {
        let j = (i + 1) % 3;
        let k = (i + 2) % 3;
        q[i] = (0.5 * libm::sqrt(f64::from(m[i][i] - m[j][j] - m[k][k] + 1.))) as f32;
        q[j] = (m[i][j] + m[j][i]) / (4. * q[i]);
        q[k] = (m[k][i] + m[i][k]) / (4. * q[i]);
        q[3] = (m[j][k] - m[k][j]) / (4. * q[i]);
    }
    q[3] = q[3].clamp(-1., 1.);
    crate::gf::Rotation::from_quat(q.map(f64::from))
        .quat()
        .map(|v| v as f32)
}
