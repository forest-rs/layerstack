// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{ColorError, ColorSpaceDefinition};
use alloc::sync::Arc;
/// Transfer curve parameters as OpenUSD nanocolor computes them.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Curve {
    gamma: f32,
    bias: f32,
    k0: f32,
    phi: f32,
}
impl Curve {
    pub(super) fn decode(&self, value: f32) -> f32 {
        if self.gamma == 1. {
            return value;
        }
        if value < self.k0 {
            value / self.phi
        } else {
            libm::powf((value + self.bias) / (1. + self.bias), self.gamma)
        }
    }
    pub(super) fn encode(&self, value: f32) -> f32 {
        if self.gamma == 1. {
            return value;
        }
        if value < self.k0 / self.phi {
            value * self.phi
        } else {
            (1. + self.bias) * libm::powf(value, 1. / self.gamma) - self.bias
        }
    }
}
pub(super) fn transfer(gamma: f32, bias: f32) -> Result<Curve, ColorError> {
    if !gamma.is_finite()
        || gamma <= 0.
        || !bias.is_finite()
        || bias < 0.
        || (bias > 0. && gamma <= 1.)
    {
        return Err(ColorError::InvalidTransferFunction);
    }
    let (k0, phi) = if gamma == 1. {
        (1e9, 1.)
    } else if bias == 0. {
        (0., 1.)
    } else {
        let k0 = bias / (gamma - 1.);
        let phi = (bias
            / libm::expf(libm::logf(gamma * bias / (gamma + gamma * bias - 1. - bias)) * gamma))
            / (gamma - 1.);
        (k0, phi)
    };
    if !k0.is_finite() || !phi.is_finite() || phi <= 0. {
        return Err(ColorError::InvalidTransferFunction);
    }
    Ok(Curve {
        gamma,
        bias,
        k0,
        phi,
    })
}
pub(super) fn inverse(m: [[f32; 3]; 3]) -> Result<[[f32; 3]; 3], ColorError> {
    if m.iter().flatten().any(|v| !v.is_finite()) {
        return Err(ColorError::InvalidMatrix);
    }
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[1][0] * (m[0][1] * m[2][2] - m[0][2] * m[2][1])
        + m[2][0] * (m[0][1] * m[1][2] - m[0][2] * m[1][1]);
    if det == 0. || !det.is_finite() {
        return Err(ColorError::InvalidMatrix);
    }
    let d = 1. / det;
    let inv = [
        [
            (m[1][1] * m[2][2] - m[2][1] * m[1][2]) * d,
            (m[2][1] * m[0][2] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[1][1] * m[0][2]) * d,
        ],
        [
            (m[2][0] * m[1][2] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[2][0] * m[0][2]) * d,
            (m[1][0] * m[0][2] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[2][0] * m[1][1]) * d,
            (m[2][0] * m[0][1] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[1][0] * m[0][1]) * d,
        ],
    ];
    if inv.iter().flatten().any(|v| !v.is_finite()) {
        return Err(ColorError::InvalidMatrix);
    }
    Ok(inv)
}
pub(super) fn multiply(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    core::array::from_fn(|i| {
        core::array::from_fn(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j])
    })
}
pub(super) fn matrix(chroma: [[f32; 2]; 4]) -> Result<[[f32; 3]; 3], ColorError> {
    if chroma.iter().flatten().any(|v| !v.is_finite()) || chroma[3][1] == 0. {
        return Err(ColorError::InvalidChromaticities);
    }
    let [r, g, b, w] = chroma;
    let m = [
        [r[0], g[0], b[0]],
        [r[1], g[1], b[1]],
        [1. - r[0] - r[1], 1. - g[0] - g[1], 1. - b[0] - b[1]],
    ];
    let inverse = inverse(m).map_err(|_| ColorError::InvalidChromaticities)?;
    let white = [w[0] / w[1], 1., (1. - w[0] - w[1]) / w[1]];
    let scale: [f32; 3] = core::array::from_fn(|i| {
        inverse[i][0] * white[0] + inverse[i][1] * white[1] + inverse[i][2] * white[2]
    });
    let result = core::array::from_fn(|i| core::array::from_fn(|j| m[i][j] * scale[j]));
    inverse_check(result)?;
    Ok(result)
}
fn inverse_check(m: [[f32; 3]; 3]) -> Result<(), ColorError> {
    inverse(m)
        .map(|_| ())
        .map_err(|_| ColorError::InvalidChromaticities)
}
#[allow(
    clippy::excessive_precision,
    reason = "OpenUSD's Bradford-preadapted chromaticities rounded to float"
)]
pub(super) fn builtin(name: &str) -> Option<ColorSpaceDefinition> {
    let d65 = [0.3127, 0.3290];
    let ap1 = [
        [0.71319588766205, 0.29268891446333],
        [0.15950855654178, 0.83878851615096],
        [0.128672995285350, 0.043895571160528],
        d65,
    ];
    let ap0 = [
        [0.73485524337371, 0.26422532524554],
        [-0.0061709124786224, 1.0113149590212864],
        [0.015967559255041, -0.064235503128551],
        d65,
    ];
    let rec709 = [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060], d65];
    let p3 = [[0.6800, 0.3200], [0.2650, 0.6900], [0.1500, 0.0600], d65];
    let adobe = [[0.64, 0.33], [0.21, 0.71], [0.15, 0.06], d65];
    let (chroma, gamma, bias) = match name {
        "lin_ap1_scene" => (ap1, 1., 0.),
        "lin_ap0_scene" => (ap0, 1., 0.),
        "lin_rec709_scene" => (rec709, 1., 0.),
        "lin_p3d65_scene" => (p3, 1., 0.),
        "lin_rec2020_scene" => (
            [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046], d65],
            1.,
            0.,
        ),
        "lin_adobergb_scene" => (adobe, 1., 0.),
        "lin_ciexyzd65_scene" => ([[1., 0.], [0., 1.], [0., 0.], d65], 1., 0.),
        "srgb_rec709_scene" => (rec709, 2.4, 0.055),
        "g24_rec709_scene" => (rec709, 2.4, 0.),
        "g22_rec709_scene" => (rec709, 2.2, 0.),
        "g18_rec709_scene" => (rec709, 1.8, 0.),
        "srgb_ap1_scene" => (ap1, 2.4, 0.055),
        "g22_ap1_scene" => (ap1, 2.2, 0.),
        "srgb_p3d65_scene" => (p3, 2.4, 0.055),
        "g22_adobergb_scene" => (adobe, 2.2, 0.),
        "identity" | "data" | "raw" | "unknown" => {
            return Some(ColorSpaceDefinition {
                name: Arc::from(name),
                source: None,
                chromaticities: [[1., 0.], [0., 1.], [0., 0.], [1. / 3., 1. / 3.]],
                gamma: 1.,
                linear_bias: 0.,
                rgb_to_xyz: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            });
        }
        _ => return None,
    };
    ColorSpaceDefinition::from_chromaticities(name, chroma, gamma, bias).ok()
}
