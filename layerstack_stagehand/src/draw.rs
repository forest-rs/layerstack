// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The retained draw list: what a renderer draws from a stage, grouped
//! into instanced draws, kept up to date from change records.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use hashbrown::{HashMap, HashSet};
use layerstack::{Changes, PathId, Time};
use layerstack_schemas::point_instancer::{InstanceTransform, InstanceTransformOptions};
use layerstack_schemas::usd_geom::{
    Camera, GeomSubsetElementType, Imageable, ImageablePurpose, PointInstancer,
};
use layerstack_schemas::{
    BindingOptions, MaterialPurpose, PrimView, Scene, Visibility, XformCache, is_transform_property,
};

use crate::camera::{CAMERA_INPUTS, CameraItem};
use crate::input::MeshInput;
use crate::light::{
    InstanceId, LIGHT_INPUTS, LightId, LightInstance, LightItem, LightKind, read_light,
};
use crate::material::{MaterialItem, read_material};
use crate::plan::{MeshBuffers, MeshOptions, MeshPlan};
use crate::report::MeshReport;
use crate::shapes::{Shape, Tessellation, tessellate};
use crate::stage::{StageMesh, read_shape};

/// Which `UsdGeomImageable` purposes a draw list draws.
///
/// The default draws `default` and `proxy`, as usdview shows a stage by
/// default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one switch per purpose the schema lists"
)]
pub struct Purposes {
    /// `default`.
    pub default: bool,
    /// `render`.
    pub render: bool,
    /// `proxy`.
    pub proxy: bool,
    /// `guide`.
    pub guide: bool,
}

impl Default for Purposes {
    fn default() -> Self {
        Self {
            default: true,
            render: false,
            proxy: true,
            guide: false,
        }
    }
}

impl Purposes {
    /// Whether `purpose` is drawn; a purpose the schema does not list is
    /// not.
    #[must_use]
    pub fn draws(&self, purpose: &ImageablePurpose) -> bool {
        match purpose {
            ImageablePurpose::Default => self.default,
            ImageablePurpose::Render => self.render,
            ImageablePurpose::Proxy => self.proxy,
            ImageablePurpose::Guide => self.guide,
            _ => false,
        }
    }
}

/// How a [`DrawList`] reads a stage. The default reads the default time
/// with the default purposes, mesh options and tessellation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawOptions {
    /// The time read.
    pub time: Time,
    /// The purposes drawn.
    pub purposes: Purposes,
    /// How meshes are planned.
    pub mesh: MeshOptions,
    /// How implicit shapes are cut.
    pub tessellation: Tessellation,
    /// The render contexts whose material outputs are read, in order
    /// (`["mtlx"]` reads `outputs:mtlx:surface` first); the universal
    /// `outputs:surface` is read after them, or alone when empty
    /// (`UsdShadeMaterial::ComputeSurfaceSource`).
    pub render_contexts: &'static [&'static str],
}

impl Default for DrawOptions {
    fn default() -> Self {
        Self {
            time: Time::Default,
            purposes: Purposes::default(),
            mesh: MeshOptions::default(),
            tessellation: Tessellation::default(),
            render_contexts: &[],
        }
    }
}

/// An item's identity in a [`DrawList`]: stable while the item exists,
/// never reused for another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId(pub u32);

/// A retained buffer's identity in a [`DrawList`]: stable while any item
/// draws it, never reused for another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferKey(pub u32);

/// What an item draws, and why its instances are grouped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemSource {
    /// A gprim outside any instance, drawn once.
    Gprim {
        /// The gprim.
        path: PathId,
    },
    /// A gprim of a native instancing prototype, drawn once per instance
    /// that binds the same material: `prim` is the gprim in the
    /// prototype's first instance (`layerstack::Stage::prototypes`), and
    /// the item's [`DrawItem::instances`] are its instance proxies.
    /// Instances of one prototype bound to different materials are
    /// separate items, as Hydra splits them.
    Prototype {
        /// The gprim in the prototype's first instance.
        prim: PathId,
        /// The material its instances bind.
        material: Option<PathId>,
    },
    /// A gprim a `PointInstancer` places: one of its prototypes, or a
    /// gprim inside one, drawn once per instance of that prototype.
    PointInstancer {
        /// The instancer.
        instancer: PathId,
        /// The prototype's index in the instancer's `prototypes`.
        index: u32,
        /// The gprim: the prototype or a descendant of it.
        prim: PathId,
    },
}

/// One instanced draw: a retained buffer at one or more world matrices.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawItem {
    /// Its identity.
    pub id: ItemId,
    /// What it draws.
    pub source: ItemSource,
    /// The prim whose geometry, primvars and material are drawn.
    pub prim: PathId,
    /// The buffer drawn ([`DrawList::buffers`]).
    pub buffer: BufferKey,
    /// Each drawn instance's local-to-world matrix: row-vector, as
    /// OpenUSD's (`row[3]` is the translation). Empty when nothing of the
    /// item is drawn: it is [`Self::hidden`].
    pub matrices: Vec<[[f64; 4]; 4]>,
    /// Each drawn instance's prim, in step with [`Self::matrices`]: the
    /// gprim, or its instance proxies; empty for a point instancer.
    pub instances: Vec<PathId>,
    /// Each drawn instance's id (`ids`, else its position), in step with
    /// [`Self::matrices`], for a point instancer; empty otherwise.
    pub ids: Vec<i64>,
    /// The material bound to [`Self::prim`] (`preview` purpose, falling
    /// back to all-purpose bindings), if any.
    pub material: Option<PathId>,
    /// The material bound to each face subset, in the order of the
    /// buffer's [`DrawBuffers::subsets`].
    pub subset_materials: Vec<Option<PathId>>,
}

impl DrawItem {
    /// Whether nothing of it is drawn: invisible, of a purpose not drawn,
    /// or every instance masked. Its buffer is kept.
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.matrices.is_empty()
    }
}

/// A retained buffer: a mesh's planned buffers or a shape's tessellation,
/// owned by the [`DrawList`] and lent to the caller.
#[derive(Clone, Debug)]
pub struct DrawBuffers {
    /// Its key.
    pub key: BufferKey,
    /// The prim it was read from: the mesh, or the first prim drawn with
    /// this shape.
    pub prim: PathId,
    /// The buffers.
    pub buffers: MeshBuffers,
    /// The mesh's face subsets, in the order of their draw ranges.
    pub subsets: Vec<PathId>,
    /// Bumped each time the buffers are filled again or rebuilt.
    pub revision: u32,
    /// The shape tessellated, for a shape.
    pub shape: Option<Shape>,
    /// The mesh's plan, for a mesh.
    plan: Option<MeshPlan>,
    /// The mesh as last read, for a mesh.
    mesh: Option<StageMesh>,
    /// The texture coordinate sets besides `st` the materials its items
    /// bind read, sorted: those it was read with.
    uv_sets: Vec<String>,
    /// Items drawing it.
    users: u32,
}

impl DrawBuffers {
    /// What the plan made of the mesh; `None` for a shape.
    #[must_use]
    pub fn report(&self) -> Option<&MeshReport> {
        self.plan.as_ref().map(MeshPlan::report)
    }
}

/// The work a [`DrawList`] did for one diff: what it read, reused,
/// recomputed or rebuilt. A caller verifies incrementality from these
/// counts without reading the list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawWork {
    /// Prims visited discovering items under resynced paths.
    pub prims_traversed: u32,
    /// Items a change reached, whose inputs were read again.
    pub items_read: u32,
    /// Items no change reached: nothing of them was read.
    pub items_untouched: u32,
    /// Items [`DrawList::set_time`] did not read because nothing they read
    /// might differ at another time; counted in [`Self::items_untouched`]
    /// too.
    pub items_time_invariant: u32,
    /// Instance world matrices computed.
    pub matrices_computed: u32,
    /// Local transforms the transform cache computed (the rest it reused).
    pub local_transforms: u32,
    /// Meshes read again and found unchanged: their buffers reused.
    pub buffers_reused: u32,
    /// Buffers filled again by a kept plan, for new points, normals or
    /// primvar values: no triangulation.
    pub refills: u32,
    /// Meshes planned: faces triangulated and vertices split. A new mesh,
    /// or a new topology, subset or primvar layout.
    pub triangulations: u32,
    /// Implicit shapes tessellated.
    pub tessellations: u32,
    /// Cameras read: computed again.
    pub cameras_read: u32,
    /// Lights read again.
    pub lights_read: u32,
    /// Materials read: their networks captured and their records built.
    pub materials_read: u32,
}

/// What changed in a [`DrawList`] since the diff before, for a renderer to
/// apply instead of rebuilding: each list sorted, without duplicates.
///
/// An item in [`Self::added`] is in no other item list. A buffer's
/// changes are in the buffer lists: an item drawing a rebuilt or refilled
/// buffer is not listed for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrawDiff {
    /// New items.
    pub added: Vec<ItemId>,
    /// Items gone; their ids name nothing now.
    pub removed: Vec<ItemId>,
    /// Items whose matrices changed: moved, or instances shown or hidden.
    pub matrices: Vec<ItemId>,
    /// Items that became hidden or shown ([`DrawItem::hidden`]).
    pub visibility: Vec<ItemId>,
    /// Items whose bound materials changed.
    pub materials: Vec<ItemId>,
    /// Items now drawing another buffer: a shape resized to one another
    /// item already draws, or newly tessellated.
    pub rebound: Vec<ItemId>,
    /// New buffers.
    pub buffers_added: Vec<BufferKey>,
    /// Buffers filled again by their kept plan: same indices, ranges and
    /// vertex count; positions, normals, `st` or colors rewritten.
    pub refilled: Vec<BufferKey>,
    /// Buffers planned again: new indices, ranges and vertices; also when
    /// the texture coordinate sets its items' materials read change
    /// ([`crate::MeshBuffers::uv_sets`]).
    pub rebuilt: Vec<BufferKey>,
    /// Buffers no item draws any more: dropped.
    pub freed: Vec<BufferKey>,
    /// New cameras.
    pub cameras_added: Vec<PathId>,
    /// Cameras gone.
    pub cameras_removed: Vec<PathId>,
    /// Cameras read again and found different: moved, or a lens changed.
    pub cameras_changed: Vec<PathId>,
    /// New lights.
    pub lights_added: Vec<LightId>,
    /// Lights gone.
    pub lights_removed: Vec<LightId>,
    /// Lights read again and found different.
    pub lights_changed: Vec<LightId>,
    /// Materials newly bound by an item.
    pub materials_added: Vec<PathId>,
    /// Materials no item binds any more, or no longer materials.
    pub materials_removed: Vec<PathId>,
    /// Materials read again and found different: an input, a connection,
    /// a texture's file or sampling changed.
    pub materials_changed: Vec<PathId>,
    /// The work done.
    pub work: DrawWork,
}

impl DrawDiff {
    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.matrices.is_empty()
            && self.visibility.is_empty()
            && self.materials.is_empty()
            && self.rebound.is_empty()
            && self.buffers_added.is_empty()
            && self.refilled.is_empty()
            && self.rebuilt.is_empty()
            && self.freed.is_empty()
            && self.cameras_added.is_empty()
            && self.cameras_removed.is_empty()
            && self.cameras_changed.is_empty()
            && self.lights_added.is_empty()
            && self.lights_removed.is_empty()
            && self.lights_changed.is_empty()
            && self.materials_added.is_empty()
            && self.materials_removed.is_empty()
            && self.materials_changed.is_empty()
    }
}

/// What a change may have changed of an item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per input an item reads"
)]
struct Dirty {
    instances: bool,
    transform: bool,
    visibility: bool,
    geometry: bool,
    material: bool,
}

impl Dirty {
    const ALL: Self = Self {
        instances: true,
        transform: true,
        visibility: true,
        geometry: true,
        material: true,
    };

    fn merge(&mut self, other: Self) {
        self.instances |= other.instances;
        self.transform |= other.transform;
        self.visibility |= other.visibility;
        self.geometry |= other.geometry;
        self.material |= other.material;
    }

    fn any(self) -> bool {
        self.instances || self.transform || self.visibility || self.geometry || self.material
    }
}

/// Whether a camera or light reading `inputs` reads its own property
/// `name`: one of them, `treatAsPoint`, `treatAsLine`, `poleAxis`, or a
/// property of its link collections.
fn reads(inputs: &[&str], name: &str) -> bool {
    inputs.contains(&name)
        || matches!(name, "treatAsPoint" | "treatAsLine" | "poleAxis")
        || name.starts_with("collection:lightLink:")
        || name.starts_with("collection:shadowLink:")
}

/// Whether an info-only change of `prim` reaches nothing a draw list reads:
/// the pseudo-root's. It holds no properties, so its own fields are the
/// stage's metadata: a layer field of the stage's own layer stack that
/// composes nothing (`documentation`, `customLayerData`, `startTimeCode`,
/// `endTimeCode`, `metersPerUnit`, `upAxis` and the like), or the
/// `subLayers` of a sublayer inserted or removed, whose prims are listed on
/// their own (`layerstack::Changes::changed_info_only`). No buffer, matrix,
/// camera, light or material reads them: the time codes bound what a
/// caller animates, not a value read at a time; `metersPerUnit` and
/// `upAxis` scale and orient a whole scene for a renderer (a
/// `DomeLight_1`'s `poleAxis = "scene"` follows `upAxis`), which reads them
/// off the stage again when the pseudo-root changes. OpenUSD reports these
/// edits as an info change of the pseudo-root
/// (`UsdStage::_HandleLayersDidChange`).
fn stage_metadata(scene: &Scene<'_>, prim: PathId) -> bool {
    parent(scene, prim).is_none()
}

/// What a property's change may change of the items under its prim.
fn property_dirt(name: &str) -> Dirty {
    let mut dirty = Dirty::default();
    if is_transform_property(name) {
        dirty.transform = true;
    } else if name == "visibility" || name == "purpose" || name.ends_with("Visibility") {
        dirty.visibility = true;
    } else if name.starts_with("material:binding") {
        dirty.material = true;
    } else if GEOMETRY_INPUTS.contains(&name) || SUBSET_INPUTS.contains(&name) {
        dirty.geometry = true;
    }
    dirty
}

/// The attributes a mesh's or a shape's buffer is read from: points,
/// normals, topology, the drawn primvars and their indices, and a shape's
/// size attributes.
const GEOMETRY_INPUTS: &[&str] = &[
    "points",
    "normals",
    "faceVertexCounts",
    "faceVertexIndices",
    "holeIndices",
    "orientation",
    "subdivisionScheme",
    "size",
    "radius",
    "radiusTop",
    "radiusBottom",
    "height",
    "width",
    "length",
    "axis",
    "primvars:normals",
    "primvars:normals:indices",
    "primvars:st",
    "primvars:st:indices",
    "primvars:displayColor",
    "primvars:displayColor:indices",
];

/// A `GeomSubset`'s attributes, read by its mesh.
const SUBSET_INPUTS: &[&str] = &["indices", "elementType", "familyName"];

/// The attributes a `PointInstancer` places its instances from
/// (`UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`).
const INSTANCER_INPUTS: &[&str] = &[
    "protoIndices",
    "ids",
    "positions",
    "orientations",
    "orientationsf",
    "scales",
    "velocities",
    "accelerations",
    "angularVelocities",
    "invisibleIds",
];

/// Whether any of the prim's attributes `names` might hold another value
/// at another time, the default time included: time samples (even one), a
/// spline or value clips supply it.
/// OpenUSD: `UsdAttributeQuery::ValueSourceMightBeTimeVarying`.
fn inputs_might_vary(scene: &Scene<'_>, path: PathId, names: &[&str]) -> bool {
    scene.stage().prim(path, scene.store()).is_some_and(|prim| {
        names.iter().any(|name| {
            prim.attribute(*name)
                .is_some_and(layerstack::Attribute::value_source_might_be_time_varying)
        })
    })
}

/// Whether a property name is a shading port or a shader's identity:
/// what a material or a connected light input reads of a shading prim.
fn shading_property(name: &str) -> bool {
    name.starts_with("inputs:") || name.starts_with("outputs:") || name.starts_with("info:")
}

/// Whether `changes` reach the shading properties of the prim at `prim`:
/// a resync over it (one of `roots`), a shading property of it changed or
/// resynced, or an unlisted change of it or above it.
fn shading_touched(scene: &Scene<'_>, changes: &Changes, roots: &[PathId], prim: PathId) -> bool {
    let tokens = scene.store().tokens();
    roots.iter().any(|&root| under(scene, prim, root))
        || changes.changed_info_only.iter().any(|&changed| {
            if stage_metadata(scene, changed) {
                return false;
            }
            match changes
                .property_changes
                .iter()
                .find(|properties| properties.prim == changed)
            {
                None => under(scene, prim, changed),
                Some(properties) => {
                    changed == prim
                        && properties
                            .fields
                            .iter()
                            .any(|field| shading_property(tokens.resolve(field.name)))
                }
            }
        })
        || changes.resynced_properties.iter().any(|property| {
            property.prim_path() == prim && shading_property(tokens.resolve(property.property()))
        })
}

/// Whether any shading input or output of the prim might hold another
/// value at another time.
fn shading_might_vary(scene: &Scene<'_>, path: PathId) -> bool {
    scene.stage().prim(path, scene.store()).is_some_and(|prim| {
        prim.property_names().into_iter().any(|name| {
            shading_property(name)
                && prim
                    .attribute(name)
                    .is_some_and(layerstack::Attribute::value_source_might_be_time_varying)
        })
    })
}

/// Whether the prim's computed visibility might differ at another time:
/// its `visibility` or an ancestor's might. `purpose` is uniform.
fn visibility_might_vary(scene: &Scene<'_>, path: PathId) -> bool {
    let mut at = Some(path);
    while let Some(current) = at {
        if inputs_might_vary(scene, current, &["visibility"]) {
            return true;
        }
        at = parent(scene, current);
    }
    false
}

/// What of an item might differ at another time, as last read: what
/// [`DrawList::set_time`] reads again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Varies {
    /// Its matrices: a drawn instance's world transform, or an instancer's
    /// placement.
    matrices: bool,
    /// Whether it, or any instance, is shown.
    visibility: bool,
    /// Its buffer's inputs.
    geometry: bool,
}

/// The drawn gprim types: a mesh or an implicit shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gprim {
    Mesh,
    Shape,
}

fn gprim(scene: &Scene<'_>, path: PathId) -> Option<Gprim> {
    if scene.is_a(path, "Mesh") {
        return Some(Gprim::Mesh);
    }
    [
        "Cube",
        "Sphere",
        "Cylinder",
        "Cone",
        "Capsule",
        "Plane",
        "Cylinder_1",
        "Capsule_1",
    ]
    .iter()
    .any(|schema| scene.is_a(path, schema))
    .then_some(Gprim::Shape)
}

/// A shape's bits, for sharing one tessellation between equal shapes.
type ShapeKey = (u8, [u32; 4], u8);

fn shape_key(shape: &Shape) -> ShapeKey {
    use crate::shapes::Axis;
    let axis = |axis: Axis| match axis {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    };
    let bits = |values: [f32; 4]| values.map(f32::to_bits);
    match *shape {
        Shape::Cube { size } => (0, bits([size, 0.0, 0.0, 0.0]), 2),
        Shape::Sphere { radius } => (1, bits([radius, 0.0, 0.0, 0.0]), 2),
        Shape::Cylinder {
            radius,
            height,
            axis: a,
        } => (2, bits([radius, height, 0.0, 0.0]), axis(a)),
        Shape::Cone {
            radius,
            height,
            axis: a,
        } => (3, bits([radius, height, 0.0, 0.0]), axis(a)),
        Shape::Capsule {
            radius,
            height,
            axis: a,
        } => (4, bits([radius, height, 0.0, 0.0]), axis(a)),
        Shape::Plane {
            width,
            length,
            axis: a,
        } => (5, bits([width, length, 0.0, 0.0]), axis(a)),
        Shape::Cylinder1 {
            radius_top,
            radius_bottom,
            height,
            axis: a,
        } => (6, bits([radius_top, radius_bottom, height, 0.0]), axis(a)),
        Shape::Capsule1 {
            radius_top,
            radius_bottom,
            height,
            axis: a,
        } => (7, bits([radius_top, radius_bottom, height, 0.0]), axis(a)),
    }
}

/// An item and what the list keeps to refresh it.
#[derive(Clone, Debug)]
struct Entry {
    item: DrawItem,
    /// Every path whose change reaches the item: its instances (all of
    /// them, hidden ones included), its prim, and an instancer's
    /// prototype prims.
    depends: Vec<PathId>,
    /// What of it might differ at another time.
    varies: Varies,
}

/// The items and buffers a renderer draws from a stage, retained and kept
/// up to date from change records.
///
/// - **Items** ([`DrawItem`]) are instanced draws: a gprim outside any
///   instance is one item drawn once; the gprims of a native instancing
///   prototype are one item each, drawn at every instance's world matrix;
///   each prototype of a `PointInstancer` (and each gprim inside one) is
///   one item, drawn at every unmasked instance. Hidden items (invisible,
///   of a purpose not drawn, or with every instance masked) stay listed
///   with no matrices, their buffers kept for when they are shown again.
/// - **Buffers** ([`DrawBuffers`]) are owned by the list and lent out by
///   [`Self::buffers`]. A mesh has one buffer, planned once
///   ([`MeshPlan`]) and filled again when its points or primvar values
///   change; equal implicit shapes share one tessellation. A buffer lives
///   while an item draws it and is freed, and listed in
///   [`DrawDiff::freed`], in the diff that drops its last item.
/// - **Diffs** ([`DrawDiff`]): [`Self::refresh`] reads the whole stage;
///   [`Self::apply_changes`] reads only what a change record reaches;
///   [`Self::set_time`] reads at the new time only the items whose
///   inputs might vary with time, and rebuilds nothing that did not
///   change. Each lists what changed and counts the work ([`DrawWork`]).
///
/// What a change record reaches (`layerstack::Changes`, OpenUSD's
/// `UsdNotice::ObjectsChanged`):
///
/// - a resynced path: every item under it is read again, and items are
///   discovered under it or dropped;
/// - an info-only prim with a property inventory, and a resynced
///   property: by property name, `xformOp:*` and `xformOpOrder` recompute
///   the matrices of the items under the prim; `visibility`, `purpose` and
///   the purpose visibilities their visibility; `material:binding*` their
///   materials; `points`, `normals`, the topology, the drawn primvars
///   (`primvars:normals`, `primvars:st`, `primvars:displayColor`, and the
///   texture coordinate sets the items' materials read), a
///   shape's size attributes and a `GeomSubset`'s `indices` the geometry
///   of the items they belong to (a refill when the plan still fits, a new
///   plan when not). A point instancer's properties recompute its items'
///   instances. Other properties draw nothing and are ignored;
/// - an info-only prim without an inventory: every input of the items
///   under it is read again, and compared; but the pseudo-root's change
///   is the stage's metadata (`documentation`, `metersPerUnit`, `upAxis`,
///   the start and end time codes, or a sublayer's `subLayers`), which
///   nothing in the list reads: it reads nothing.
///
/// Cameras ([`CameraItem`]) and lights ([`LightItem`]) are listed beside
/// the items, by path (an instance proxy's inside a native instance, one
/// per instance) and, for a light a point instancer places, by instance
/// ([`LightId`]), and diffed by [`DrawDiff::cameras_changed`] and
/// [`DrawDiff::lights_changed`]: a change of an input it reads, of its
/// transform or visibility or one above it, or a resync over it, reads it
/// again; other properties of its prim (a lamp's `aim`) do not. Moving
/// one native instance reads its own lights and cameras again; a change of
/// a point instancer reads the lights it places. A light input connected
/// elsewhere is read again when a shading property of a prim its
/// connections reach changes.
///
/// Materials ([`MaterialItem`]) are held for the materials the items bind,
/// by path, and diffed by [`DrawDiff::materials_added`],
/// [`DrawDiff::materials_removed`] and [`DrawDiff::materials_changed`]: a
/// material is read when an item comes to bind it, and again when a
/// shading property (`inputs:*`, `outputs:*`, `info:*`) of it or of a
/// shader or node graph its network reads changes, or a resync covers one;
/// [`DrawWork::materials_read`] counts the reads. A time change reads only
/// those an input of which might vary.
///
/// Not read: collection-based material bindings changed through the
/// collection alone, and primvars a prototype's instances inherit from
/// above their instance roots (read from the first instance).
#[derive(Debug)]
pub struct DrawList {
    options: DrawOptions,
    xforms: XformCache,
    entries: BTreeMap<ItemId, Entry>,
    keys: HashMap<ItemSource, ItemId>,
    buffers: BTreeMap<BufferKey, DrawBuffers>,
    mesh_buffers: HashMap<PathId, BufferKey>,
    shape_buffers: HashMap<ShapeKey, BufferKey>,
    cameras: BTreeMap<PathId, Held<CameraItem>>,
    lights: BTreeMap<LightId, Held<LightItem>>,
    materials: BTreeMap<PathId, Held<MaterialItem>>,
    next_item: u32,
    next_buffer: u32,
}

/// A camera, a light or a material, whether what it reads might differ at
/// another time, and the other prims it reads (a light's connection
/// sources, a material's shaders and node graphs), as last read.
#[derive(Clone, Debug)]
struct Held<T> {
    item: T,
    varies: bool,
    sources: Vec<PathId>,
}

/// The diff being gathered, as sets.
#[derive(Default)]
struct Gather {
    added: BTreeSet<ItemId>,
    removed: BTreeSet<ItemId>,
    matrices: BTreeSet<ItemId>,
    visibility: BTreeSet<ItemId>,
    materials: BTreeSet<ItemId>,
    rebound: BTreeSet<ItemId>,
    buffers_added: BTreeSet<BufferKey>,
    refilled: BTreeSet<BufferKey>,
    rebuilt: BTreeSet<BufferKey>,
    freed: BTreeSet<BufferKey>,
    cameras: Changed<PathId>,
    lights: Changed<LightId>,
    shading: Changed<PathId>,
    /// Materials read in this diff, so none is read twice.
    materials_read: BTreeSet<PathId>,
    /// The materials the native instancing prototype groups this diff
    /// adds bind, by gprim: a new mesh buffer is planned once with the
    /// sets all of its groups read.
    groups: HashMap<PathId, Vec<PathId>>,
    work: DrawWork,
}

/// Cameras, lights or materials added, removed and changed in the diff
/// being gathered.
struct Changed<K> {
    added: BTreeSet<K>,
    removed: BTreeSet<K>,
    changed: BTreeSet<K>,
}

impl<K> Default for Changed<K> {
    fn default() -> Self {
        Self {
            added: BTreeSet::new(),
            removed: BTreeSet::new(),
            changed: BTreeSet::new(),
        }
    }
}

impl<K: Ord + Copy> Changed<K> {
    fn finish(mut self) -> [Vec<K>; 3] {
        for path in &self.added {
            self.changed.remove(path);
        }
        [self.added, self.removed, self.changed].map(|set| set.into_iter().collect())
    }
}

impl Gather {
    fn finish(mut self, local_before: usize, list: &DrawList, read: &HashSet<ItemId>) -> DrawDiff {
        // An added item is only added; a buffer added in this diff is not
        // also refilled or rebuilt.
        for id in &self.added {
            self.matrices.remove(id);
            self.visibility.remove(id);
            self.materials.remove(id);
            self.rebound.remove(id);
        }
        for key in &self.buffers_added {
            self.refilled.remove(key);
            self.rebuilt.remove(key);
        }
        for key in &self.rebuilt {
            self.refilled.remove(key);
        }
        self.work.local_transforms = count(
            list.xforms
                .stats()
                .local_computed
                .saturating_sub(local_before),
        );
        self.work.items_read = count(read.len());
        self.work.items_untouched =
            count(list.entries.keys().filter(|id| !read.contains(*id)).count());
        let [cameras_added, cameras_removed, cameras_changed] = self.cameras.finish();
        let [lights_added, lights_removed, lights_changed] = self.lights.finish();
        let [materials_added, materials_removed, materials_changed] = self.shading.finish();
        DrawDiff {
            added: self.added.into_iter().collect(),
            removed: self.removed.into_iter().collect(),
            matrices: self.matrices.into_iter().collect(),
            visibility: self.visibility.into_iter().collect(),
            materials: self.materials.into_iter().collect(),
            rebound: self.rebound.into_iter().collect(),
            buffers_added: self.buffers_added.into_iter().collect(),
            refilled: self.refilled.into_iter().collect(),
            rebuilt: self.rebuilt.into_iter().collect(),
            freed: self.freed.into_iter().collect(),
            cameras_added,
            cameras_removed,
            cameras_changed,
            lights_added,
            lights_removed,
            lights_changed,
            materials_added,
            materials_removed,
            materials_changed,
            work: self.work,
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "work counts saturate at `u32::MAX`"
)]
fn count(n: usize) -> u32 {
    n.min(u32::MAX as usize) as u32
}

/// What discovery covered and found: the items' sources, and the scopes
/// whose known items it would have found.
#[derive(Default)]
struct Found {
    sources: Vec<ItemSource>,
    scopes: Vec<Scope>,
    cameras: Vec<PathId>,
    lights: Vec<(LightId, LightKind)>,
    prims: u32,
}

/// What one discovery covers.
#[derive(Clone, Copy, Debug)]
enum Scope {
    /// Everything under a path, prototypes and instancers reached
    /// included as their own scopes.
    Resync(PathId),
    /// A native instancing prototype's items, by its first instance.
    Prototype(PathId),
    /// A point instancer's items.
    Instancer(PathId),
}

impl Scope {
    fn covers(self, scene: &Scene<'_>, source: &ItemSource) -> bool {
        match (self, *source) {
            (Self::Resync(root), ItemSource::Gprim { path }) => under(scene, path, root),
            (Self::Resync(root), ItemSource::PointInstancer { instancer, .. }) => {
                under(scene, instancer, root)
            }
            // A prototype whose first instance is gone is under the resync.
            (Self::Resync(root) | Self::Prototype(root), ItemSource::Prototype { prim, .. }) => {
                under(scene, prim, root)
            }
            (Self::Instancer(at), ItemSource::PointInstancer { instancer, .. }) => at == instancer,
            _ => false,
        }
    }
}

impl DrawList {
    /// An empty list reading the stage as `options` says; fill it with
    /// [`Self::refresh`].
    #[must_use]
    pub fn new(options: DrawOptions) -> Self {
        Self {
            xforms: XformCache::new(options.time),
            options,
            entries: BTreeMap::new(),
            keys: HashMap::new(),
            buffers: BTreeMap::new(),
            mesh_buffers: HashMap::new(),
            shape_buffers: HashMap::new(),
            cameras: BTreeMap::new(),
            lights: BTreeMap::new(),
            materials: BTreeMap::new(),
            next_item: 0,
            next_buffer: 0,
        }
    }

    /// The options it reads with; the time is the last one set.
    #[must_use]
    pub fn options(&self) -> &DrawOptions {
        &self.options
    }

    /// The items, in the order they were added.
    pub fn items(&self) -> impl Iterator<Item = &DrawItem> {
        self.entries.values().map(|entry| &entry.item)
    }

    /// The item `id`, while it exists.
    #[must_use]
    pub fn item(&self, id: ItemId) -> Option<&DrawItem> {
        self.entries.get(&id).map(|entry| &entry.item)
    }

    /// The number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it has no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The buffer `key`, while an item draws it. Borrowed from the list:
    /// copy or upload it before the next diff, which may refill, rebuild
    /// or free it.
    #[must_use]
    pub fn buffers(&self, key: BufferKey) -> Option<&DrawBuffers> {
        self.buffers.get(&key)
    }

    /// Every retained buffer, by key.
    pub fn all_buffers(&self) -> impl Iterator<Item = &DrawBuffers> {
        self.buffers.values()
    }

    /// The cameras, by path id.
    pub fn cameras(&self) -> impl Iterator<Item = &CameraItem> {
        self.cameras.values().map(|held| &held.item)
    }

    /// The camera at `path`, while it is listed.
    #[must_use]
    pub fn camera(&self, path: PathId) -> Option<&CameraItem> {
        self.cameras.get(&path).map(|held| &held.item)
    }

    /// The lights, by [`LightId`]: a light inside a native instance once
    /// per instance (by its instance proxy's path), one inside a point
    /// instancer's prototype once per unmasked instance.
    pub fn lights(&self) -> impl Iterator<Item = &LightItem> {
        self.lights.values().map(|held| &held.item)
    }

    /// The light `id` (a [`PathId`] for one no point instancer places),
    /// while it is listed.
    #[must_use]
    pub fn light(&self, id: impl Into<LightId>) -> Option<&LightItem> {
        self.lights.get(&id.into()).map(|held| &held.item)
    }

    /// The materials the items bind, by path id.
    pub fn materials(&self) -> impl Iterator<Item = &MaterialItem> {
        self.materials.values().map(|held| &held.item)
    }

    /// The material at `path`, while an item binds it.
    #[must_use]
    pub fn material(&self, path: PathId) -> Option<&MaterialItem> {
        self.materials.get(&path).map(|held| &held.item)
    }

    /// Reads the whole stage, as a resync of the pseudo-root: the first
    /// call adds every item; a later one reports what differs from what
    /// the list holds, reusing every buffer whose inputs are unchanged.
    pub fn refresh(&mut self, scene: &Scene<'_>) -> DrawDiff {
        self.xforms.clear();
        let root = scene.store().paths().intern(layerstack::Path::root());
        let mut changes = Changes::default();
        changes.resynced.push(root);
        self.apply_changes(scene, &changes)
    }

    /// Brings the list up to `scene`, which `changes` (from
    /// `LiveStage::apply` and the like) led to from the scene it last
    /// read: only the items the changes reach are read again. Pass every
    /// change record, in order.
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &Changes) -> DrawDiff {
        let local_before = self.xforms.stats().local_computed;
        self.xforms.apply_changes(scene, changes);
        let mut gather = Gather::default();
        let paths = scene.store().paths();
        let tokens = scene.store().tokens();
        let mut dirty: HashMap<ItemId, Dirty> = HashMap::new();
        let mut found = Found::default();
        let firsts = prototype_firsts(scene);

        // Resyncs: discover the items under each root, and read again every
        // item depending on a path under it.
        for &root in &changes.resynced {
            self.discover(scene, root, &firsts, &mut found);
            for (id, entry) in &self.entries {
                if entry.depends.iter().any(|&path| under(scene, path, root)) {
                    dirty.entry(*id).or_default().merge(Dirty::ALL);
                }
            }
        }
        // Prims and properties: what their names reach, under the prim.
        let mut reached: Vec<(PathId, Dirty)> = Vec::new();
        for &prim in &changes.changed_info_only {
            if stage_metadata(scene, prim) {
                continue;
            }
            let what = changes
                .property_changes
                .iter()
                .find(|changed| changed.prim == prim)
                .map_or(Dirty::ALL, |changed| {
                    let mut what = Dirty::default();
                    for field in &changed.fields {
                        what.merge(self.property_dirt(tokens.resolve(field.name)));
                    }
                    what
                });
            reached.push((prim, what));
        }
        for property in &changes.resynced_properties {
            reached.push((
                property.prim_path(),
                self.property_dirt(tokens.resolve(property.property())),
            ));
        }
        let mut prototypes: BTreeSet<PathId> = BTreeSet::new();
        for (prim, what) in reached {
            // A point instancer's own properties place its instances, and
            // may change its prototypes: discover its items again.
            let instancer = PointInstancer::get(scene, prim).is_some();
            if instancer {
                found.scopes.push(Scope::Instancer(prim));
                self.discover_instancer(scene, prim, &mut found);
            }
            // A `GeomSubset`'s properties reach its mesh.
            let subset_of = scene
                .is_a(prim, "GeomSubset")
                .then(|| paths.parent(prim))
                .flatten();
            for (id, entry) in &self.entries {
                let mut what = what;
                let mut hit = entry.depends.iter().any(|&path| under(scene, path, prim))
                    || subset_of == Some(entry.item.prim);
                if let ItemSource::PointInstancer { instancer: at, .. } = entry.item.source
                    && instancer
                    && at == prim
                {
                    what.instances = true;
                    hit = true;
                }
                if !hit || !what.any() {
                    continue;
                }
                // A binding may move a prototype's instance between its
                // material groups: discover the prototype's items again.
                if what.material
                    && let ItemSource::Prototype { prim, .. } = entry.item.source
                    && let Some(first) = instance_root(scene, prim)
                {
                    prototypes.insert(first);
                }
                dirty.entry(*id).or_default().merge(what);
            }
        }
        for first in prototypes {
            found.scopes.push(Scope::Prototype(first));
            self.discover_prototype(scene, first, false, &mut found);
        }
        gather.work.prims_traversed = found.prims;
        self.apply_to_views(scene, changes, &found, &mut gather);

        // Discovered items: new ones are added, known ones read again; known
        // items a discovery covers but did not find are gone. Added first,
        // so a buffer a gone item shares with a new one (a renamed prim's
        // shape) is kept rather than freed and made again.
        let roots: Vec<PathId> = found
            .scopes
            .iter()
            .filter_map(|scope| match scope {
                Scope::Resync(root) => Some(*root),
                _ => None,
            })
            .collect();
        let seen: HashSet<ItemSource> = found.sources.iter().copied().collect();
        let gone: Vec<ItemId> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                !seen.contains(&entry.item.source)
                    && found
                        .scopes
                        .iter()
                        .any(|scope| scope.covers(scene, &entry.item.source))
            })
            .map(|(id, _)| *id)
            .collect();
        for source in &found.sources {
            if let ItemSource::Prototype {
                prim,
                material: Some(material),
            } = *source
                && !self.keys.contains_key(source)
            {
                let materials = gather.groups.entry(prim).or_default();
                if !materials.contains(&material) {
                    materials.push(material);
                }
            }
        }
        let mut read: HashSet<ItemId> = HashSet::new();
        let mut added: HashSet<ItemSource> = HashSet::new();
        for source in found.sources {
            match self.keys.get(&source) {
                Some(&id) => dirty.entry(id).or_default().merge(Dirty::ALL),
                None if added.insert(source) => {
                    let id = ItemId(self.next_item);
                    self.next_item += 1;
                    read.insert(id);
                    if self.read_item(scene, id, source, Dirty::ALL, true, &mut gather) {
                        gather.added.insert(id);
                    }
                }
                None => {}
            }
        }
        for id in gone {
            dirty.remove(&id);
            self.remove(id, &mut gather);
        }
        let mut order: Vec<(ItemId, Dirty)> = dirty.into_iter().collect();
        order.sort_by_key(|(id, _)| *id);
        for (id, what) in order {
            let Some(entry) = self.entries.get(&id) else {
                continue;
            };
            let source = entry.item.source;
            read.insert(id);
            if !self.read_item(scene, id, source, what, false, &mut gather) {
                self.remove(id, &mut gather);
            }
        }
        read.retain(|id| self.entries.contains_key(id));
        self.apply_to_materials(scene, changes, &roots, &mut gather);
        gather.finish(local_before, self, &read)
    }

    /// Reads at `time` the items whose inputs might differ at another
    /// time, and only what of them might: matrices, visibility and
    /// geometry are compared with the time before, and only what differs
    /// is listed; a mesh whose points changed is refilled by its plan,
    /// never triangulated again unless its topology changed.
    ///
    /// An item's inputs might vary when time samples (even one), a spline
    /// or value clips supply them (`UsdAttributeQuery::
    /// ValueSourceMightBeTimeVarying`): its geometry's attributes (and its
    /// face subsets' `indices`), the `visibility` of it or an ancestor, an
    /// op of its world transform's chain
    /// ([`XformCache::local_to_world_might_vary`]), or a point
    /// instancer's placement attributes. The rest are left untouched and
    /// counted in [`DrawWork::items_time_invariant`]. What might vary is
    /// found as the item is read, so [`Self::apply_changes`] keeps it up to
    /// date: a time sample authored on a static input makes its item read
    /// at the next time.
    ///
    /// Cameras, lights and materials whose inputs might vary are read
    /// again too. A material's `UsdPrimvarReader_float2` `varname` (or the
    /// interface input it connects to) is one of them: when the sets the
    /// materials read differ at `time`, the meshes drawing them are
    /// planned again, listed in [`DrawDiff::rebuilt`].
    pub fn set_time(&mut self, scene: &Scene<'_>, time: Time) -> DrawDiff {
        let local_before = self.xforms.stats().local_computed;
        self.options.time = time;
        self.xforms.set_time(time);
        let mut gather = Gather::default();
        let ids: Vec<ItemId> = self.entries.keys().copied().collect();
        let mut read = HashSet::new();
        for id in ids {
            let entry = &self.entries[&id];
            let source = entry.item.source;
            let varies = entry.varies;
            let what = Dirty {
                instances: varies.matrices,
                transform: varies.matrices,
                visibility: varies.visibility,
                geometry: varies.geometry,
                material: false,
            };
            if !what.any() {
                gather.work.items_time_invariant += 1;
                continue;
            }
            read.insert(id);
            if !self.read_item(scene, id, source, what, false, &mut gather) {
                self.remove(id, &mut gather);
            }
        }
        let cameras: Vec<PathId> = self
            .cameras
            .iter()
            .filter(|(_, held)| held.varies)
            .map(|(path, _)| *path)
            .collect();
        for path in cameras {
            self.read_camera(scene, path, &mut gather);
        }
        let lights: Vec<(LightId, LightKind)> = self
            .lights
            .iter()
            .filter(|(_, held)| held.varies)
            .map(|(id, held)| (*id, held.item.kind))
            .collect();
        let mut placements = Placements::default();
        for (id, kind) in lights {
            self.read_light(scene, id, kind, &mut placements, &mut gather);
        }
        let materials: Vec<PathId> = self
            .materials
            .iter()
            .filter(|(_, held)| held.varies)
            .map(|(path, _)| *path)
            .collect();
        for path in materials {
            self.read_material(scene, path, &mut gather);
        }
        // A `varname` read at the new time may name other sets.
        self.apply_to_uv_sets(scene, &mut gather);
        read.retain(|id| self.entries.contains_key(id));
        gather.finish(local_before, self, &read)
    }

    /// Brings the cameras and lights up to `scene` after `changes`, given
    /// what discovery found: new ones are added and read, those a resync
    /// covers but did not find removed, and those a change reaches read
    /// again: a change of an input it reads, of its transform or
    /// visibility or those above it, an unlisted change of it or above it,
    /// or a resync over it.
    fn apply_to_views(
        &mut self,
        scene: &Scene<'_>,
        changes: &Changes,
        found: &Found,
        gather: &mut Gather,
    ) {
        let tokens = scene.store().tokens();
        let roots: Vec<PathId> = found
            .scopes
            .iter()
            .filter_map(|scope| match scope {
                Scope::Resync(root) => Some(*root),
                _ => None,
            })
            .collect();
        let resynced = |path: PathId| roots.iter().any(|&root| under(scene, path, root));
        // The properties changed on each prim; `None` when the record has no
        // inventory of them, which reaches everything under the prim.
        let mut reached: Vec<(PathId, Option<Vec<&str>>)> = Vec::new();
        for &prim in &changes.changed_info_only {
            if stage_metadata(scene, prim) {
                continue;
            }
            let names = changes
                .property_changes
                .iter()
                .find(|changed| changed.prim == prim)
                .map(|changed| {
                    changed
                        .fields
                        .iter()
                        .map(|field| tokens.resolve(field.name))
                        .collect()
                });
            reached.push((prim, names));
        }
        for property in &changes.resynced_properties {
            reached.push((
                property.prim_path(),
                Some(alloc::vec![tokens.resolve(property.property())]),
            ));
        }
        // A camera or light reads its own inputs, and the transform and
        // visibility of its prim and the prims above it.
        let reaches = |path: PathId, inputs: &[&str]| {
            resynced(path)
                || reached.iter().any(|(prim, names)| {
                    let placement = |name: &&str| {
                        let dirt = property_dirt(name);
                        dirt.transform || dirt.visibility
                    };
                    match names {
                        None => under(scene, path, *prim),
                        Some(names) if *prim == path => names
                            .iter()
                            .any(|name| placement(name) || reads(inputs, name)),
                        Some(names) => under(scene, path, *prim) && names.iter().any(placement),
                    }
                })
        };

        let cameras: BTreeSet<PathId> = found.cameras.iter().copied().collect();
        let gone: Vec<PathId> = self
            .cameras
            .keys()
            .filter(|path| resynced(**path) && !cameras.contains(path))
            .copied()
            .collect();
        for path in gone {
            self.cameras.remove(&path);
            gather.cameras.removed.insert(path);
        }
        let mut read: BTreeSet<PathId> = cameras;
        read.extend(
            self.cameras
                .keys()
                .copied()
                .filter(|&path| reaches(path, CAMERA_INPUTS)),
        );
        for path in read {
            self.read_camera(scene, path, gather);
        }

        let lights: BTreeMap<LightId, LightKind> = found.lights.iter().copied().collect();
        // A point instancer's discovery covers the lights it places.
        let instancers: Vec<PathId> = found
            .scopes
            .iter()
            .filter_map(|scope| match scope {
                Scope::Instancer(at) => Some(*at),
                _ => None,
            })
            .collect();
        let gone: Vec<LightId> = self
            .lights
            .keys()
            .filter(|id| {
                !lights.contains_key(id)
                    && (resynced(id.path)
                        || id
                            .instance
                            .is_some_and(|instance| instancers.contains(&instance.instancer)))
            })
            .copied()
            .collect();
        for id in gone {
            self.lights.remove(&id);
            gather.lights.removed.insert(id);
        }
        let mut read = lights;
        for (id, held) in &self.lights {
            if reaches(id.path, LIGHT_INPUTS)
                || held
                    .sources
                    .iter()
                    .any(|&source| shading_touched(scene, changes, &roots, source))
            {
                read.entry(*id).or_insert(held.item.kind);
            }
        }
        let mut placements = Placements::default();
        for (id, kind) in read {
            self.read_light(scene, id, kind, &mut placements, gather);
        }
    }

    /// Brings the materials up to `scene` after `changes`, once the items
    /// are: a material no item binds any more is dropped, a newly bound one
    /// read, and a held one read again when a change reaches it: a
    /// shading property (`inputs:*`, `outputs:*`, `info:*`) of it or of a
    /// shader or node graph it reads, an unlisted change of one of them, or
    /// a resync over one.
    fn apply_to_materials(
        &mut self,
        scene: &Scene<'_>,
        changes: &Changes,
        roots: &[PathId],
        gather: &mut Gather,
    ) {
        let bound: BTreeSet<PathId> = self
            .entries
            .values()
            .flat_map(|entry| {
                entry
                    .item
                    .material
                    .iter()
                    .chain(entry.item.subset_materials.iter().flatten())
                    .copied()
            })
            .collect();
        let gone: Vec<PathId> = self
            .materials
            .keys()
            .filter(|path| !bound.contains(path))
            .copied()
            .collect();
        for path in gone {
            self.materials.remove(&path);
            gather.shading.removed.insert(path);
        }
        let read: Vec<PathId> = bound
            .into_iter()
            .filter(|path| {
                self.materials.get(path).is_none_or(|held| {
                    core::iter::once(path)
                        .chain(&held.sources)
                        .any(|&prim| shading_touched(scene, changes, roots, prim))
                })
            })
            .collect();
        for path in read {
            if !gather.materials_read.contains(&path) {
                self.read_material(scene, path, gather);
            }
        }
        self.apply_to_uv_sets(scene, gather);
    }

    /// Plans again the meshes whose items' materials now read other
    /// texture coordinate sets besides `st` than the mesh was read with:
    /// listed in [`DrawDiff::rebuilt`]. Only when a material or an item's
    /// binding changed, or an item came.
    fn apply_to_uv_sets(&mut self, scene: &Scene<'_>, gather: &mut Gather) {
        if gather.shading.added.is_empty()
            && gather.shading.changed.is_empty()
            && gather.shading.removed.is_empty()
            && gather.materials.is_empty()
            && gather.added.is_empty()
        {
            return;
        }
        let mut wanted: BTreeMap<BufferKey, BTreeSet<String>> = BTreeMap::new();
        for entry in self.entries.values() {
            if self
                .buffers
                .get(&entry.item.buffer)
                .is_none_or(|buffer| buffer.mesh.is_none())
            {
                continue;
            }
            let names = wanted.entry(entry.item.buffer).or_default();
            for material in entry
                .item
                .material
                .iter()
                .chain(entry.item.subset_materials.iter().flatten())
            {
                if let Some(held) = self.materials.get(material) {
                    names.extend(uv_sets_read(&held.item));
                }
            }
        }
        let time = self.options.time;
        let mut planned: Vec<BufferKey> = Vec::new();
        for (key, names) in wanted {
            let names: Vec<String> = names.into_iter().collect();
            let Some(buffer) = self.buffers.get_mut(&key) else {
                continue;
            };
            if buffer.uv_sets == names {
                continue;
            }
            let Some(mesh) = StageMesh::read_with_uv_sets(scene, buffer.prim, time, &names) else {
                continue;
            };
            let uv_sets = mesh.uv_sets();
            let input = MeshInput {
                uv_sets: &uv_sets,
                ..mesh.input()
            };
            let plan = MeshPlan::new(&input, &self.options.mesh);
            gather.work.triangulations += 1;
            gather.rebuilt.insert(key);
            buffer.buffers = plan.buffers(&input).expect("its own input");
            buffer.revision += 1;
            buffer.subsets = mesh.subset_paths().to_vec();
            buffer.plan = Some(plan);
            drop(uv_sets);
            buffer.mesh = Some(mesh);
            buffer.uv_sets = names;
            planned.push(key);
        }
        // The sets drawn now might vary where the old ones did not.
        for entry in self.entries.values_mut() {
            if planned.contains(&entry.item.buffer) {
                let buffer = self.buffers.get(&entry.item.buffer);
                entry.varies.geometry = geometry_might_vary(scene, entry.item.prim, buffer);
            }
        }
    }

    /// The texture coordinate sets besides `st` the materials bound to a
    /// new mesh item read: its own, or every prototype group's of its gprim
    /// the diff adds, and its `materialBind` subsets'. A material not held
    /// yet is read now, once for the diff, so the mesh is planned once with
    /// them.
    fn uv_sets_for(
        &mut self,
        scene: &Scene<'_>,
        item: &DrawItem,
        gather: &mut Gather,
    ) -> Vec<String> {
        let mut materials = alloc::vec![match item.source {
            ItemSource::Prototype { material, .. } => material,
            _ => bound_material(scene, item.prim),
        }];
        if let ItemSource::Prototype { prim, .. } = item.source {
            let groups = gather.groups.get(&prim).into_iter().flatten();
            materials.extend(groups.map(|&material| Some(material)));
        }
        if let Some(imageable) = Imageable::get(scene, item.prim) {
            for subset in
                imageable.geom_subsets(Some(&GeomSubsetElementType::Face), Some("materialBind"))
            {
                materials.push(bound_material(scene, subset.path()));
            }
        }
        let mut names = BTreeSet::new();
        for material in materials.into_iter().flatten() {
            if !self.materials.contains_key(&material) && !gather.materials_read.contains(&material)
            {
                self.read_material(scene, material, gather);
            }
            if let Some(held) = self.materials.get(&material) {
                names.extend(uv_sets_read(&held.item));
            }
        }
        names.into_iter().collect()
    }

    /// Reads the material at `path` again and records whether it changed;
    /// drops it when it is no longer a material.
    fn read_material(&mut self, scene: &Scene<'_>, path: PathId, gather: &mut Gather) {
        gather.work.materials_read += 1;
        gather.materials_read.insert(path);
        let Some(read) =
            read_material(scene, path, self.options.time, self.options.render_contexts)
        else {
            if self.materials.remove(&path).is_some() {
                gather.shading.removed.insert(path);
            }
            return;
        };
        let mut sources: Vec<PathId> = Vec::new();
        let mut varies = false;
        for dependency in &read.dependencies {
            if dependency.prim != path && !sources.contains(&dependency.prim) {
                sources.push(dependency.prim);
            }
            varies = varies
                || inputs_might_vary(scene, dependency.prim, &[dependency.property.as_str()]);
        }
        let held = Held {
            item: read.item,
            varies,
            sources,
        };
        match self.materials.insert(path, held) {
            None => {
                gather.shading.added.insert(path);
            }
            Some(old) if old.item != self.materials[&path].item => {
                gather.shading.changed.insert(path);
            }
            Some(_) => {}
        }
    }

    /// Computes the camera at `path` again and records whether it changed.
    fn read_camera(&mut self, scene: &Scene<'_>, path: PathId, gather: &mut Gather) {
        gather.work.cameras_read += 1;
        let Some(view) = Camera::get(scene, path) else {
            if self.cameras.remove(&path).is_some() {
                gather.cameras.removed.insert(path);
            }
            return;
        };
        let item = CameraItem {
            path,
            camera: view.compute_camera(&mut self.xforms),
        };
        let varies =
            self.world_might_vary(scene, path) || inputs_might_vary(scene, path, CAMERA_INPUTS);
        let held = Held {
            item,
            varies,
            sources: Vec::new(),
        };
        match self.cameras.insert(path, held) {
            None => {
                gather.cameras.added.insert(path);
            }
            Some(old) if old.item != self.cameras[&path].item => {
                gather.cameras.changed.insert(path);
            }
            Some(_) => {}
        }
    }

    /// Reads the light `id` again and records whether it changed; drops it
    /// when its prim, or the instance placing it, is gone.
    fn read_light(
        &mut self,
        scene: &Scene<'_>,
        id: LightId,
        kind: LightKind,
        placements: &mut Placements,
        gather: &mut Gather,
    ) {
        gather.work.lights_read += 1;
        let path = id.path;
        let time = self.options.time;
        let matrix = if scene.stage().prim(path, scene.store()).is_none() {
            None
        } else if let Some(instance) = id.instance {
            self.placed_matrix(scene, path, instance, placements)
        } else {
            Some(self.xforms.local_to_world(scene, path).unwrap_or(IDENTITY))
        };
        let Some(matrix) = matrix else {
            if self.lights.remove(&id).is_some() {
                gather.lights.removed.insert(id);
            }
            return;
        };
        let invisible = |at: PathId| {
            Imageable::get(scene, at).is_some_and(|imageable| {
                imageable.compute_visibility(time) == Visibility::Invisible
            })
        };
        let hidden = invisible(path)
            || id
                .instance
                .is_some_and(|instance| invisible(instance.instancer));
        let (item, sources) = read_light(scene, id, kind, time, matrix, hidden);
        // A connected input's source might vary as well as its own value;
        // a placed light moves with its instancer's placement.
        let varies = self.world_might_vary(scene, path)
            || visibility_might_vary(scene, path)
            || inputs_might_vary(scene, path, LIGHT_INPUTS)
            || sources
                .iter()
                .any(|&source| shading_might_vary(scene, source))
            || id.instance.is_some_and(|instance| {
                inputs_might_vary(scene, instance.instancer, INSTANCER_INPUTS)
                    || visibility_might_vary(scene, instance.instancer)
                    || self.world_might_vary(scene, instance.instancer)
            });
        match self.lights.insert(
            id,
            Held {
                item,
                varies,
                sources,
            },
        ) {
            None => {
                gather.lights.added.insert(id);
            }
            Some(old) if old.item != self.lights[&id].item => {
                gather.lights.changed.insert(id);
            }
            Some(_) => {}
        }
    }

    /// The world matrix of the light at `path` placed by `instance`: its
    /// transform relative to its prototype, the instance's
    /// (`UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`, the
    /// prototype's own transform included, masked instances left out),
    /// then the instancer's world transform, as the instancer's draw items
    /// are placed. `None` when the instance is masked or gone, or no longer
    /// of a prototype holding the light.
    fn placed_matrix(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
        instance: LightInstance,
        placements: &mut Placements,
    ) -> Option<[[f64; 4]; 4]> {
        let time = self.options.time;
        let placed = placements
            .entry(instance.instancer)
            .or_insert_with(|| {
                let view = PointInstancer::get(scene, instance.instancer)?;
                let instances = view
                    .compute_instance_transforms(time, time, InstanceTransformOptions::default())
                    .ok()?;
                let ids = instance_ids(&view, time, &instances);
                Some(Placed {
                    prototypes: view
                        .prototypes()
                        .iter()
                        .map(|target| target.prim_path())
                        .collect(),
                    instances: ids
                        .into_iter()
                        .zip(instances)
                        .map(|(id, placed)| (id, (placed.prototype_index, placed.matrix)))
                        .collect(),
                })
            })
            .as_ref()?;
        let &(index, matrix) = placed.instances.get(&instance.id)?;
        let prototype = *placed.prototypes.get(index)?;
        if !under(scene, path, prototype) {
            return None;
        }
        let world = self
            .xforms
            .local_to_world(scene, instance.instancer)
            .unwrap_or(IDENTITY);
        let inner = if path == prototype {
            IDENTITY
        } else {
            self.xforms
                .relative_transform(scene, path, prototype)
                .map_or(IDENTITY, |relative| relative.matrix)
        };
        Some(multiply(&multiply(&inner, &matrix), &world))
    }

    /// Finds the items under `root`.
    fn discover(
        &self,
        scene: &Scene<'_>,
        root: PathId,
        firsts: &HashMap<PathId, PathId>,
        found: &mut Found,
    ) {
        let stage = scene.stage();
        let store = scene.store();
        // The root may be inside an instance or a point instancer: discover
        // from the outermost one then.
        let mut start = root;
        let mut at = parent(scene, root);
        while let Some(path) = at {
            if stage
                .prim(path, store)
                .is_some_and(layerstack::Prim::is_instance)
                || PointInstancer::get(scene, path).is_some()
            {
                start = path;
            }
            at = parent(scene, path);
        }
        found.scopes.push(Scope::Resync(start));
        let mut prototypes: BTreeSet<PathId> = BTreeSet::new();
        let mut stack = alloc::vec![start];
        while let Some(path) = stack.pop() {
            let Some(prim) = stage.prim(path, store) else {
                continue;
            };
            found.prims += 1;
            if !prim.is_active() || !prim.is_defined() || prim.is_abstract() {
                continue;
            }
            if PointInstancer::get(scene, path).is_some() {
                found.scopes.push(Scope::Instancer(path));
                self.discover_instancer(scene, path, found);
                continue;
            }
            if prim.is_instance() {
                if let Some(&first) = firsts.get(&path) {
                    prototypes.insert(first);
                }
                continue;
            }
            if gprim(scene, path).is_some() {
                found.sources.push(ItemSource::Gprim { path });
            }
            // A mesh light is a gprim drawn and a light listed.
            if scene.is_a(path, "Camera") {
                found.cameras.push(path);
            } else if let Some(kind) = LightKind::of(scene, path) {
                found.lights.push((path.into(), kind));
            }
            let children: Vec<PathId> = prim.all_children().map(|child| child.path()).collect();
            stack.extend(children.into_iter().rev());
        }
        for first in prototypes {
            found.scopes.push(Scope::Prototype(first));
            self.discover_prototype(scene, first, true, found);
        }
    }

    /// Finds a native instancing prototype's items, from its first
    /// instance: one per gprim and material its instances bind; with
    /// `views`, its cameras and lights too, once per instance, by their
    /// instance proxies' paths. Hydra lists them per instance as well:
    /// `UsdImagingInstanceAdapter::_Populate` gives each prim of a
    /// prototype its own adapter, lights included, and
    /// `GetLightParamValue` reads a light's parameters per instance.
    fn discover_prototype(&self, scene: &Scene<'_>, first: PathId, views: bool, found: &mut Found) {
        let stage = scene.stage();
        let store = scene.store();
        let roots = instances_of(scene, first);
        let mut stack: Vec<PathId> = stage
            .prim(first, store)
            .map(|prim| prim.all_children().map(|child| child.path()).collect())
            .unwrap_or_default();
        while let Some(path) = stack.pop() {
            let Some(prim) = stage.prim(path, store) else {
                continue;
            };
            found.prims += 1;
            if !prim.is_active() || !prim.is_defined() || prim.is_abstract() {
                continue;
            }
            if gprim(scene, path).is_some() {
                let mut materials: Vec<Option<PathId>> = Vec::new();
                for proxy in proxies(scene, first, path, &roots) {
                    let material = bound_material(scene, proxy);
                    if !materials.contains(&material) {
                        materials.push(material);
                    }
                }
                for material in materials {
                    found.sources.push(ItemSource::Prototype {
                        prim: path,
                        material,
                    });
                }
            }
            if views {
                let camera = scene.is_a(path, "Camera");
                let light = if camera {
                    None
                } else {
                    LightKind::of(scene, path)
                };
                if camera || light.is_some() {
                    for proxy in proxies(scene, first, path, &roots) {
                        match light {
                            Some(kind) => found.lights.push((proxy.into(), kind)),
                            None => found.cameras.push(proxy),
                        }
                    }
                }
            }
            stack.extend(prim.all_children().map(|child| child.path()));
        }
    }

    /// Finds a point instancer's items, each prototype's gprims, and the
    /// lights inside its prototypes, once per unmasked instance of their
    /// prototype. Cameras inside prototypes are not listed.
    fn discover_instancer(&self, scene: &Scene<'_>, instancer: PathId, found: &mut Found) {
        let Some(view) = PointInstancer::get(scene, instancer) else {
            return;
        };
        let stage = scene.stage();
        let store = scene.store();
        let mut lights: Vec<(usize, PathId, LightKind)> = Vec::new();
        for (index, target) in view.prototypes().iter().enumerate() {
            let prototype = target.prim_path();
            let mut stack = alloc::vec![prototype];
            while let Some(path) = stack.pop() {
                let Some(prim) = stage.prim(path, store) else {
                    continue;
                };
                found.prims += 1;
                if !prim.is_active() || !prim.is_defined() {
                    continue;
                }
                if gprim(scene, path).is_some() {
                    found.sources.push(ItemSource::PointInstancer {
                        instancer,
                        index: count(index),
                        prim: path,
                    });
                }
                if let Some(kind) = LightKind::of(scene, path) {
                    lights.push((index, path, kind));
                }
                stack.extend(prim.all_children().map(|child| child.path()));
            }
        }
        if lights.is_empty() {
            return;
        }
        let time = self.options.time;
        let placed = view
            .compute_instance_transforms(time, time, InstanceTransformOptions::default())
            .unwrap_or_default();
        let ids = instance_ids(&view, time, &placed);
        for (instance, &key) in placed.iter().zip(&ids) {
            for &(index, path, kind) in &lights {
                if instance.prototype_index == index {
                    let id = LightId {
                        path,
                        instance: Some(LightInstance { instancer, id: key }),
                    };
                    found.lights.push((id, kind));
                }
            }
        }
    }

    /// Reads the item `id` of `source` again, for what `what` says may
    /// have changed, and records what did; `new` for an item not in the
    /// list yet. `false` when there is nothing to draw any more.
    #[expect(
        clippy::too_many_lines,
        reason = "one pass over an item's inputs, in the order they depend on each other"
    )]
    fn read_item(
        &mut self,
        scene: &Scene<'_>,
        id: ItemId,
        source: ItemSource,
        what: Dirty,
        new: bool,
        gather: &mut Gather,
    ) -> bool {
        let time = self.options.time;
        let purposes = self.options.purposes;
        let previous = self.entries.get(&id).map(|entry| entry.item.clone());
        let mut item = previous.clone().unwrap_or(DrawItem {
            id,
            source,
            prim: source_path(&source),
            buffer: BufferKey(u32::MAX),
            matrices: Vec::new(),
            instances: Vec::new(),
            ids: Vec::new(),
            material: None,
            subset_materials: Vec::new(),
        });
        let mut depends = self
            .entries
            .get(&id)
            .map(|entry| entry.depends.clone())
            .unwrap_or_default();
        let mut varies = self
            .entries
            .get(&id)
            .map(|entry| entry.varies)
            .unwrap_or_default();

        // Instances, visibility and matrices.
        if what.instances || what.transform || what.visibility || new {
            let mut matrices = Vec::new();
            let mut instances = Vec::new();
            let mut ids = Vec::new();
            varies.matrices = false;
            varies.visibility = false;
            match source {
                ItemSource::Gprim { path } => {
                    if scene.stage().prim(path, scene.store()).is_none() {
                        return false;
                    }
                    depends = alloc::vec![path];
                    varies.visibility = visibility_might_vary(scene, path);
                    if shown(scene, path, time, &purposes) {
                        gather.work.matrices_computed += 1;
                        matrices.push(self.xforms.local_to_world(scene, path).unwrap_or(IDENTITY));
                        instances.push(path);
                        varies.matrices = self.world_might_vary(scene, path);
                    }
                }
                ItemSource::Prototype { prim, material } => {
                    let Some(first) = instance_root(scene, prim) else {
                        return false;
                    };
                    let roots = instances_of(scene, first);
                    if roots.first() != Some(&first)
                        || scene.stage().prim(prim, scene.store()).is_none()
                    {
                        return false;
                    }
                    let all = proxies(scene, first, prim, &roots);
                    let mut members = 0;
                    for &proxy in &all {
                        if bound_material(scene, proxy) != material {
                            continue;
                        }
                        members += 1;
                        varies.visibility |= visibility_might_vary(scene, proxy);
                        if !shown(scene, proxy, time, &purposes) {
                            continue;
                        }
                        gather.work.matrices_computed += 1;
                        matrices.push(self.xforms.local_to_world(scene, proxy).unwrap_or(IDENTITY));
                        instances.push(proxy);
                        varies.matrices |= self.world_might_vary(scene, proxy);
                    }
                    if members == 0 {
                        return false;
                    }
                    depends = all;
                    depends.extend(roots);
                }
                ItemSource::PointInstancer {
                    instancer,
                    index,
                    prim,
                } => {
                    let Some(view) = PointInstancer::get(scene, instancer) else {
                        return false;
                    };
                    let Some(prototype) = view
                        .prototypes()
                        .get(index as usize)
                        .map(|target| target.prim_path())
                    else {
                        return false;
                    };
                    if !under(scene, prim, prototype)
                        || scene.stage().prim(prim, scene.store()).is_none()
                    {
                        return false;
                    }
                    depends = alloc::vec![instancer, prototype, prim];
                    varies.visibility = visibility_might_vary(scene, instancer);
                    if shown(scene, instancer, time, &purposes) {
                        // The prim's world chain covers its transform
                        // relative to the prototype and the instancer's.
                        varies.matrices = inputs_might_vary(scene, instancer, INSTANCER_INPUTS)
                            || self.world_might_vary(scene, instancer)
                            || (prim != prototype && self.world_might_vary(scene, prim));
                        let placed = view.compute_instance_transforms(
                            time,
                            time,
                            InstanceTransformOptions::default(),
                        );
                        let world = self
                            .xforms
                            .local_to_world(scene, instancer)
                            .unwrap_or(IDENTITY);
                        let inner = if prim == prototype {
                            IDENTITY
                        } else {
                            self.xforms
                                .relative_transform(scene, prim, prototype)
                                .map_or(IDENTITY, |relative| relative.matrix)
                        };
                        for instance in placed.unwrap_or_default() {
                            if instance.prototype_index != index as usize {
                                continue;
                            }
                            gather.work.matrices_computed += 1;
                            matrices.push(multiply(&multiply(&inner, &instance.matrix), &world));
                            ids.push(instance.id);
                        }
                    }
                }
            }
            if !new {
                if matrices != item.matrices || ids != item.ids || instances != item.instances {
                    gather.matrices.insert(id);
                }
                if matrices.is_empty() != item.matrices.is_empty() {
                    gather.visibility.insert(id);
                }
            }
            item.matrices = matrices;
            item.instances = instances;
            item.ids = ids;
        }

        // Geometry.
        if what.geometry || new {
            let Some(key) = self.read_geometry(scene, &item, gather) else {
                if !new {
                    self.release(item.buffer, gather);
                }
                return false;
            };
            if key != item.buffer {
                if !new {
                    gather.rebound.insert(id);
                    self.release(item.buffer, gather);
                }
                if let Some(buffer) = self.buffers.get_mut(&key) {
                    buffer.users += 1;
                }
                item.buffer = key;
            }
            varies.geometry = geometry_might_vary(scene, item.prim, self.buffers.get(&key));
        }

        // Materials.
        if what.material || what.geometry || new {
            // A prototype's group binds its material by definition.
            let material = match source {
                ItemSource::Prototype { material, .. } => material,
                _ => bound_material(scene, item.prim),
            };
            let subsets: Vec<Option<PathId>> = self
                .buffers
                .get(&item.buffer)
                .map(|buffer| {
                    buffer
                        .subsets
                        .iter()
                        .map(|&subset| bound_material(scene, subset))
                        .collect()
                })
                .unwrap_or_default();
            if !new && (material != item.material || subsets != item.subset_materials) {
                gather.materials.insert(id);
            }
            item.material = material;
            item.subset_materials = subsets;
        }
        if let Some(buffer) = self.buffers.get(&item.buffer) {
            for &subset in &buffer.subsets {
                if !depends.contains(&subset) {
                    depends.push(subset);
                }
            }
        }
        if previous.is_none() {
            self.keys.insert(source, id);
        }
        self.entries.insert(
            id,
            Entry {
                item,
                depends,
                varies,
            },
        );
        true
    }

    /// What a property's change may change of the items under its prim
    /// ([`property_dirt`]); a texture coordinate set a buffer is drawn
    /// with (`primvars:<name>` and its `:indices`) is geometry too.
    fn property_dirt(&self, name: &str) -> Dirty {
        let mut dirty = property_dirt(name);
        if let Some(set) = name.strip_prefix("primvars:") {
            let set = set.strip_suffix(":indices").unwrap_or(set);
            dirty.geometry |= self
                .buffers
                .values()
                .any(|buffer| buffer.uv_sets.iter().any(|drawn| drawn == set));
        }
        dirty
    }

    /// Whether the prim's local-to-world transform might differ at another
    /// time ([`XformCache::local_to_world_might_vary`]).
    fn world_might_vary(&mut self, scene: &Scene<'_>, path: PathId) -> bool {
        self.xforms
            .local_to_world_might_vary(scene, path)
            .unwrap_or(false)
    }

    /// The buffer `item` draws, read again: kept, refilled or rebuilt in
    /// place for a mesh, shared or tessellated for a shape. `None` when
    /// the prim draws nothing.
    fn read_geometry(
        &mut self,
        scene: &Scene<'_>,
        item: &DrawItem,
        gather: &mut Gather,
    ) -> Option<BufferKey> {
        let time = self.options.time;
        match gprim(scene, item.prim)? {
            Gprim::Mesh => {
                let uv_sets = match self.mesh_buffers.get(&item.prim) {
                    Some(key) => self.buffers[key].uv_sets.clone(),
                    None => self.uv_sets_for(scene, item, gather),
                };
                let mesh = StageMesh::read_with_uv_sets(scene, item.prim, time, &uv_sets)?;
                let sets = mesh.uv_sets();
                if let Some(&key) = self.mesh_buffers.get(&item.prim) {
                    let buffer = self.buffers.get_mut(&key).expect("a kept buffer");
                    let old = buffer.mesh.as_ref().expect("a mesh's buffer");
                    let plan = buffer.plan.as_ref().expect("a mesh's buffer");
                    let input = MeshInput {
                        uv_sets: &sets,
                        ..mesh.input()
                    };
                    let same_shape = plan.same_topology(&input.topology)
                        && old.topology().subdivision_scheme == mesh.topology().subdivision_scheme
                        && old.input().subsets == input.subsets;
                    if same_shape && old.same_values(&mesh) {
                        gather.work.buffers_reused += 1;
                        drop(sets);
                        buffer.mesh = Some(mesh);
                        return Some(key);
                    }
                    if same_shape {
                        let filled = if old.same_primvars(&mesh) {
                            let mut buffers = buffer.buffers.clone();
                            plan.refill(&mut buffers, input.points, input.normals.as_ref())
                                .map(|()| buffers)
                        } else {
                            plan.buffers(&input)
                        };
                        if let Ok(filled) = filled {
                            gather.work.refills += 1;
                            gather.refilled.insert(key);
                            buffer.buffers = filled;
                            buffer.revision += 1;
                            buffer.subsets = mesh.subset_paths().to_vec();
                            drop(sets);
                            buffer.mesh = Some(mesh);
                            return Some(key);
                        }
                    }
                    let plan = MeshPlan::new(&input, &self.options.mesh);
                    gather.work.triangulations += 1;
                    gather.rebuilt.insert(key);
                    buffer.buffers = plan.buffers(&input).expect("its own input");
                    buffer.revision += 1;
                    buffer.subsets = mesh.subset_paths().to_vec();
                    buffer.plan = Some(plan);
                    drop(sets);
                    buffer.mesh = Some(mesh);
                    return Some(key);
                }
                let input = MeshInput {
                    uv_sets: &sets,
                    ..mesh.input()
                };
                let plan = MeshPlan::new(&input, &self.options.mesh);
                gather.work.triangulations += 1;
                let key = self.next_key();
                gather.buffers_added.insert(key);
                self.buffers.insert(
                    key,
                    DrawBuffers {
                        key,
                        prim: item.prim,
                        buffers: plan.buffers(&input).expect("its own input"),
                        subsets: mesh.subset_paths().to_vec(),
                        revision: 0,
                        shape: None,
                        plan: Some(plan),
                        mesh: None,
                        uv_sets,
                        users: 0,
                    },
                );
                drop(sets);
                if let Some(buffer) = self.buffers.get_mut(&key) {
                    buffer.mesh = Some(mesh);
                }
                self.mesh_buffers.insert(item.prim, key);
                Some(key)
            }
            Gprim::Shape => {
                let shape = read_shape(scene, item.prim, time)?;
                let shape_key = shape_key(&shape);
                if let Some(&key) = self.shape_buffers.get(&shape_key) {
                    if key == item.buffer {
                        gather.work.buffers_reused += 1;
                    }
                    return Some(key);
                }
                gather.work.tessellations += 1;
                let key = self.next_key();
                gather.buffers_added.insert(key);
                self.buffers.insert(
                    key,
                    DrawBuffers {
                        key,
                        prim: item.prim,
                        buffers: tessellate(&shape, &self.options.tessellation),
                        subsets: Vec::new(),
                        revision: 0,
                        shape: Some(shape),
                        plan: None,
                        mesh: None,
                        uv_sets: Vec::new(),
                        users: 0,
                    },
                );
                self.shape_buffers.insert(shape_key, key);
                Some(key)
            }
        }
    }

    fn next_key(&mut self) -> BufferKey {
        let key = BufferKey(self.next_buffer);
        self.next_buffer += 1;
        key
    }

    /// One item fewer draws `key`: freed when it was the last.
    fn release(&mut self, key: BufferKey, gather: &mut Gather) {
        let Some(buffer) = self.buffers.get_mut(&key) else {
            return;
        };
        buffer.users = buffer.users.saturating_sub(1);
        if buffer.users > 0 {
            return;
        }
        let buffer = self.buffers.remove(&key).expect("present");
        match &buffer.shape {
            Some(shape) => {
                self.shape_buffers.remove(&shape_key(shape));
            }
            None => {
                self.mesh_buffers.remove(&buffer.prim);
            }
        }
        gather.freed.insert(key);
        gather.buffers_added.remove(&key);
    }

    fn remove(&mut self, id: ItemId, gather: &mut Gather) {
        let Some(entry) = self.entries.remove(&id) else {
            return;
        };
        self.keys.remove(&entry.item.source);
        self.release(entry.item.buffer, gather);
        if !gather.added.remove(&id) {
            gather.removed.insert(id);
        }
    }
}

/// The instances of the point instancers read in one diff, by instancer,
/// computed once for all the lights they place.
type Placements = HashMap<PathId, Option<Placed>>;

/// A point instancer's prototypes and its unmasked instances, by
/// [`InstanceId`]: their prototype index and matrix.
struct Placed {
    prototypes: Vec<PathId>,
    instances: HashMap<InstanceId, (usize, [[f64; 4]; 4])>,
}

/// The [`InstanceId`] of each of `placed`, a point instancer's unmasked
/// instances at `time`: their `ids` entries when it authors `ids` and
/// they are distinct, their array positions otherwise.
fn instance_ids(
    view: &PointInstancer<'_>,
    time: Time,
    placed: &[InstanceTransform],
) -> Vec<InstanceId> {
    let authored = view.ids_at(time).is_some() && {
        let mut seen = HashSet::with_capacity(placed.len());
        placed.iter().all(|instance| seen.insert(instance.id))
    };
    placed
        .iter()
        .map(|instance| {
            if authored {
                InstanceId::Authored(instance.id)
            } else {
                InstanceId::Index(count(instance.index))
            }
        })
        .collect()
}

const IDENTITY: [[f64; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// `a * b`, row-vector matrices: `a` applies first.
fn multiply(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, value) in out_row.iter_mut().enumerate() {
            *value = (0..4).map(|k| a[row][k] * b[k][column]).sum();
        }
    }
    out
}

/// Whether the geometry `prim` draws into `buffer` might differ at another
/// time: its geometry's attributes, its face subsets', or a texture
/// coordinate set the buffer draws.
fn geometry_might_vary(scene: &Scene<'_>, prim: PathId, buffer: Option<&DrawBuffers>) -> bool {
    inputs_might_vary(scene, prim, GEOMETRY_INPUTS)
        || buffer.is_some_and(|buffer| {
            buffer
                .subsets
                .iter()
                .any(|&subset| inputs_might_vary(scene, subset, SUBSET_INPUTS))
                || buffer.uv_sets.iter().any(|name| {
                    let primvar = alloc::format!("primvars:{name}");
                    let indices = alloc::format!("primvars:{name}:indices");
                    inputs_might_vary(scene, prim, &[&primvar, &indices])
                })
        })
}

/// The texture coordinate sets besides `st` a material's textures read
/// (their `UsdPrimvarReader_float2`'s `varname`).
fn uv_sets_read(material: &MaterialItem) -> impl Iterator<Item = String> + '_ {
    material
        .textures
        .iter()
        .filter_map(|texture| texture.coordinates.primvar.clone())
        .filter(|name| name != "st")
}

/// The path an item is discovered at.
fn source_path(source: &ItemSource) -> PathId {
    match *source {
        ItemSource::Gprim { path } => path,
        ItemSource::Prototype { prim, .. } | ItemSource::PointInstancer { prim, .. } => prim,
    }
}

/// The parent prim's path; `None` for the pseudo-root.
fn parent(scene: &Scene<'_>, path: PathId) -> Option<PathId> {
    scene.store().paths().parent(path)
}

/// Whether `path` is `root` or beneath it.
fn under(scene: &Scene<'_>, path: PathId, root: PathId) -> bool {
    let paths = scene.store().paths();
    let mut at = Some(path);
    while let Some(current) = at {
        if current == root {
            return true;
        }
        at = paths.parent(current);
    }
    false
}

/// Whether the prim is drawn: visible, and of a drawn purpose.
/// OpenUSD: `UsdGeomImageable::ComputeVisibility`, `ComputePurposeInfo`.
fn shown(scene: &Scene<'_>, path: PathId, time: Time, purposes: &Purposes) -> bool {
    Imageable::get(scene, path).is_some_and(|imageable| {
        imageable.compute_visibility(time) != Visibility::Invisible
            && purposes.draws(&imageable.compute_purpose_info().purpose)
    })
}

/// The material bound to the prim for the `preview` purpose (falling back
/// to all-purpose bindings).
/// OpenUSD: `UsdShadeMaterialBindingAPI::ComputeBoundMaterial`.
fn bound_material(scene: &Scene<'_>, path: PathId) -> Option<PathId> {
    PrimView::new(*scene, path)
        .compute_bound_material(&MaterialPurpose::Preview, BindingOptions::default())
        .material
}

/// Each native instance root's prototype's first instance root.
fn prototype_firsts(scene: &Scene<'_>) -> HashMap<PathId, PathId> {
    let mut firsts = HashMap::new();
    for prototype in scene.stage().prototypes() {
        if let Some(&first) = prototype.instances().first() {
            for &instance in prototype.instances() {
                firsts.insert(instance, first);
            }
        }
    }
    firsts
}

/// The instance roots sharing the prototype of the instance root `first`,
/// in traversal order.
fn instances_of(scene: &Scene<'_>, first: PathId) -> Vec<PathId> {
    let stage = scene.stage();
    let Some(id) = stage
        .prim(first, scene.store())
        .and_then(layerstack::Prim::prototype)
    else {
        return Vec::new();
    };
    stage
        .prototypes()
        .find(|prototype| prototype.id() == id)
        .map(|prototype| prototype.instances().to_vec())
        .unwrap_or_default()
}

/// The nearest instance root above `path`.
fn instance_root(scene: &Scene<'_>, path: PathId) -> Option<PathId> {
    let stage = scene.stage();
    let mut at = parent(scene, path);
    while let Some(current) = at {
        if stage
            .prim(current, scene.store())
            .is_some_and(layerstack::Prim::is_instance)
        {
            return Some(current);
        }
        at = parent(scene, current);
    }
    None
}

/// The prim at `path` (under the instance root `first`) in each instance
/// of `roots`.
fn proxies(scene: &Scene<'_>, first: PathId, path: PathId, roots: &[PathId]) -> Vec<PathId> {
    let paths = scene.store().paths();
    let relative = paths
        .resolve(path)
        .strip_prefix(paths.resolve(first))
        .map(<[_]>::to_vec)
        .unwrap_or_default();
    roots
        .iter()
        .map(|&root| paths.intern(paths.resolve(root).join(&relative)))
        .collect()
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use layerstack::{
        EditTarget, InMemoryStore, LayerId, LiveStage, NoAssets, PropertySpec, PropertyType,
        StageOptions, Time, Transaction, Value,
    };
    use layerstack_schemas::usd_geom::{MeshEdit, PointInstancerEdit, SphereEdit};
    use layerstack_schemas::usd_shade::MaterialBindingApi;
    use layerstack_schemas::{MaterialPurpose, PrimEdit, Scene, SchemaEdit};

    use super::{DrawDiff, DrawList, DrawOptions, ItemId, ItemSource};

    const SCENE: &str = r#"#usda 1.0
def Xform "World" {
    def Xform "Rocks" {
        def Mesh "Rock" {
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points.timeSamples = {
                1: [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)],
                2: [(0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)],
            }
        }
        def Sphere "Pebble" {
            double radius = 0.5
        }
        def Sphere "Stone" {
            double radius = 0.5
        }
    }
    def Xform "Beacons" {
        def Xform "North" (
            instanceable = true
            prepend references = </Beacon>
        ) {
            double3 xformOp:translate = (1, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
        def Xform "South" (
            instanceable = true
            prepend references = </Beacon>
        ) {
            double3 xformOp:translate = (-1, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
    def PointInstancer "Stones" {
        rel prototypes = </World/Stones/Prototypes/Stone>
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0)]
        def Scope "Prototypes" {
            def Cube "Stone" {
                double size = 0.25
            }
        }
    }
}
def Scope "Looks" {
    def Material "Red" {
    }
}
class Xform "Beacon" {
    def Cube "Post" {
        double size = 0.5
    }
    def Sphere "Bulb" {
        double radius = 0.25
    }
}
"#;

    struct Fixture {
        store: InMemoryStore,
        live: LiveStage,
        list: DrawList,
    }

    impl Fixture {
        fn new() -> (Self, DrawDiff) {
            let mut store = InMemoryStore::default();
            let read = layerstack_usda::read_usda(
                SCENE,
                LayerId(1),
                store.tokens(),
                store.paths(),
                &mut NoAssets,
            );
            assert!(
                !read.has_errors(),
                "{:?}",
                read.diagnostics().collect::<Vec<_>>()
            );
            store.insert_layer(read.emitted.layer);
            let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
            let live = LiveStage::compose(
                &mut store,
                LayerId(1),
                StageOptions {
                    schemas: Some(schemas),
                    ..StageOptions::default()
                },
            );
            let mut list = DrawList::new(DrawOptions {
                time: Time::at(1.0),
                ..DrawOptions::default()
            });
            let diff = list.refresh(&Scene::new(live.stage(), &store));
            (Self { store, live, list }, diff)
        }

        /// Applies the edit `author` makes and hands the changes to the list.
        fn edit(&mut self, author: impl FnOnce(&mut SchemaEdit<'_>)) -> DrawDiff {
            let mut edit = SchemaEdit::new(
                self.live.stage(),
                &mut self.store,
                EditTarget::for_layer(LayerId(1)),
            );
            author(&mut edit);
            let transaction = edit.finish();
            self.apply(&transaction)
        }

        fn apply(&mut self, transaction: &Transaction) -> DrawDiff {
            let applied = self.live.apply(&mut self.store, transaction).unwrap();
            let scene = Scene::new(self.live.stage(), &self.store);
            self.list.apply_changes(&scene, &applied.changes)
        }

        fn path(&self, text: &str) -> layerstack::PathId {
            self.store.path(text)
        }

        /// The item drawing `path` (a gprim, or a prototype's gprim in its
        /// first instance, or an instancer's prototype).
        fn item(&self, text: &str) -> ItemId {
            let path = self.path(text);
            self.list
                .items()
                .find(|item| item.prim == path)
                .unwrap_or_else(|| panic!("no item draws {text}"))
                .id
        }
    }

    #[test]
    fn a_refresh_groups_instances_and_shares_equal_shapes() {
        let (fixture, diff) = Fixture::new();
        let list = &fixture.list;
        // The rock, two pebble-sized spheres, the beacon's post and bulb
        // (one item each for both instances) and the instancer's stone.
        assert_eq!(list.len(), 6);
        assert_eq!(diff.added.len(), 6);
        // One plan for the rock; four tessellations: the two spheres of
        // radius 0.5 share one.
        assert_eq!(diff.work.triangulations, 1);
        assert_eq!(diff.work.tessellations, 4);
        assert_eq!(diff.buffers_added.len(), 5);
        let pebble = list.item(fixture.item("/World/Rocks/Pebble")).unwrap();
        let stone = list.item(fixture.item("/World/Rocks/Stone")).unwrap();
        assert_eq!(pebble.buffer, stone.buffer);
        let post = list
            .item(fixture.item("/World/Beacons/North/Post"))
            .unwrap();
        assert!(matches!(post.source, ItemSource::Prototype { .. }));
        assert_eq!(
            post.instances,
            [
                fixture.path("/World/Beacons/North/Post"),
                fixture.path("/World/Beacons/South/Post")
            ]
        );
        // Row-vector matrices: the translation is the last row.
        assert_eq!(post.matrices[0][3], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(post.matrices[1][3], [-1.0, 0.0, 0.0, 1.0]);
        let stones = list
            .item(fixture.item("/World/Stones/Prototypes/Stone"))
            .unwrap();
        assert_eq!(stones.ids, [0, 1, 2]);
        assert_eq!(stones.matrices[2][3], [2.0, 0.0, 0.0, 1.0]);
        // Nothing changed: a second refresh reads everything and rebuilds
        // nothing.
        let (mut fixture, _) = Fixture::new();
        let scene = Scene::new(fixture.live.stage(), &fixture.store);
        let again = fixture.list.refresh(&scene);
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(again.work.triangulations + again.work.tessellations, 0);
        assert_eq!(again.work.buffers_reused, 6);
    }

    #[test]
    fn a_property_edit_on_one_prim_touches_only_its_item() {
        let (mut fixture, _) = Fixture::new();
        let pebble = fixture.item("/World/Rocks/Pebble");
        let path = fixture.path("/World/Rocks/Pebble");
        let diff = fixture.edit(|edit| {
            SphereEdit::new(edit, path)
                .unwrap()
                .unwrap()
                .set_radius(edit, 0.75);
        });
        // The pebble draws a new tessellation; the stone keeps the old one.
        assert_eq!(diff.rebound, [pebble]);
        assert_eq!(diff.buffers_added.len(), 1);
        assert!(diff.freed.is_empty() && diff.matrices.is_empty() && diff.added.is_empty());
        assert_eq!(diff.work.items_read, 1);
        assert_eq!(diff.work.items_untouched, 5);
        assert_eq!(diff.work.tessellations, 1);
        assert_eq!(diff.work.triangulations, 0);
        assert_eq!(diff.work.prims_traversed, 0);
    }

    #[test]
    fn a_time_change_refills_an_animated_mesh_without_triangulating() {
        let (mut fixture, _) = Fixture::new();
        let rock = fixture.item("/World/Rocks/Rock");
        let buffer = fixture.list.item(rock).unwrap().buffer;
        let scene = Scene::new(fixture.live.stage(), &fixture.store);
        let diff = fixture.list.set_time(&scene, Time::at(2.0));
        assert_eq!(diff.refilled, [buffer]);
        assert!(diff.rebuilt.is_empty() && diff.matrices.is_empty() && diff.rebound.is_empty());
        assert_eq!(diff.work.refills, 1);
        assert_eq!(diff.work.triangulations, 0);
        assert_eq!(diff.work.tessellations, 0);
        // Nothing the other items read is animated: none was read.
        assert_eq!(diff.work.items_read, 1);
        assert_eq!(diff.work.items_untouched, 5);
        assert_eq!(diff.work.items_time_invariant, 5);
        assert_eq!(diff.work.buffers_reused, 0);
        let buffers = fixture.list.buffers(buffer).unwrap();
        assert_eq!(buffers.revision, 1);
        assert!(buffers.buffers.positions.iter().all(|p| p[2] == 1.0));
    }

    /// An edit that animates a static input makes its items read at the
    /// next time, and undoing it makes them time-invariant again: a sample
    /// on the pebble's radius reaches the pebble alone, one on the rocks'
    /// transform every item beneath them, one on a beacon instance's
    /// visibility the beacon's items.
    #[test]
    fn edits_that_animate_an_input_make_its_items_read_at_a_new_time() {
        let (mut fixture, _) = Fixture::new();
        let rock = fixture.item("/World/Rocks/Rock");
        let pebble = fixture.item("/World/Rocks/Pebble");
        let stone = fixture.item("/World/Rocks/Stone");
        let read_at = |fixture: &mut Fixture, time: f64| {
            let scene = Scene::new(fixture.live.stage(), &fixture.store);
            let diff = fixture.list.set_time(&scene, Time::at(time));
            assert_eq!(
                diff.work.items_read + diff.work.items_time_invariant,
                6,
                "every item read or skipped"
            );
            diff
        };
        let property = |fixture: &Fixture, path: &str, name: &str| {
            let name = fixture.store.tokens().intern(name);
            EditTarget::for_layer(LayerId(1))
                .property(layerstack::PropertyPath::new(fixture.path(path), name))
        };
        let token =
            |fixture: &Fixture, text: &str| Value::Token(fixture.store.tokens().intern(text));

        let mut sample = Transaction::new();
        sample.set_time_sample(
            property(&fixture, "/World/Rocks/Pebble", "radius"),
            3.0,
            Value::Double(2.0),
        );
        let applied = fixture.live.apply(&mut fixture.store, &sample).unwrap();
        let scene = Scene::new(fixture.live.stage(), &fixture.store);
        let diff = fixture.list.apply_changes(&scene, &applied.changes);
        // A single sample holds at every numeric time: the pebble is
        // larger already.
        assert_eq!(diff.work.items_read, 1);
        assert_eq!(diff.rebound, [pebble]);
        let diff = read_at(&mut fixture, 3.0);
        assert_eq!(diff.work.items_read, 2, "the rock and the pebble");
        let undo = fixture.apply(&applied.inverse);
        assert_eq!(undo.rebound, [pebble], "back to the radius at default");
        assert_eq!(
            read_at(&mut fixture, 4.0).work.items_read,
            1,
            "the rock alone"
        );

        let typed = |name: &str| PropertySpec::typed_attribute(PropertyType::parse(name).unwrap());
        let mut moving = Transaction::new();
        moving.create_property(
            property(&fixture, "/World/Rocks", "xformOpOrder"),
            typed("token[]").with_default(Value::array(alloc::vec![token(
                &fixture,
                "xformOp:translate"
            )])),
        );
        let translate = property(&fixture, "/World/Rocks", "xformOp:translate");
        moving.create_property(translate.clone(), typed("double3"));
        moving.set_time_sample(translate.clone(), 5.0, Value::Vec3d([0.0, 5.0, 0.0]));
        moving.set_time_sample(translate, 7.0, Value::Vec3d([0.0, 7.0, 0.0]));
        let diff = fixture.apply(&moving);
        assert_eq!(diff.work.items_read, 3, "the items beneath the rocks");
        let diff = read_at(&mut fixture, 7.0);
        assert_eq!(diff.work.items_read, 3);
        assert_eq!(diff.matrices, [rock, pebble, stone]);

        let mut blink = Transaction::new();
        let visibility = property(&fixture, "/World/Beacons/North", "visibility");
        blink.create_property(visibility.clone(), typed("token"));
        blink.set_time_sample(visibility, 6.0, token(&fixture, "invisible"));
        fixture.apply(&blink);
        let diff = read_at(&mut fixture, 8.0);
        // The rocks (moving) and the beacon's post and bulb (blinking).
        assert_eq!(diff.work.items_read, 5);
        assert_eq!(diff.work.items_time_invariant, 1, "the stones");
    }

    /// A prim of a schema derived from `Mesh`, or from a shape, is drawn
    /// as one (`IsA`), as the TypeScript draw list draws it.
    #[test]
    fn prims_of_derived_schemas_are_drawn_as_their_base() {
        use layerstack::{SchemaDefinition, SchemaRegistry};
        let mut store = InMemoryStore::default();
        let read = layerstack_usda::read_usda(
            r#"#usda 1.0
def Xform "Lab" {
    def RockMesh "Rock" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    }
    def Lamp "Lamp" {
        double radius = 0.25
    }
    def Robot "Robot" {
    }
}
"#,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        store.insert_layer(read.emitted.layer);
        let tokens = store.tokens();
        let mut builder = SchemaRegistry::builder();
        layerstack_schemas::register(&mut builder, layerstack_schemas::Domain::ALL, tokens)
            .unwrap();
        for (name, base) in [("RockMesh", "Mesh"), ("Lamp", "Sphere"), ("Robot", "Xform")] {
            builder.register(
                SchemaDefinition::typed(tokens.intern(name)).with_parent(tokens.intern(base)),
            );
        }
        let schemas = Arc::new(builder.build(tokens));
        let live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
        );
        let mut list = DrawList::new(DrawOptions::default());
        let diff = list.refresh(&Scene::new(live.stage(), &store));
        let drawn: Vec<_> = list
            .items()
            .map(|item| store.paths().display(item.prim, store.tokens()))
            .collect();
        assert_eq!(drawn, ["/Lab/Rock", "/Lab/Lamp"]);
        assert_eq!(diff.work.triangulations, 1);
        assert_eq!(diff.work.tessellations, 1);
        let lamp = list
            .items()
            .find(|item| item.prim == store.path("/Lab/Lamp"))
            .unwrap();
        assert_eq!(
            list.buffers(lamp.buffer).unwrap().shape,
            Some(crate::shapes::Shape::Sphere { radius: 0.25 })
        );
    }

    /// Stage metadata (the root layer's `documentation`, `metersPerUnit`,
    /// `upAxis`, `startTimeCode`) changes the pseudo-root info-only, and
    /// nothing the list holds reads it: nothing is read.
    #[test]
    fn stage_metadata_reads_nothing() {
        let (mut fixture, _) = Fixture::new();
        let root = fixture.path("/");
        let tokens = fixture.store.tokens();
        let fields = [
            ("documentation", Value::string("rocks and beacons")),
            ("metersPerUnit", Value::Double(0.01)),
            ("upAxis", Value::Token(tokens.intern("Z"))),
            ("startTimeCode", Value::Double(1.0)),
        ];
        for (key, value) in fields {
            let mut edit = Transaction::new();
            edit.set_layer_metadata(
                LayerId(1),
                fixture.store.tokens().intern(key),
                layerstack::FieldValue::Value(value),
            );
            let applied = fixture.live.apply(&mut fixture.store, &edit).unwrap();
            assert_eq!(applied.changes.changed_info_only, [root], "{key}");
            let scene = Scene::new(fixture.live.stage(), &fixture.store);
            let diff = fixture.list.apply_changes(&scene, &applied.changes);
            assert!(diff.is_empty(), "{key}: {diff:?}");
            assert_eq!(diff.work.items_read, 0, "{key}");
            assert_eq!(diff.work.items_untouched, 6, "{key}");
            assert_eq!(diff.work.prims_traversed, 0, "{key}");
            assert_eq!(
                diff.work.lights_read + diff.work.cameras_read + diff.work.materials_read,
                0,
                "{key}"
            );
        }
    }

    /// A quad authoring `st` and `st1`, whose material reads `st1`
    /// (`UsdPrimvarReader_float2` `varname`), split where either differs.
    const UV_SETS: &str = r#"#usda 1.0
def Mesh "Plate" (
    prepend apiSchemas = ["MaterialBindingAPI"]
) {
    int[] faceVertexCounts = [3, 3]
    int[] faceVertexIndices = [0, 1, 2, 0, 2, 3]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
        interpolation = "vertex"
    )
    texCoord2f[] primvars:st1 = [(0, 0), (2, 0), (2, 2), (0, 2), (5, 5), (5, 5)] (
        interpolation = "faceVarying"
    )
    rel material:binding = </Looks/Tiled>
}
def Scope "Looks" {
    def Material "Tiled" {
        token outputs:surface.connect = </Looks/Tiled/Surface.outputs:surface>
        token inputs:coordinates = "st1"
        def Shader "Surface" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Tiled/Texture.outputs:rgb>
            token outputs:surface
        }
        def Shader "Texture" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @tile.png@
            float2 inputs:st.connect = </Looks/Tiled/Reader.outputs:result>
            float3 outputs:rgb
        }
        def Shader "Reader" {
            uniform token info:id = "UsdPrimvarReader_float2"
            string inputs:varname.connect = </Looks/Tiled.inputs:coordinates>
            float2 outputs:result
        }
    }
}
"#;

    /// A material reading `st1` gets the mesh a buffer channel of its own,
    /// planned once; making it read `st` instead plans the mesh again
    /// without it, listed as rebuilt.
    #[test]
    fn a_material_reading_another_uv_set_gets_its_channel() {
        let mut store = InMemoryStore::default();
        let read = layerstack_usda::read_usda(
            UV_SETS,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        assert!(
            !read.has_errors(),
            "{:?}",
            read.diagnostics().collect::<Vec<_>>()
        );
        store.insert_layer(read.emitted.layer);
        let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
        let mut live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
        );
        let mut list = DrawList::new(DrawOptions::default());
        let diff = list.refresh(&Scene::new(live.stage(), &store));
        assert_eq!(diff.work.triangulations, 1, "planned once, with st1");
        assert_eq!(diff.work.materials_read, 1, "read once");
        let item = list.items().next().unwrap();
        let buffers = &list.buffers(item.buffer).unwrap().buffers;
        assert_eq!(buffers.uv_sets.len(), 1);
        assert_eq!(buffers.uv_sets[0].name, "st1");
        // The second face's st1 differs at the shared corners: six
        // vertices, not four.
        assert_eq!(buffers.positions.len(), 6);
        let at = |position: [f32; 3], face: u32| {
            (0..buffers.positions.len())
                .find(|&v| {
                    buffers.positions[v] == position
                        && buffers.indices[3 * face as usize..3 * face as usize + 3]
                            .contains(&u32::try_from(v).unwrap())
                })
                .unwrap()
        };
        assert_eq!(buffers.uv_sets[0].values[at([1., 1., 0.], 0)], [2., 2.]);
        assert_eq!(buffers.uv_sets[0].values[at([1., 1., 0.], 1)], [5., 5.]);
        assert_eq!(
            buffers.st.as_ref().unwrap()[at([1., 1., 0.], 1)],
            [1., 1.],
            "st drawn as well"
        );
        let report = list.buffers(item.buffer).unwrap().report().unwrap();
        assert_eq!(report.uv_sets.len(), 1);

        let key = item.buffer;
        let coordinates = store.tokens().intern("inputs:coordinates");
        let mut back = Transaction::new();
        back.set_default(
            EditTarget::for_layer(LayerId(1)).property(layerstack::PropertyPath::new(
                store.path("/Looks/Tiled"),
                coordinates,
            )),
            Value::Token(store.tokens().intern("st")),
        );
        let applied = live.apply(&mut store, &back).unwrap();
        let diff = list.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
        assert_eq!(diff.materials_changed, [store.path("/Looks/Tiled")]);
        assert_eq!(diff.rebuilt, [key]);
        assert_eq!(diff.work.triangulations, 1);
        let buffers = &list.buffers(key).unwrap().buffers;
        assert!(buffers.uv_sets.is_empty());
        assert_eq!(buffers.positions.len(), 4);
    }

    /// `text` composed with the OpenUSD schemas.
    fn compose(text: &str) -> (InMemoryStore, LiveStage) {
        let mut store = InMemoryStore::default();
        let read = layerstack_usda::read_usda(
            text,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        assert!(
            !read.has_errors(),
            "{:?}",
            read.diagnostics().collect::<Vec<_>>()
        );
        store.insert_layer(read.emitted.layer);
        let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
        let live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
        );
        (store, live)
    }

    /// A material reading the primvar `varname` through a texture.
    fn reading(name: &str, varname: &str) -> alloc::string::String {
        alloc::format!(
            r#"
    def Material "{name}" {{
        token outputs:surface.connect = </Looks/{name}/Surface.outputs:surface>
        {varname}
        def Shader "Surface" {{
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/{name}/Texture.outputs:rgb>
            token outputs:surface
        }}
        def Shader "Texture" {{
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @tile.png@
            float2 inputs:st.connect = </Looks/{name}/Reader.outputs:result>
            float3 outputs:rgb
        }}
        def Shader "Reader" {{
            uniform token info:id = "UsdPrimvarReader_float2"
            string inputs:varname.connect = </Looks/{name}.inputs:coordinates>
            float2 outputs:result
        }}
    }}"#
        )
    }

    /// Two instances of one card, bound to materials reading different
    /// sets: two prototype groups drawing one buffer, planned once with
    /// both sets on the diff that adds them.
    #[test]
    fn a_prototypes_groups_plan_their_mesh_once_with_every_set() {
        let text = alloc::format!(
            r#"#usda 1.0
def Xform "World" {{
    def Xform "North" (
        instanceable = true
        prepend references = </Card>
        prepend apiSchemas = ["MaterialBindingAPI"]
    ) {{
        rel material:binding = </Looks/Near>
    }}
    def Xform "South" (
        instanceable = true
        prepend references = </Card>
        prepend apiSchemas = ["MaterialBindingAPI"]
    ) {{
        rel material:binding = </Looks/Far>
    }}
}}
class Xform "Card" {{
    def Mesh "Face" {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        texCoord2f[] primvars:st1 = [(0, 0), (2, 0), (2, 2), (0, 2)] (
            interpolation = "vertex"
        )
        texCoord2f[] primvars:st2 = [(0, 0), (3, 0), (3, 3), (0, 3)] (
            interpolation = "vertex"
        )
    }}
}}
def Scope "Looks" {{{}{}
}}
"#,
            reading("Near", r#"token inputs:coordinates = "st1""#),
            reading("Far", r#"token inputs:coordinates = "st2""#),
        );
        let (store, live) = compose(&text);
        let mut list = DrawList::new(DrawOptions::default());
        let diff = list.refresh(&Scene::new(live.stage(), &store));
        assert_eq!(diff.added.len(), 2, "one group per material");
        assert_eq!(diff.work.triangulations, 1, "planned once");
        assert!(diff.rebuilt.is_empty());
        let mut items = list.items();
        let (first, second) = (items.next().unwrap(), items.next().unwrap());
        assert_eq!(first.buffer, second.buffer);
        let buffers = &list.buffers(first.buffer).unwrap().buffers;
        let names: Vec<&str> = buffers
            .uv_sets
            .iter()
            .map(|set| set.name.as_str())
            .collect();
        assert_eq!(names, ["st1", "st2"]);
    }

    /// A `varname` that varies over time: `set_time` reads the material
    /// again and plans the mesh again with the set it names, and the set's
    /// own time samples then refill it.
    #[test]
    fn set_time_follows_a_varname_that_varies() {
        let text = alloc::format!(
            r#"#usda 1.0
def Mesh "Plate" (
    prepend apiSchemas = ["MaterialBindingAPI"]
) {{
    int[] faceVertexCounts = [4]
    int[] faceVertexIndices = [0, 1, 2, 3]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    texCoord2f[] primvars:st1 (
        interpolation = "vertex"
    )
    texCoord2f[] primvars:st1.timeSamples = {{
        0: [(0, 0), (2, 0), (2, 2), (0, 2)],
        20: [(0, 0), (4, 0), (4, 4), (0, 4)],
    }}
    rel material:binding = </Looks/Shifting>
}}
def Scope "Looks" {{{}
}}
"#,
            reading(
                "Shifting",
                r#"token inputs:coordinates.timeSamples = {0: "st", 10: "st1"}"#
            ),
        );
        let (store, live) = compose(&text);
        let scene = Scene::new(live.stage(), &store);
        let mut list = DrawList::new(DrawOptions {
            time: Time::at(0.0),
            ..DrawOptions::default()
        });
        list.refresh(&scene);
        let key = list.items().next().unwrap().buffer;
        assert!(list.buffers(key).unwrap().buffers.uv_sets.is_empty());

        let diff = list.set_time(&scene, Time::at(10.0));
        assert_eq!(diff.materials_changed, [store.path("/Looks/Shifting")]);
        assert_eq!(diff.rebuilt, [key]);
        assert_eq!(diff.work.triangulations, 1);
        let buffers = &list.buffers(key).unwrap().buffers;
        assert_eq!(buffers.uv_sets[0].name, "st1");
        assert_eq!(
            buffers.uv_sets[0].values[2],
            [3., 3.],
            "st1 between samples"
        );

        // `st1` is drawn now, and its samples make the mesh read again.
        let diff = list.set_time(&scene, Time::at(20.0));
        assert_eq!(diff.refilled, [key]);
        assert_eq!(diff.work.triangulations, 0);
        let buffers = &list.buffers(key).unwrap().buffers;
        assert_eq!(buffers.uv_sets[0].values[2], [4., 4.]);

        // And back: planned again without it.
        let diff = list.set_time(&scene, Time::at(0.0));
        assert_eq!(diff.rebuilt, [key]);
        assert!(list.buffers(key).unwrap().buffers.uv_sets.is_empty());
    }

    #[test]
    fn an_instancer_edit_only_changes_matrices() {
        let (mut fixture, _) = Fixture::new();
        let stones = fixture.item("/World/Stones/Prototypes/Stone");
        let path = fixture.path("/World/Stones");
        let diff = fixture.edit(|edit| {
            PointInstancerEdit::new(edit, path)
                .unwrap()
                .unwrap()
                .set_positions(edit, &[[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 2.0, 0.0]]);
        });
        assert_eq!(diff.matrices, [stones]);
        assert!(diff.added.is_empty() && diff.removed.is_empty() && diff.rebound.is_empty());
        assert!(diff.buffers_added.is_empty() && diff.refilled.is_empty());
        assert_eq!(diff.work.triangulations + diff.work.tessellations, 0);
        assert_eq!(diff.work.items_read, 1);
        assert_eq!(diff.work.matrices_computed, 3);
        let item = fixture.list.item(stones).unwrap();
        assert_eq!(item.matrices[2][3], [0.0, 2.0, 0.0, 1.0]);
        // Hiding one instance drops its matrix.
        let diff = fixture.edit(|edit| {
            PointInstancerEdit::new(edit, path)
                .unwrap()
                .unwrap()
                .make_invisible(edit, &[1])
                .unwrap();
        });
        assert_eq!(diff.matrices, [stones]);
        assert!(diff.visibility.is_empty());
        assert_eq!(fixture.list.item(stones).unwrap().ids, [0, 2]);
    }

    #[test]
    fn moving_an_instance_recomputes_its_groups_matrices_only() {
        let (mut fixture, _) = Fixture::new();
        let post = fixture.item("/World/Beacons/North/Post");
        let bulb = fixture.item("/World/Beacons/North/Bulb");
        let north = fixture.path("/World/Beacons/North");
        let diff = fixture.edit(|edit| {
            PrimEdit::from_path_unchecked(north).write_value(
                edit,
                "xformOp:translate",
                Value::Vec3d([5.0, 0.0, 0.0]),
            );
        });
        let mut expected = [post, bulb];
        expected.sort();
        assert_eq!(diff.matrices, expected);
        assert!(diff.visibility.is_empty() && diff.rebound.is_empty());
        assert_eq!(diff.work.triangulations + diff.work.tessellations, 0);
        assert_eq!(diff.work.items_read, 2);
        assert_eq!(diff.work.local_transforms, 1, "the north beacon's alone");
        assert_eq!(
            fixture.list.item(post).unwrap().matrices[0][3],
            [5.0, 0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn binding_a_material_to_one_instance_splits_its_groups() {
        let (mut fixture, _) = Fixture::new();
        let post = fixture.item("/World/Beacons/North/Post");
        let bulb = fixture.item("/World/Beacons/North/Bulb");
        let south = fixture.path("/World/Beacons/South");
        let red = fixture.path("/Looks/Red");
        let diff = fixture.edit(|edit| {
            MaterialBindingApi::apply(edit, south)
                .unwrap()
                .bind(edit, red, None, &MaterialPurpose::All)
                .unwrap();
        });
        // The north beacon's groups keep only it; the south beacon's post
        // and bulb are new groups bound to red, drawing the same buffers.
        let mut moved = [post, bulb];
        moved.sort();
        assert_eq!(diff.matrices, moved);
        assert_eq!(diff.added.len(), 2);
        assert!(diff.buffers_added.is_empty() && diff.freed.is_empty());
        assert_eq!(diff.work.triangulations + diff.work.tessellations, 0);
        for id in &diff.added {
            let item = fixture.list.item(*id).unwrap();
            assert_eq!(item.material, Some(red));
            assert_eq!(item.instances.len(), 1);
            assert!(
                fixture
                    .list
                    .items()
                    .any(|other| other.id != *id && other.buffer == item.buffer)
            );
        }
        assert_eq!(fixture.list.item(post).unwrap().instances.len(), 1);
    }

    #[test]
    fn hiding_a_prim_keeps_its_buffer_and_lists_its_visibility() {
        let (mut fixture, _) = Fixture::new();
        let pebble = fixture.item("/World/Rocks/Pebble");
        let rocks = fixture.path("/World/Rocks");
        let invisible = fixture.store.tokens().intern("invisible");
        let diff = fixture.edit(|edit| {
            PrimEdit::from_path_unchecked(rocks).write_value(
                edit,
                "visibility",
                Value::Token(invisible),
            );
        });
        // Everything under the rocks: the rock, the pebble and the stone.
        assert_eq!(diff.visibility.len(), 3);
        assert_eq!(diff.matrices, diff.visibility);
        assert!(diff.freed.is_empty());
        assert!(fixture.list.item(pebble).unwrap().hidden());
        assert_eq!(diff.work.triangulations + diff.work.tessellations, 0);
        assert_eq!(diff.work.items_untouched, 3);
    }

    #[test]
    fn a_resync_rebuilds_only_its_subtree() {
        let (mut fixture, _) = Fixture::new();
        let rocks = fixture.path("/World/Rocks");
        let before: Vec<ItemId> = fixture.list.items().map(|item| item.id).collect();
        let mut off = Transaction::new();
        off.set_active(EditTarget::for_layer(LayerId(1)).prim(rocks), false);
        let applied = fixture.live.apply(&mut fixture.store, &off).unwrap();
        assert_eq!(applied.changes.resynced, [rocks]);
        let scene = Scene::new(fixture.live.stage(), &fixture.store);
        let diff = fixture.list.apply_changes(&scene, &applied.changes);
        // The rock, the pebble and the stone are gone with their buffers
        // (the mesh's and the shared sphere's); the rest is untouched.
        assert_eq!(diff.removed.len(), 3);
        assert_eq!(diff.freed.len(), 2);
        assert_eq!(diff.work.items_untouched, 3);
        assert_eq!(fixture.list.len(), 3);
        let diff = fixture.apply(&applied.inverse);
        assert_eq!(diff.added.len(), 3);
        assert_eq!(diff.work.triangulations, 1);
        assert_eq!(diff.work.tessellations, 1);
        assert_eq!(diff.work.items_untouched, 3);
        // Items read again keep nothing of the old ones' ids.
        assert!(diff.added.iter().all(|id| !before.contains(id)));
    }

    #[test]
    fn a_points_edit_refills_and_a_topology_edit_plans_again() {
        let (mut fixture, _) = Fixture::new();
        let rock = fixture.path("/World/Rocks/Rock");
        let buffer = fixture
            .list
            .item(fixture.item("/World/Rocks/Rock"))
            .unwrap()
            .buffer;
        let diff = fixture.edit(|edit| {
            MeshEdit::new(edit, rock).unwrap().unwrap().set_points_at(
                edit,
                1.0,
                &[
                    [0.0, 0.0, 0.0],
                    [2.0, 0.0, 0.0],
                    [2.0, 2.0, 0.0],
                    [0.0, 2.0, 0.0],
                ],
            );
        });
        assert_eq!(diff.refilled, [buffer]);
        assert_eq!(diff.work.triangulations, 0);
        assert_eq!(diff.work.items_read, 1);
        let diff = fixture.edit(|edit| {
            MeshEdit::new(edit, rock)
                .unwrap()
                .unwrap()
                .set_face_vertex_indices(edit, &[3, 2, 1, 0]);
        });
        assert_eq!(diff.rebuilt, [buffer]);
        assert!(diff.refilled.is_empty());
        assert_eq!(diff.work.triangulations, 1);
        assert_eq!(fixture.list.buffers(buffer).unwrap().revision, 2);
    }
}
