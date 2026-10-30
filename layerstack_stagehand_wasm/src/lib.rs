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
    Axis, FaceSubsets, Interpolation, MeshBuffers, MeshInput, MeshOptions, MeshPlan, MeshTopology,
    Orientation, Primvar, Shape, SubdivisionScheme, Tessellation, Triangulation, tessellate,
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

/// The version of the exports the TypeScript wrapper speaks: 1.
pub const ABI: u32 = 1;

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

    /// Its face subsets, flat: subset `i` is `faces[ends[i - 1]..ends[i]]`.
    pub fn subsets(&mut self, faces: Vec<i32>, ends: Vec<u32>) {
        self.subset_faces = faces;
        self.subset_ends = ends;
    }

    /// Plans the mesh (`hydraFan` fans every face) and fills its buffers.
    #[must_use]
    pub fn build(self, hydra_fan: bool) -> StagehandMesh {
        let triangulation = if hydra_fan {
            Triangulation::HydraFan
        } else {
            Triangulation::Concave
        };
        let input = self.input();
        let plan = MeshPlan::new(&input, &MeshOptions { triangulation });
        let buffers = plan
            .buffers(&input)
            .expect("a plan fills from the input it was built from");
        StagehandMesh {
            plan: Some(plan),
            buffers,
        }
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
}

/// The triangles of an implicit shape: `kind` (`Cube`, `Sphere`,
/// `Cylinder`, `Cone`, `Capsule`, `Plane`) with its sizes (`size` for a
/// cube, `radius` for a sphere, `radius` and `height` for a cylinder, cone
/// or capsule, `width` and `length` for a plane, as `a` and `b`), its
/// `axis` token, and the segments around and rings from pole to pole.
///
/// # Errors
///
/// An unknown kind or axis.
#[wasm_bindgen(js_name = tessellateShape)]
pub fn tessellate_shape(
    kind: &str,
    a: f32,
    b: f32,
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
        _ => return Err(error("tessellate", "unknown shape")),
    };
    Ok(StagehandMesh {
        plan: None,
        buffers: tessellate(&shape, &Tessellation { segments, rings }),
    })
}
