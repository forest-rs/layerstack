// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The WebAssembly module of `@forest-rs/layerstack_stagehand`: the
//! mesh plans and implicit shapes of [`layerstack_stagehand`], over typed
//! arrays.
//!
//! A WebAssembly module cannot share Rust objects with another module, so
//! this one never sees a stage: the TypeScript wrapper reads a mesh's
//! topology, points, primvars and subsets through `@forest-rs/layerstack`
//! as typed arrays and hands them to a [`MeshBuilder`]. The builder copies
//! them in; [`MeshBuilder::build`] plans the mesh and fills its buffers
//! into a [`StagehandMesh`], which keeps the plan for
//! [`StagehandMesh::refill`]. Buffers leave as JS-owned typed arrays,
//! copied once; the report leaves as JSON text.
//!
//! The exports are an ABI for the TypeScript wrapper, which is the API;
//! [`ABI`] is its version.
#![no_std]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod report;

use alloc::string::String;
use alloc::vec::Vec;

use layerstack_stagehand::{
    Axis, CardGeometry, CurveBuffers, CurveForm, CurveOptions, CurveStyle, CurvesInput, CurvesPlan,
    DrawMode, FaceSubsets, Interpolation, MeshBuffers, MeshInput, MeshOptions, MeshPlan,
    MeshTopology, Orientation, PointShape, Primvar, Shape, SmoothNormals, StandinInput,
    SubdivisionScheme, Tessellation, Triangulation, UvSetInput, draw_mode_standin, point_shape,
    tessellate,
};
use wasm_bindgen::prelude::*;

/// The allocator and panic handler of a module built without `std`.
#[cfg(all(feature = "standalone", not(feature = "std"), target_arch = "wasm32"))]
mod standalone {
    /// The allocator `std` uses on `wasm32-unknown-unknown`.
    #[global_allocator]
    static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

    /// Traps, as `std`'s panic runtime does on this target.
    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
        core::arch::wasm32::unreachable()
    }
}

/// The version of the exports the TypeScript wrapper speaks: 6.
pub const ABI: u32 = 6;

/// [`ABI`], which the wrapper checks before using the module.
#[wasm_bindgen(js_name = stagehandAbi)]
#[must_use]
pub fn stagehand_abi() -> u32 {
    ABI
}

fn error(operation: &str, message: &str) -> JsValue {
    let mut text = String::from(operation);
    text.push_str(": ");
    text.push_str(message);
    JsValue::from_str(&text)
}

/// A primvar's arrays, copied in.
#[derive(Debug, Default)]
struct OwnedPrimvar<const N: usize> {
    values: Vec<[f32; N]>,
    indices: Option<Vec<i32>>,
    interpolation: Option<Interpolation>,
    element_size: u32,
}

impl<const N: usize> OwnedPrimvar<N> {
    fn new(
        operation: &str,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
        element_size: u32,
    ) -> Result<Self, JsValue> {
        let interpolation = Interpolation::from_token(interpolation)
            .ok_or_else(|| error(operation, "unknown interpolation"))?;
        let (values, rest) = values.as_chunks::<N>();
        if !rest.is_empty() {
            return Err(error(operation, "the values are not whole elements"));
        }
        Ok(Self {
            values: values.to_vec(),
            indices,
            interpolation: Some(interpolation),
            element_size,
        })
    }

    fn primvar(&self) -> Option<Primvar<'_, [f32; N]>> {
        Some(Primvar {
            values: &self.values,
            indices: self.indices.as_deref(),
            interpolation: self.interpolation?,
            element_size: self.element_size,
        })
    }
}

/// Points as triples, or an error naming `operation`.
fn triples(operation: &str, points: &[f32]) -> Result<Vec<[f32; 3]>, JsValue> {
    let (points, rest) = points.as_chunks::<3>();
    if !rest.is_empty() {
        return Err(error(operation, "the points are not whole triples"));
    }
    Ok(points.to_vec())
}

/// A mesh's arrays, gathered call by call, then planned.
#[wasm_bindgen]
#[derive(Debug, Default)]
pub struct MeshBuilder {
    counts: Vec<i32>,
    indices: Vec<i32>,
    holes: Vec<i32>,
    orientation: Orientation,
    scheme: SubdivisionScheme,
    points: Vec<[f32; 3]>,
    normals: Option<OwnedPrimvar<3>>,
    st: Option<OwnedPrimvar<2>>,
    color: Option<OwnedPrimvar<3>>,
    uv_sets: Vec<(String, OwnedPrimvar<2>)>,
    subset_faces: Vec<i32>,
    subset_ends: Vec<u32>,
}

#[wasm_bindgen]
impl MeshBuilder {
    /// A mesh of this topology (`orientation` and `subdivisionScheme` as
    /// their tokens) and points (three numbers each).
    ///
    /// # Errors
    ///
    /// When `points` is not whole triples.
    #[wasm_bindgen(constructor)]
    pub fn new(
        counts: Vec<i32>,
        indices: Vec<i32>,
        holes: Vec<i32>,
        orientation: &str,
        scheme: &str,
        points: &[f32],
    ) -> Result<Self, JsValue> {
        Ok(Self {
            counts,
            indices,
            holes,
            orientation: Orientation::from_token(orientation),
            scheme: SubdivisionScheme::from_token(scheme),
            points: triples("buildMesh", points)?,
            ..Self::default()
        })
    }

    /// Its normals.
    ///
    /// # Errors
    ///
    /// An unknown interpolation, or values that are not whole triples.
    pub fn normals(
        &mut self,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
        element_size: u32,
    ) -> Result<(), JsValue> {
        self.normals = Some(OwnedPrimvar::new(
            "buildMesh",
            values,
            indices,
            interpolation,
            element_size,
        )?);
        Ok(())
    }

    /// Its `st`.
    ///
    /// # Errors
    ///
    /// An unknown interpolation, or values that are not whole pairs.
    pub fn st(
        &mut self,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
        element_size: u32,
    ) -> Result<(), JsValue> {
        self.st = Some(OwnedPrimvar::new(
            "buildMesh",
            values,
            indices,
            interpolation,
            element_size,
        )?);
        Ok(())
    }

    /// Its `displayColor`.
    ///
    /// # Errors
    ///
    /// An unknown interpolation, or values that are not whole triples.
    #[wasm_bindgen(js_name = displayColor)]
    pub fn display_color(
        &mut self,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
        element_size: u32,
    ) -> Result<(), JsValue> {
        self.color = Some(OwnedPrimvar::new(
            "buildMesh",
            values,
            indices,
            interpolation,
            element_size,
        )?);
        Ok(())
    }

    /// A texture coordinate set besides `st`: the `float2` primvar `name`.
    ///
    /// # Errors
    ///
    /// An unknown interpolation, or values that are not whole pairs.
    #[wasm_bindgen(js_name = uvSet)]
    pub fn uv_set(
        &mut self,
        name: String,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
        element_size: u32,
    ) -> Result<(), JsValue> {
        let primvar = OwnedPrimvar::new("buildMesh", values, indices, interpolation, element_size)?;
        self.uv_sets.push((name, primvar));
        Ok(())
    }

    /// Its face subsets, flat: subset `i` is `faces[ends[i - 1]..ends[i]]`.
    pub fn subsets(&mut self, faces: Vec<i32>, ends: Vec<u32>) {
        self.subset_faces = faces;
        self.subset_ends = ends;
    }

    /// Plans the mesh (`hydra_fan` fans every face; `face_oriented`
    /// computes [`SmoothNormals::FaceOriented`] normals) and fills its
    /// buffers.
    #[must_use]
    pub fn build(self, hydra_fan: bool, face_oriented: bool) -> StagehandMesh {
        let options = MeshOptions {
            triangulation: if hydra_fan {
                Triangulation::HydraFan
            } else {
                Triangulation::Concave
            },
            normals: if face_oriented {
                SmoothNormals::FaceOriented
            } else {
                SmoothNormals::Hydra
            },
        };
        let sets = self.sets();
        let input = MeshInput {
            uv_sets: &sets,
            ..self.input()
        };
        let plan = MeshPlan::new(&input, &options);
        // Not `expect`: formatting its error would link `Debug` into the
        // module.
        let Ok(buffers) = plan.buffers(&input) else {
            unreachable!("a plan fills from the input it was built from")
        };
        StagehandMesh {
            plan: Some(plan),
            buffers,
        }
    }

    fn sets(&self) -> Vec<UvSetInput<'_>> {
        self.uv_sets
            .iter()
            .filter_map(|(name, set)| {
                Some(UvSetInput {
                    name,
                    primvar: set.primvar()?,
                })
            })
            .collect()
    }

    fn input(&self) -> MeshInput<'_> {
        MeshInput {
            topology: MeshTopology {
                face_vertex_counts: &self.counts,
                face_vertex_indices: &self.indices,
                hole_indices: &self.holes,
                orientation: self.orientation,
                subdivision_scheme: self.scheme,
            },
            points: &self.points,
            normals: self.normals.as_ref().and_then(OwnedPrimvar::primvar),
            st: self.st.as_ref().and_then(OwnedPrimvar::primvar),
            display_color: self.color.as_ref().and_then(OwnedPrimvar::primvar),
            uv_sets: &[],
            subsets: FaceSubsets {
                faces: &self.subset_faces,
                ends: &self.subset_ends,
            },
        }
    }
}

/// A planned mesh, or a tessellated shape, and its buffers.
#[wasm_bindgen]
#[derive(Debug)]
pub struct StagehandMesh {
    /// `None` for a shape, which has no plan to refill.
    plan: Option<MeshPlan>,
    buffers: MeshBuffers,
}

#[wasm_bindgen]
impl StagehandMesh {
    /// The positions, three numbers per vertex.
    #[must_use]
    pub fn positions(&self) -> Vec<f32> {
        self.buffers.positions.as_flattened().to_vec()
    }

    /// The normals, three numbers per vertex.
    #[must_use]
    pub fn normals(&self) -> Vec<f32> {
        self.buffers.normals.as_flattened().to_vec()
    }

    /// The `st`, two numbers per vertex; `undefined` when not drawn.
    #[must_use]
    pub fn st(&self) -> Option<Vec<f32>> {
        self.buffers
            .st
            .as_ref()
            .map(|st| st.as_flattened().to_vec())
    }

    /// The colors, three numbers per vertex; `undefined` when not drawn.
    #[must_use]
    pub fn colors(&self) -> Option<Vec<f32>> {
        self.buffers
            .colors
            .as_ref()
            .map(|colors| colors.as_flattened().to_vec())
    }

    /// The texture coordinate sets drawn besides `st`, by name, in order.
    #[wasm_bindgen(js_name = uvSetNames)]
    #[must_use]
    pub fn uv_set_names(&self) -> Vec<String> {
        self.buffers
            .uv_sets
            .iter()
            .map(|set| set.name.clone())
            .collect()
    }

    /// The texture coordinate set `index` of [`Self::uv_set_names`], two
    /// numbers per vertex; empty past the last.
    #[wasm_bindgen(js_name = uvSetValues)]
    #[must_use]
    pub fn uv_set_values(&self, index: u32) -> Vec<f32> {
        self.buffers
            .uv_sets
            .get(index as usize)
            .map(|set| set.values.as_flattened().to_vec())
            .unwrap_or_default()
    }

    /// Three vertex indices per triangle.
    #[must_use]
    pub fn indices(&self) -> Vec<u32> {
        self.buffers.indices.clone()
    }

    /// Each triangle's authored face.
    #[wasm_bindgen(js_name = triangleFaces)]
    #[must_use]
    pub fn triangle_faces(&self) -> Vec<u32> {
        self.buffers.triangle_faces.clone()
    }

    /// Each vertex's source point (`MeshPlan::vertex_points`); `undefined`
    /// for a shape.
    #[wasm_bindgen(js_name = vertexPoints)]
    #[must_use]
    pub fn vertex_points(&self) -> Option<Vec<u32>> {
        self.plan.as_ref().map(|plan| plan.vertex_points().to_vec())
    }

    /// The draw ranges, three numbers each: the subset (`-1` for the faces
    /// in no subset), the first index, the index count.
    #[must_use]
    pub fn ranges(&self) -> Vec<i32> {
        self.buffers
            .ranges
            .iter()
            .flat_map(|range| {
                [
                    range
                        .subset
                        .map_or(-1, |subset| i32::try_from(subset).unwrap_or(i32::MAX)),
                    i32::try_from(range.first).unwrap_or(i32::MAX),
                    i32::try_from(range.count).unwrap_or(i32::MAX),
                ]
            })
            .collect()
    }

    /// The plan's report as JSON; `null` for a shape.
    #[must_use]
    pub fn report(&self) -> Option<String> {
        self.plan.as_ref().map(|plan| report::json(plan.report()))
    }

    /// Rewrites the positions and normals for new `points`, and new
    /// authored `normals` when the plan draws them, without triangulating
    /// again.
    ///
    /// # Errors
    ///
    /// For a shape, or when the points or normals are not shaped as the
    /// plan's (`PlanMismatch`).
    pub fn refill(
        &mut self,
        points: &[f32],
        normals: Option<Vec<f32>>,
        normal_indices: Option<Vec<i32>>,
        normal_interpolation: Option<String>,
    ) -> Result<(), JsValue> {
        let plan = self
            .plan
            .as_ref()
            .ok_or_else(|| error("refill", "a shape has no plan"))?;
        let points = triples("refill", points)?;
        let normals = match (normals, normal_interpolation) {
            (Some(values), Some(interpolation)) => Some(OwnedPrimvar::<3>::new(
                "refill",
                &values,
                normal_indices,
                &interpolation,
                1,
            )?),
            _ => None,
        };
        let primvar = normals.as_ref().and_then(OwnedPrimvar::primvar);
        plan.refill(&mut self.buffers, &points, primvar.as_ref())
            .map_err(|mismatch| error("refill", &alloc::format!("{mismatch}")))
    }

    /// Fills every buffer again from the mesh `builder` gathered: new
    /// points, normals, `st` and colors, by the kept plan, without
    /// triangulating again. Consumes the builder.
    ///
    /// # Errors
    ///
    /// For a shape; when the topology differs from the plan's; or when the
    /// points or primvars are not shaped as the plan's (`PlanMismatch`).
    #[wasm_bindgen(js_name = refillFrom)]
    pub fn refill_from(&mut self, builder: MeshBuilder) -> Result<(), JsValue> {
        let plan = self
            .plan
            .as_ref()
            .ok_or_else(|| error("refillFrom", "a shape has no plan"))?;
        let sets = builder.sets();
        let input = MeshInput {
            uv_sets: &sets,
            ..builder.input()
        };
        if !plan.same_topology(&input.topology) {
            return Err(error("refillFrom", "the topology is not the plan's"));
        }
        self.buffers = plan
            .buffers(&input)
            .map_err(|mismatch| error("refillFrom", &alloc::format!("{mismatch}")))?;
        Ok(())
    }
}

/// The triangles of an implicit shape: `kind` (`Cube`, `Sphere`,
/// `Cylinder`, `Cone`, `Capsule`, `Plane`, `Cylinder_1`, `Capsule_1`) with
/// its sizes (`size` for a cube, `radius` for a sphere, `radius` and
/// `height` for a cylinder, cone or capsule, `width` and `length` for a
/// plane, `radiusTop`, `height` and `radiusBottom` for the `_1` kinds, as
/// `a`, `b` and `c`), its `axis` token, and the segments around and rings
/// from pole to pole.
///
/// # Errors
///
/// An unknown kind or axis.
#[wasm_bindgen(js_name = tessellateShape)]
pub fn tessellate_shape(
    kind: &str,
    a: f32,
    b: f32,
    c: f32,
    axis: &str,
    segments: u32,
    rings: u32,
) -> Result<StagehandMesh, JsValue> {
    let axis = Axis::from_token(axis).ok_or_else(|| error("tessellate", "unknown axis"))?;
    let shape = match kind {
        "Cube" => Shape::Cube { size: a },
        "Sphere" => Shape::Sphere { radius: a },
        "Cylinder" => Shape::Cylinder {
            radius: a,
            height: b,
            axis,
        },
        "Cone" => Shape::Cone {
            radius: a,
            height: b,
            axis,
        },
        "Capsule" => Shape::Capsule {
            radius: a,
            height: b,
            axis,
        },
        "Plane" => Shape::Plane {
            width: a,
            length: b,
            axis,
        },
        "Cylinder_1" => Shape::Cylinder1 {
            radius_top: a,
            height: b,
            radius_bottom: c,
            axis,
        },
        "Capsule_1" => Shape::Capsule1 {
            radius_top: a,
            height: b,
            radius_bottom: c,
            axis,
        },
        _ => return Err(error("tessellate", "unknown shape")),
    };
    Ok(StagehandMesh {
        plan: None,
        buffers: tessellate(&shape, &Tessellation { segments, rings }),
    })
}

/// A model's draw mode stand-in ([`draw_mode_standin`]), flattened: for
/// `origin` and `bounds`, the line segments' endpoints, three numbers
/// each; for `cards`, 22 numbers per card: its face (the index in
/// [`layerstack_stagehand::CardFace::ALL`]: `XPos`, `YPos`, `ZPos`, `XNeg`, `YNeg`, `ZNeg`), the
/// face whose texture it shows (`-1` for none), its four corners (twelve
/// numbers) and their texture coordinates (eight). `extent` is the
/// model's `[min, max]`, six numbers, or none; `textures` has bit `i` set
/// when face `i` has a texture; `hidden_axis` is `X`, `Y`, `Z` or empty.
///
/// # Errors
///
/// An unknown mode, or an extent of other than six numbers.
#[wasm_bindgen(js_name = drawModeStandin)]
pub fn draw_mode_standin_flat(
    mode: &str,
    extent: Option<Vec<f32>>,
    card_geometry: &str,
    textures: u32,
    hidden_axis: &str,
) -> Result<Vec<f32>, JsValue> {
    let mode =
        DrawMode::from_token(mode).ok_or_else(|| error("drawModeStandin", "unknown mode"))?;
    let extent = match extent.as_deref() {
        None => None,
        Some([a, b, c, d, e, f]) => Some([[*a, *b, *c], [*d, *e, *f]]),
        Some(_) => return Err(error("drawModeStandin", "an extent is six numbers")),
    };
    let standin = draw_mode_standin(&StandinInput {
        mode,
        extent,
        card_geometry: CardGeometry::from_token(card_geometry),
        textures: core::array::from_fn(|i| textures & (1 << i) != 0),
        hidden_axis: Axis::from_token(hidden_axis),
    });
    let mut flat = standin.lines.as_flattened().to_vec();
    for card in &standin.cards {
        #[expect(clippy::cast_precision_loss, reason = "a face index, below 6")]
        flat.push(card.face.index() as f32);
        #[expect(clippy::cast_precision_loss, reason = "a face index, below 6")]
        flat.push(card.texture.map_or(-1.0, |face| face.index() as f32));
        flat.extend_from_slice(card.corners.as_flattened());
        flat.extend_from_slice(card.uvs.as_flattened());
    }
    Ok(flat)
}

/// A primvar of one float per element, copied in.
#[derive(Debug)]
struct OwnedScalars {
    values: Vec<f32>,
    indices: Option<Vec<i32>>,
    interpolation: Interpolation,
}

impl OwnedScalars {
    fn new(
        operation: &str,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
    ) -> Result<Self, JsValue> {
        Ok(Self {
            values: values.to_vec(),
            indices,
            interpolation: Interpolation::from_token(interpolation)
                .ok_or_else(|| error(operation, "unknown interpolation"))?,
        })
    }

    fn primvar(&self) -> Primvar<'_, f32> {
        Primvar {
            values: &self.values,
            indices: self.indices.as_deref(),
            interpolation: self.interpolation,
            element_size: 1,
        }
    }
}

/// A curves prim's arrays, gathered call by call, then laid out.
#[wasm_bindgen]
#[derive(Debug, Default)]
pub struct CurvesBuilder {
    form: String,
    tokens: [String; 3],
    counts: Vec<i32>,
    points: Vec<[f32; 3]>,
    order: Vec<i32>,
    knots: Vec<f64>,
    ranges: Vec<[f64; 2]>,
    weights: Vec<f64>,
    tangents: Vec<[f32; 3]>,
    widths: Option<OwnedScalars>,
    normals: Option<OwnedPrimvar<3>>,
    colors: Option<OwnedPrimvar<3>>,
    opacities: Option<OwnedScalars>,
}

#[wasm_bindgen]
impl CurvesBuilder {
    /// Curves of `form` (`basis`, `nurbs` or `hermite`; for `basis`, the
    /// `type`, `basis` and `wrap` tokens), `curveVertexCounts` and points.
    ///
    /// # Errors
    ///
    /// An unknown form, or points that are not whole triples.
    #[wasm_bindgen(constructor)]
    pub fn new(
        form: String,
        curve_type: String,
        basis: String,
        wrap: String,
        counts: Vec<i32>,
        points: &[f32],
    ) -> Result<Self, JsValue> {
        if !matches!(form.as_str(), "basis" | "nurbs" | "hermite") {
            return Err(error("buildCurves", "unknown curve form"));
        }
        Ok(Self {
            form,
            tokens: [curve_type, basis, wrap],
            counts,
            points: triples("buildCurves", points)?,
            ..Self::default()
        })
    }

    /// A NURBS form's `order`, `knots`, `ranges` (pairs, flat) and
    /// `pointWeights`.
    ///
    /// # Errors
    ///
    /// When `ranges` is not whole pairs.
    pub fn nurbs(
        &mut self,
        order: Vec<i32>,
        knots: Vec<f64>,
        ranges: &[f64],
        weights: Vec<f64>,
    ) -> Result<(), JsValue> {
        let (pairs, rest) = ranges.as_chunks::<2>();
        if !rest.is_empty() {
            return Err(error("buildCurves", "the ranges are not whole pairs"));
        }
        self.order = order;
        self.knots = knots;
        self.ranges = pairs.to_vec();
        self.weights = weights;
        Ok(())
    }

    /// A Hermite form's tangents.
    ///
    /// # Errors
    ///
    /// When they are not whole triples.
    pub fn tangents(&mut self, tangents: &[f32]) -> Result<(), JsValue> {
        self.tangents = triples("buildCurves", tangents)?;
        Ok(())
    }

    /// Its widths.
    ///
    /// # Errors
    ///
    /// An unknown interpolation.
    pub fn widths(
        &mut self,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
    ) -> Result<(), JsValue> {
        self.widths = Some(OwnedScalars::new(
            "buildCurves",
            values,
            indices,
            interpolation,
        )?);
        Ok(())
    }

    /// Its normals.
    ///
    /// # Errors
    ///
    /// An unknown interpolation, or values that are not whole triples.
    pub fn normals(
        &mut self,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
    ) -> Result<(), JsValue> {
        self.normals = Some(OwnedPrimvar::new(
            "buildCurves",
            values,
            indices,
            interpolation,
            1,
        )?);
        Ok(())
    }

    /// Its `displayColor`.
    ///
    /// # Errors
    ///
    /// An unknown interpolation, or values that are not whole triples.
    #[wasm_bindgen(js_name = displayColor)]
    pub fn display_color(
        &mut self,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
    ) -> Result<(), JsValue> {
        self.colors = Some(OwnedPrimvar::new(
            "buildCurves",
            values,
            indices,
            interpolation,
            1,
        )?);
        Ok(())
    }

    /// Its `displayOpacity`.
    ///
    /// # Errors
    ///
    /// An unknown interpolation.
    #[wasm_bindgen(js_name = displayOpacity)]
    pub fn display_opacity(
        &mut self,
        values: &[f32],
        indices: Option<Vec<i32>>,
        interpolation: &str,
    ) -> Result<(), JsValue> {
        self.opacities = Some(OwnedScalars::new(
            "buildCurves",
            values,
            indices,
            interpolation,
        )?);
        Ok(())
    }

    /// Lays the curves out and fills their buffers: line segments, and
    /// with `swept` tubes of `sides` sides or ribbons; `samples` per
    /// segment.
    #[must_use]
    pub fn build(self, swept: bool, samples: u32, sides: u32) -> StagehandCurves {
        let options = CurveOptions {
            style: if swept {
                CurveStyle::Swept
            } else {
                CurveStyle::Lines
            },
            samples,
            sides,
        };
        let (plan, buffers) = self.with_input(|input| {
            let plan = CurvesPlan::new(input, &options);
            let Ok(buffers) = plan.buffers(input) else {
                unreachable!("a plan fills from the input it was built from")
            };
            (plan, buffers)
        });
        StagehandCurves { plan, buffers }
    }

    fn with_input<R>(&self, f: impl FnOnce(&CurvesInput<'_>) -> R) -> R {
        let input = CurvesInput {
            form: match self.form.as_str() {
                "nurbs" => CurveForm::Nurbs {
                    order: &self.order,
                    knots: &self.knots,
                    ranges: &self.ranges,
                    weights: &self.weights,
                },
                "hermite" => CurveForm::Hermite {
                    tangents: &self.tangents,
                },
                _ => CurveForm::Basis {
                    curve_type: &self.tokens[0],
                    basis: &self.tokens[1],
                    wrap: &self.tokens[2],
                },
            },
            vertex_counts: &self.counts,
            points: &self.points,
            widths: self.widths.as_ref().map(OwnedScalars::primvar),
            normals: self.normals.as_ref().and_then(OwnedPrimvar::primvar),
            colors: self.colors.as_ref().and_then(OwnedPrimvar::primvar),
            opacities: self.opacities.as_ref().map(OwnedScalars::primvar),
        };
        f(&input)
    }
}

/// Curves laid out and filled, keeping their plan for
/// [`Self::refill_from`].
#[wasm_bindgen]
#[derive(Debug)]
pub struct StagehandCurves {
    plan: CurvesPlan,
    buffers: CurveBuffers,
}

#[wasm_bindgen]
impl StagehandCurves {
    /// Each sample's position, three numbers each.
    #[must_use]
    pub fn positions(&self) -> Vec<f32> {
        self.buffers.positions.as_flattened().to_vec()
    }

    /// Each sample's width.
    #[must_use]
    pub fn widths(&self) -> Vec<f32> {
        self.buffers.widths.clone()
    }

    /// Each sample's normal, when drawn.
    #[must_use]
    pub fn normals(&self) -> Option<Vec<f32>> {
        self.buffers
            .normals
            .as_ref()
            .map(|values| values.as_flattened().to_vec())
    }

    /// Each sample's display color, when drawn.
    #[must_use]
    pub fn colors(&self) -> Option<Vec<f32>> {
        self.buffers
            .colors
            .as_ref()
            .map(|values| values.as_flattened().to_vec())
    }

    /// Each sample's display opacity, when drawn.
    #[must_use]
    pub fn opacities(&self) -> Option<Vec<f32>> {
        self.buffers.opacities.clone()
    }

    /// Two sample indices per line segment.
    #[must_use]
    pub fn lines(&self) -> Vec<u32> {
        self.buffers.lines.clone()
    }

    /// Each drawn curve's first sample and sample count.
    #[must_use]
    pub fn curves(&self) -> Vec<u32> {
        self.buffers.curves.as_flattened().to_vec()
    }

    /// The swept triangles as a mesh: empty when drawn as lines.
    #[must_use]
    pub fn swept(&self) -> StagehandMesh {
        StagehandMesh {
            plan: None,
            buffers: self.buffers.swept.clone().unwrap_or_default(),
        }
    }

    /// The report, as JSON, without its extent ([`Self::extent`]).
    #[must_use]
    pub fn report(&self) -> String {
        report::curves_json(&self.buffers.report)
    }

    /// `UsdGeomCurves::ComputeExtent` of the points and widths, as six
    /// numbers (minimum, then maximum); `None` for no points.
    #[must_use]
    pub fn extent(&self) -> Option<Vec<f32>> {
        self.buffers
            .report
            .extent
            .map(|extent| extent.as_flattened().to_vec())
    }

    /// Whether `builder` has these curves' topology, so
    /// [`Self::refill_from`] keeps the plan.
    #[wasm_bindgen(js_name = sameTopology)]
    #[must_use]
    pub fn same_topology(&self, builder: &CurvesBuilder) -> bool {
        builder.with_input(|input| self.plan.same_topology(input))
    }

    /// Fills the buffers again from `builder`, by the kept plan.
    ///
    /// # Errors
    ///
    /// When its topology is not the plan's.
    #[wasm_bindgen(js_name = refillFrom)]
    pub fn refill_from(&mut self, builder: &CurvesBuilder) -> Result<(), JsValue> {
        self.buffers = builder
            .with_input(|input| self.plan.buffers(input))
            .map_err(|_| error("refillFrom", "the topology is not the plan's"))?;
        Ok(())
    }
}

/// The unit shape points are instanced as (`spheres` or `discs`, at
/// `segments` and `rings`), or an empty mesh for `sprites`.
///
/// # Errors
///
/// An unknown kind.
#[wasm_bindgen(js_name = pointShape)]
pub fn point_shape_export(kind: &str, segments: u32, rings: u32) -> Result<StagehandMesh, JsValue> {
    let tessellation = Tessellation { segments, rings };
    let buffers = match kind {
        "spheres" => point_shape(PointShape::Spheres, &tessellation),
        "discs" => point_shape(PointShape::Discs, &tessellation),
        "sprites" => MeshBuffers::default(),
        _ => return Err(error("pointShape", "unknown point shape")),
    };
    Ok(StagehandMesh {
        plan: None,
        buffers,
    })
}
