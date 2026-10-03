// Copyright 2016 Pixar
// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: LicenseRef-TOST-1.0
//
// Rust adaptation of OpenUSD GfMatrix4d::Factor/_Jacobi3,
// GfOrthogonalizeBasis and GfMatrix3d::ExtractRotationQuaternion.
// See LICENSE-TOST-1.0 and NOTICE.

//! Private affine and rotation decomposition shared by USD skinning and cameras.
use crate::gf;
pub(crate) type Matrix3 = [[f64; 3]; 3];
pub(crate) const IDENTITY: Matrix3 = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
fn transpose(m: &Matrix3) -> Matrix3 {
    core::array::from_fn(|i| core::array::from_fn(|j| m[j][i]))
}
pub(crate) fn mul(a: &Matrix3, b: &Matrix3) -> Matrix3 {
    core::array::from_fn(|i| {
        core::array::from_fn(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j])
    })
}
pub(crate) fn inverse(m: &Matrix3) -> Matrix3 {
    let mut full = gf::IDENTITY;
    for i in 0..3 {
        full[i][..3].copy_from_slice(&m[i]);
    }
    let (inverted, _) = gf::inverse(&full);
    core::array::from_fn(|i| core::array::from_fn(|j| inverted[i][j]))
}
#[allow(
    clippy::needless_range_loop,
    reason = "Jacobi rotations update paired matrix rows and columns"
)]
fn jacobi(mut a: Matrix3) -> ([f64; 3], Matrix3) {
    let mut values = [a[0][0], a[1][1], a[2][2]];
    let mut vectors = IDENTITY;
    let mut base = values;
    let mut correction = [0.; 3];
    for sweep in 0..50 {
        let off_diagonal = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off_diagonal == 0. {
            break;
        }
        let threshold = if sweep < 3 {
            0.2 * off_diagonal / 9.
        } else {
            0.
        };
        for p in 0..2 {
            for q in p + 1..3 {
                let magnitude = 100. * a[p][q].abs();
                if sweep > 3
                    && values[p].abs() + magnitude == values[p].abs()
                    && values[q].abs() + magnitude == values[q].abs()
                {
                    a[p][q] = 0.;
                } else if a[p][q].abs() > threshold {
                    let difference = values[q] - values[p];
                    let tangent = if difference.abs() + magnitude == difference.abs() {
                        a[p][q] / difference
                    } else {
                        let theta = 0.5 * difference / a[p][q];
                        let t = 1. / (theta.abs() + libm::sqrt(1. + theta * theta));
                        if theta < 0. { -t } else { t }
                    };
                    let cosine = 1. / libm::sqrt(1. + tangent * tangent);
                    let sine = tangent * cosine;
                    let tau = sine / (1. + cosine);
                    let delta = tangent * a[p][q];
                    correction[p] -= delta;
                    correction[q] += delta;
                    values[p] -= delta;
                    values[q] += delta;
                    a[p][q] = 0.;
                    let rotate =
                        |g: f64, h: f64| (g - sine * (h + g * tau), h + sine * (g - h * tau));
                    for row in a.iter_mut().take(p) {
                        (row[p], row[q]) = rotate(row[p], row[q]);
                    }
                    for j in p + 1..q {
                        (a[p][j], a[j][q]) = rotate(a[p][j], a[j][q]);
                    }
                    for j in q + 1..3 {
                        (a[p][j], a[q][j]) = rotate(a[p][j], a[q][j]);
                    }
                    for row in &mut vectors {
                        (row[p], row[q]) = rotate(row[p], row[q]);
                    }
                }
            }
        }
        for p in 0..3 {
            base[p] += correction[p];
            values[p] = base[p];
            correction[p] = 0.;
        }
    }
    (values, vectors)
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn normalized(v: [f64; 3]) -> [f64; 3] {
    let n = libm::sqrt(dot(v, v)).max(1e-10);
    v.map(|x| x / n)
}
pub(crate) fn orthonormalize(mut rows: Matrix3) -> Matrix3 {
    rows = rows.map(normalized);
    let close = |a: [f64; 3], b: [f64; 3]| {
        let d = core::array::from_fn(|i| a[i] - b[i]);
        dot(d, d) <= 1e-12
    };
    if close(rows[0], rows[1]) || close(rows[0], rows[2]) || close(rows[1], rows[2]) {
        return rows;
    }
    for _ in 0..20 {
        let next = core::array::from_fn(|i| {
            let mut projected = rows[i];
            for (j, row) in rows.iter().enumerate() {
                if j != i {
                    let d = dot(*row, projected);
                    for k in 0..3 {
                        projected[k] -= d * row[k];
                    }
                }
            }
            normalized(core::array::from_fn(|k| 0.5 * (rows[i][k] + projected[k])))
        });
        let error = (0..3)
            .map(|i| {
                let d = core::array::from_fn(|k| rows[i][k] - next[i][k]);
                dot(d, d)
            })
            .sum::<f64>();
        if error < 1e-12 {
            break;
        }
        rows = next;
    }
    rows
}
fn determinant(matrix: &Matrix3) -> f64 {
    matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0])
}
pub(crate) fn factored_rotation(matrix: &Matrix3) -> Option<Matrix3> {
    let determinant = determinant(matrix);
    if determinant.abs() < 1e-10 {
        return None;
    }
    let sign = if determinant < 0. { -1. } else { 1. };
    let (eigenvalues, orientation) = jacobi(mul(matrix, &transpose(matrix)));
    let mut inverse_scales = IDENTITY;
    for i in 0..3 {
        let scale = if eigenvalues[i] < 1e-10 {
            1e-10
        } else {
            libm::sqrt(eigenvalues[i])
        };
        inverse_scales[i][i] = 1. / (sign * scale);
    }
    Some(orthonormalize(mul(
        &mul(
            &mul(&orientation, &inverse_scales),
            &transpose(&orientation),
        ),
        matrix,
    )))
}
/// `GfTransform`'s principal stretch scales, including the reflection sign.
#[cfg(all(feature = "usd-physics", feature = "usd-shade"))]
pub(crate) fn factored_scale(matrix: &Matrix3) -> Option<[f64; 3]> {
    if !matrix.iter().flatten().all(|v| v.is_finite()) {
        return None;
    }
    let sign = if determinant(matrix) < 0. { -1. } else { 1. };
    let (eigenvalues, _) = jacobi(mul(matrix, &transpose(matrix)));
    // GfTransform consumes Factor's outputs even when it reports singularity.
    // Factor clamps a tiny eigenvalue's scale to epsilon, not sqrt(epsilon).
    let scale = eigenvalues.map(|v| sign * if v < 1e-10 { 1e-10 } else { libm::sqrt(v) });
    scale.iter().all(|v| v.is_finite()).then_some(scale)
}
/// Quaternion in `[real, x, y, z]` order, as `GfQuatd`.
pub(crate) fn quaternion(m: &Matrix3) -> [f64; 4] {
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
        q[0] = 0.5 * libm::sqrt(trace + 1.);
        q[1] = (m[1][2] - m[2][1]) / (4. * q[0]);
        q[2] = (m[2][0] - m[0][2]) / (4. * q[0]);
        q[3] = (m[0][1] - m[1][0]) / (4. * q[0]);
    } else {
        let j = (i + 1) % 3;
        let k = (i + 2) % 3;
        q[i + 1] = 0.5 * libm::sqrt(m[i][i] - m[j][j] - m[k][k] + 1.);
        q[j + 1] = (m[i][j] + m[j][i]) / (4. * q[i + 1]);
        q[k + 1] = (m[k][i] + m[i][k]) / (4. * q[i + 1]);
        q[0] = (m[j][k] - m[k][j]) / (4. * q[i + 1]);
    }
    q[0] = q[0].clamp(-1., 1.);
    q
}
