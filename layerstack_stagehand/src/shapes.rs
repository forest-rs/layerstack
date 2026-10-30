// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `UsdGeom` implicit shapes as triangles: cube, sphere, cylinder,
//! cone, capsule and plane, and the `_1` revisions of the cylinder and
//! capsule with a radius per end, at an explicit [`Tessellation`].
//!
//! Each shape is centered on the origin and sized as its schema says
//! (OpenUSD `pxr/usd/usdGeom/schema.usda`): a `Cube` of edge `size`; a
//! `Sphere` of `radius`; a `Cylinder` and a `Cone` of `radius` and
//! `height` along `axis` (the cone's apex at `+height / 2`); a `Capsule`
//! whose cylinder of `height` is capped by hemispheres of `radius`
//! (`height + 2 * radius` long); a `Plane` of `width` and `length` facing
//! `+axis`. A `Cylinder_1` runs from `radiusBottom` at `-height / 2` to
//! `radiusTop` at `+height / 2`; a `Capsule_1` caps such a slanted side with
//! spheres that meet it tangentially, centered inward of the ends by
//! `slope * radius` with radius `radius * sqrt(1 + slope²)`, `slope` being
//! `(radiusBottom - radiusTop) / height` (OpenUSD 26.11
//! `usdGeom/capsule_1.cpp`, `_ComputeExtentMax`, which bounds the same
//! spheres). Triangles wind counter-clockwise seen from outside; normals
//! are smooth on curved surfaces and flat on caps and cube faces.

use alloc::vec;
use alloc::vec::Vec;
use core::f32::consts::{PI, TAU};

use crate::plan::{DrawRange, MeshBuffers, small};

/// An implicit shape's spine or facing axis (`UsdGeom` `axis`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Axis {
    /// `X`.
    X,
    /// `Y`.
    Y,
    /// `Z`: the schemas' fallback.
    #[default]
    Z,
}

impl Axis {
    /// The axis a token names (`X`, `Y`, `Z`); `None` for another token.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "X" => Self::X,
            "Y" => Self::Y,
            "Z" => Self::Z,
            _ => return None,
        })
    }

    /// A shape built along `+Z`, turned to run along this axis: a cyclic
    /// permutation of the coordinates, so windings are kept.
    fn place(self, [x, y, z]: [f32; 3]) -> [f32; 3] {
        match self {
            Self::X => [z, x, y],
            Self::Y => [y, z, x],
            Self::Z => [x, y, z],
        }
    }
}

/// An implicit shape and its schema's size attributes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// `Cube`: `size`, the edge length (fallback 2).
    Cube {
        /// The edge length.
        size: f32,
    },
    /// `Sphere`: `radius` (fallback 1).
    Sphere {
        /// The radius.
        radius: f32,
    },
    /// `Cylinder`: `radius` (fallback 1), `height` (fallback 2), `axis`.
    Cylinder {
        /// The radius.
        radius: f32,
        /// The length along `axis`.
        height: f32,
        /// The spine.
        axis: Axis,
    },
    /// `Cone`: `radius` of its base (fallback 1), `height` (fallback 2),
    /// `axis`; the apex points to `+axis`.
    Cone {
        /// The base's radius.
        radius: f32,
        /// The length along `axis`.
        height: f32,
        /// The spine.
        axis: Axis,
    },
    /// `Capsule`: `radius` (fallback 0.5), `height` of the cylinder
    /// between its hemispherical caps (fallback 1), `axis`.
    Capsule {
        /// The radius of the cylinder and caps.
        radius: f32,
        /// The cylinder's length, without the caps.
        height: f32,
        /// The spine.
        axis: Axis,
    },
    /// `Cylinder_1`: `radiusTop` and `radiusBottom` (fallbacks 1), `height`
    /// (fallback 2), `axis`: a cylinder whose ends may differ in radius, a
    /// cone's frustum.
    Cylinder1 {
        /// The radius of the end at `+axis`.
        radius_top: f32,
        /// The radius of the end at `-axis`.
        radius_bottom: f32,
        /// The length along `axis`.
        height: f32,
        /// The spine.
        axis: Axis,
    },
    /// `Capsule_1`: `radiusTop` and `radiusBottom` (fallbacks 0.5) of its
    /// capping spheres, `height` (fallback 1), `axis`. With equal radii it
    /// is a [`Shape::Capsule`].
    Capsule1 {
        /// The radius at the top end (`+axis`).
        radius_top: f32,
        /// The radius at the bottom end (`-axis`).
        radius_bottom: f32,
        /// The length of the side between the caps' rims, along `axis`.
        height: f32,
        /// The spine.
        axis: Axis,
    },
    /// `Plane`: `width` and `length` (fallbacks 2), facing `+axis`. Width
    /// runs along `x` (along `z` for axis `X`), length along `y` (along
    /// `z` for axis `Y`).
    Plane {
        /// The extent along the width direction.
        width: f32,
        /// The extent along the length direction.
        length: f32,
        /// The facing axis.
        axis: Axis,
    },
}

/// How finely curved shapes are cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tessellation {
    /// Segments around the axis (at least 3). Default 32.
    pub segments: u32,
    /// Bands from pole to pole of a sphere, and of a capsule's two caps
    /// together (at least 2). Default 16.
    pub rings: u32,
}

impl Default for Tessellation {
    fn default() -> Self {
        Self {
            segments: 32,
            rings: 16,
        }
    }
}

/// The triangles of `shape` at `tessellation`, as [`MeshBuffers`] with
/// one draw range and no `st` or colors. The counts:
///
/// | shape | vertices | triangles |
/// |---|---|---|
/// | cube | 24 | 12 |
/// | plane | 4 | 2 |
/// | sphere | `(rings + 1) * (segments + 1)` | `2 * segments * (rings - 1)` |
/// | cylinder, `Cylinder_1` | `4 * (segments + 1) + 2` | `4 * segments` |
/// | cone | `3 * (segments + 1) + 1` | `2 * segments` |
/// | capsule, `Capsule_1` | `(2 * (rings / 2) + 2) * (segments + 1)` | `2 * segments * (2 * (rings / 2))` |
///
/// A `Cylinder_1` with a zero radius draws neither that end's cap nor the
/// side's degenerate triangles there: `segments` fewer of each, as a
/// cone.
#[must_use]
pub fn tessellate(shape: &Shape, tessellation: &Tessellation) -> MeshBuffers {
    let segments = tessellation.segments.max(3);
    let rings = tessellation.rings.max(2);
    let mut out = Builder::default();
    match *shape {
        Shape::Cube { size } => out.cube(size / 2.0),
        Shape::Plane {
            width,
            length,
            axis,
        } => out.plane(width / 2.0, length / 2.0, axis),
        Shape::Sphere { radius } => {
            let rows: Vec<Row> = (0..=rings)
                .map(|i| Row {
                    polar: PI * i as f32 / rings as f32,
                    center: 0.0,
                    radius,
                })
                .collect();
            out.lathe(&rows, segments, Axis::Z);
        }
        Shape::Capsule {
            radius,
            height,
            axis,
        } => out.capsule(radius, radius, height, rings, segments, axis),
        Shape::Capsule1 {
            radius_top,
            radius_bottom,
            height,
            axis,
        } => out.capsule(radius_top, radius_bottom, height, rings, segments, axis),
        Shape::Cylinder1 {
            radius_top,
            radius_bottom,
            height,
            axis,
        } => {
            out.side(radius_bottom, radius_top, height, segments, axis);
            if radius_top != 0.0 {
                out.cap(radius_top, height / 2.0, segments, axis, 1.0);
            }
            if radius_bottom != 0.0 {
                out.cap(radius_bottom, -height / 2.0, segments, axis, -1.0);
            }
        }
        Shape::Cylinder {
            radius,
            height,
            axis,
        } => {
            out.side(radius, radius, height, segments, axis);
            out.cap(radius, height / 2.0, segments, axis, 1.0);
            out.cap(radius, -height / 2.0, segments, axis, -1.0);
        }
        Shape::Cone {
            radius,
            height,
            axis,
        } => {
            out.side(radius, 0.0, height, segments, axis);
            out.cap(radius, -height / 2.0, segments, axis, -1.0);
        }
    }
    let count = small(out.indices.len());
    MeshBuffers {
        uv_sets: Vec::new(),
        triangle_faces: vec![0; out.indices.len() / 3],
        positions: out.positions,
        normals: out.normals,
        st: None,
        colors: None,
        indices: out.indices,
        ranges: vec![DrawRange {
            subset: None,
            first: 0,
            count,
        }],
    }
}

/// One circle of latitude of a lathed surface: its polar angle from `+Z`
/// on a sphere of `radius` centered at `center` on the axis.
struct Row {
    polar: f32,
    center: f32,
    radius: f32,
}

#[derive(Default)]
struct Builder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

impl Builder {
    fn vertex(&mut self, axis: Axis, position: [f32; 3], normal: [f32; 3]) -> u32 {
        self.positions.push(axis.place(position));
        self.normals.push(axis.place(normal));
        small(self.positions.len()) - 1
    }

    fn triangle(&mut self, a: u32, b: u32, c: u32) {
        self.indices.extend([a, b, c]);
    }

    fn cube(&mut self, h: f32) {
        // Each face: the axis it faces, its sign, and two axes spanning it
        // counter-clockwise seen from outside.
        for (axis, sign) in [
            (0, 1.0),
            (0, -1.0),
            (1, 1.0),
            (1, -1.0),
            (2, 1.0),
            (2, -1.0),
        ] {
            let (u, v) = if sign > 0.0 {
                ((axis + 1) % 3, (axis + 2) % 3)
            } else {
                ((axis + 2) % 3, (axis + 1) % 3)
            };
            let mut normal = [0.0; 3];
            normal[axis] = sign;
            let base = small(self.positions.len());
            for (du, dv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let mut p = [0.0; 3];
                p[axis] = sign * h;
                p[u] = du * h;
                p[v] = dv * h;
                self.vertex(Axis::Z, p, normal);
            }
            self.triangle(base, base + 1, base + 2);
            self.triangle(base, base + 2, base + 3);
        }
    }

    fn plane(&mut self, half_width: f32, half_length: f32, axis: Axis) {
        // The width and length directions in the frame where `axis` is +Z:
        // `Axis::place` maps the frame's (x, y, z) to (z, x, y) for X and to
        // (y, z, x) for Y, so the schema's width (world Z for X, world X
        // for Y) is the frame's y, and its length (world Y for X, world Z
        // for Y) is the frame's x.
        let (w, l) = match axis {
            Axis::X | Axis::Y => ([0.0, half_width, 0.0], [half_length, 0.0, 0.0]),
            Axis::Z => ([half_width, 0.0, 0.0], [0.0, half_length, 0.0]),
        };
        let base = small(self.positions.len());
        for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = [a * w[0] + b * l[0], a * w[1] + b * l[1], 0.0];
            self.vertex(axis, p, [0.0, 0.0, 1.0]);
        }
        // Counter-clockwise about +Z in the frame when the width and
        // length directions form a right-handed pair with it; else flipped.
        let w_cross_l = w[0] * l[1] - w[1] * l[0];
        if w_cross_l > 0.0 {
            self.triangle(base, base + 1, base + 2);
            self.triangle(base, base + 2, base + 3);
        } else {
            self.triangle(base, base + 2, base + 1);
            self.triangle(base, base + 3, base + 2);
        }
    }

    /// A capsule of `height` along `+Z` whose capping spheres meet a
    /// side running from `bottom` to `top` tangentially: rows from the top
    /// pole down to the top rim, then from the bottom rim to the bottom
    /// pole, `rings / 2` bands per cap; the side is the band between the
    /// rims.
    fn capsule(
        &mut self,
        top: f32,
        bottom: f32,
        height: f32,
        rings: u32,
        segments: u32,
        axis: Axis,
    ) {
        let half = (rings / 2).max(1);
        let slope = if height > 0.0 {
            (bottom - top) / height
        } else {
            0.0
        };
        let secant = libm::sqrtf(1.0 + slope * slope);
        // The rims sit at the polar angle where a sphere's normal leans as
        // the side's does: `atan2(1, slope)`, a right angle when straight.
        let rim = libm::atan2f(1.0, slope);
        let mut rows = Vec::new();
        rows.extend((0..=half).map(|i| Row {
            polar: rim * i as f32 / half as f32,
            center: height / 2.0 - slope * top,
            radius: top * secant,
        }));
        rows.extend((0..=half).map(|i| Row {
            polar: rim + (PI - rim) * i as f32 / half as f32,
            center: -height / 2.0 - slope * bottom,
            radius: bottom * secant,
        }));
        self.lathe(&rows, segments, axis);
    }

    /// A surface of revolution about `+Z` through rows, each a circle of
    /// latitude of a sphere; rows at a pole get one triangle per segment.
    fn lathe(&mut self, rows: &[Row], segments: u32, axis: Axis) {
        let base = small(self.positions.len());
        for row in rows {
            let (sin_t, cos_t) = (libm::sinf(row.polar), libm::cosf(row.polar));
            for j in 0..=segments {
                let phi = TAU * j as f32 / segments as f32;
                let n = [sin_t * libm::cosf(phi), sin_t * libm::sinf(phi), cos_t];
                self.vertex(
                    axis,
                    [
                        row.radius * n[0],
                        row.radius * n[1],
                        row.radius * n[2] + row.center,
                    ],
                    n,
                );
            }
        }
        let width = segments + 1;
        let last = rows.len() - 1;
        for i in 0..last {
            for j in 0..segments {
                let a = base + small(i) * width + j;
                let b = a + width;
                let (c, d) = (b + 1, a + 1);
                let north = rows[i].polar == 0.0;
                let south = i + 1 == last && (rows[last].polar - PI).abs() < 1e-6;
                if !south {
                    self.triangle(a, b, c);
                }
                if !north {
                    self.triangle(a, c, d);
                }
            }
        }
    }

    /// The side of a cylinder (`top == bottom`), cone (`top == 0`) or
    /// frustum of `height` along `+Z`, smooth around; a zero radius draws
    /// no triangle at that end.
    fn side(&mut self, bottom: f32, top: f32, height: f32, segments: u32, axis: Axis) {
        let base = small(self.positions.len());
        // The side's normal leans by the slope (bottom - top) / height.
        let slope = (bottom - top) / height;
        for (radius, z) in [(bottom, -height / 2.0), (top, height / 2.0)] {
            for j in 0..=segments {
                // On a cone's apex row, each vertex takes the normal halfway
                // across its segment.
                let shift = if radius == 0.0 { 0.5 } else { 0.0 };
                let phi = TAU * (j as f32 + shift) / segments as f32;
                let (sin_p, cos_p) = (libm::sinf(phi), libm::cosf(phi));
                let n = [cos_p, sin_p, slope];
                let length = libm::sqrtf(1.0 + slope * slope);
                let phi_p = TAU * j as f32 / segments as f32;
                let p = [radius * libm::cosf(phi_p), radius * libm::sinf(phi_p), z];
                self.vertex(axis, p, n.map(|c| c / length));
            }
        }
        let width = segments + 1;
        for j in 0..segments {
            let (a, b) = (base + j, base + j + 1);
            let (c, d) = (b + width, a + width);
            if bottom != 0.0 {
                self.triangle(a, b, c);
            }
            if top != 0.0 {
                self.triangle(a, c, d);
            }
        }
    }

    /// A flat disc of `radius` at `z`, facing `sign * Z`.
    fn cap(&mut self, radius: f32, z: f32, segments: u32, axis: Axis, sign: f32) {
        let normal = [0.0, 0.0, sign];
        let center = self.vertex(axis, [0.0, 0.0, z], normal);
        for j in 0..=segments {
            let phi = TAU * j as f32 / segments as f32;
            self.vertex(
                axis,
                [radius * libm::cosf(phi), radius * libm::sinf(phi), z],
                normal,
            );
        }
        for j in 0..segments {
            let (a, b) = (center + 1 + j, center + 2 + j);
            if sign > 0.0 {
                self.triangle(center, a, b);
            } else {
                self.triangle(center, b, a);
            }
        }
    }
}
