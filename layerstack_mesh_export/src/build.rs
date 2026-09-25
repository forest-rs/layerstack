// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Mapping from the mesh description to an authored USDA document.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use layerstack_usda::writer::{
    Attribute, Document, LayerOffset, ListOp, Metadatum, Prim, Property, Reference, Relationship,
    Specifier, Value,
};

use crate::{
    CustomAttribute, ExportError, Faces, FamilyType, INSTANCE_ID, INSTANCE_NAMES, Instance,
    InstancerProblem, Instancing, Interpolation, MATERIALS_SCOPE, Material, MaterialSubset, Mesh,
    MeshProblem, Node, Orientation, PROTOTYPES_SCOPE, PointInstancer, Primvar, PrimvarData, Scene,
    Transform, UpAxis, Xform,
};

/// What prims need to know about the rest of the scene.
struct Context<'s, 'a> {
    /// The scene's materials, which meshes bind by name.
    materials: &'s [Material<'a>],
    /// Prim path of the materials scope.
    materials_scope: String,
    /// The scene's shared prototypes, which instances place by name.
    prototypes: &'s [Node<'a>],
    /// Prim path of the shared prototypes' `class` prim.
    prototypes_scope: String,
    /// What the document is written for.
    target: Target,
}

/// What a document is written for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Target {
    /// How point instancers are written.
    pub(crate) instancing: Instancing,
    /// Whether instance prims are marked `instanceable`.
    pub(crate) instanceable: bool,
}

impl Target {
    /// A layer for USD readers in general: point instancers as the scene
    /// asks, instances shared through scene graph instancing.
    pub(crate) fn stage(instancing: Instancing) -> Self {
        Self {
            instancing,
            instanceable: true,
        }
    }

    /// The root layer of a [`UsdzProfile::Arkit`](crate::UsdzProfile::Arkit)
    /// package: instanced references, since Apple's stack does not draw
    /// `PointInstancer` instances, and no `instanceable` metadata, since
    /// its importer (`ModelIO`, under `SceneKit` and AR Quick Look) also
    /// draws each instancing prototype (`/__Prototype_N`) once more, where
    /// the prototype is defined.
    pub(crate) const ARKIT: Self = Self {
        instancing: Instancing::References,
        instanceable: false,
    };
}

impl Context<'_, '_> {
    /// The shared prototype named `name`, if the scene defines one.
    fn prototype(&self, name: &str) -> Option<&Node<'_>> {
        self.prototypes.iter().find(|p| p.name() == name)
    }
}

pub(crate) fn document(scene: &Scene<'_>, target: Target) -> Result<Document, ExportError> {
    let stage = scene.stage;
    if !(stage.meters_per_unit.is_finite() && stage.meters_per_unit > 0.0) {
        return Err(ExportError::InvalidStage);
    }
    // Stage metadata: `metersPerUnit` and `upAxis` are layer metadata on
    // the root layer (`pxr/usd/usdGeom/metrics.h:23`, `:107`). The root prim is the
    // `defaultPrim` so references to the file need no prim path (AOUSD
    // Core §7.6.1.2.3).
    let up_axis = match stage.up_axis {
        UpAxis::Y => "Y",
        UpAxis::Z => "Z",
    };
    let cx = Context {
        materials: &scene.materials,
        materials_scope: format!("/{}/{MATERIALS_SCOPE}", scene.root.name),
        prototypes: &scene.prototypes,
        prototypes_scope: format!("/{}/{PROTOTYPES_SCOPE}", scene.root.name),
        target,
    };
    check_prototype_cycles(&cx)?;
    // Materials live inside the root prim, so the `defaultPrim` carries
    // them into any referencing stage and bindings never point outside
    // the asset.
    let mut scope = Prim::def("Scope", MATERIALS_SCOPE);
    for material in &scene.materials {
        scope.children.push(crate::shading::material_prim(
            material,
            &cx.materials_scope,
        )?);
    }
    let mut root = xform_prim(&scene.root, "", &cx)?;
    // Shared prototypes live in one `class` prim inside the root, for the
    // same reason; being abstract, they are not drawn there (AOUSD Core
    // §7.6, §12.2.1), only through the instances that reference them.
    if !scene.prototypes.is_empty() {
        let mut class = Prim::new(Specifier::Class, None, PROTOTYPES_SCOPE);
        for prototype in &scene.prototypes {
            class
                .children
                .push(node_prim(prototype, &cx.prototypes_scope, &cx)?);
        }
        root.children.push(class);
    }
    if !scope.children.is_empty() {
        root.children.push(scope);
    }
    Ok(Document {
        default_prim: Some(scene.root.name.to_string()),
        metadata: vec![
            Metadatum::new("metersPerUnit", Value::Double(stage.meters_per_unit)),
            Metadatum::new("upAxis", Value::Token(up_axis.into())),
        ],
        prim_order: None,
        sublayers: Vec::new(),
        prims: vec![root],
    })
}

/// Rejects shared prototypes that place themselves, directly or through
/// other shared prototypes: the references would form a cycle, which
/// composition rejects (AOUSD Core §10.3.2.1).
fn check_prototype_cycles(cx: &Context<'_, '_>) -> Result<(), ExportError> {
    fn visit<'n>(
        node: &'n Node<'_>,
        cx: &'n Context<'_, '_>,
        stack: &mut Vec<&'n str>,
    ) -> Result<(), ExportError> {
        let children: &[Node<'_>] = match node {
            Node::Xform(x) => &x.children,
            Node::PointInstancer(p) => &p.prototypes,
            Node::Mesh(_) => &[],
            Node::Instance(instance) => {
                let name = &*instance.prototype;
                if stack.contains(&name) {
                    return Err(ExportError::PrototypeCycle {
                        prototype: name.into(),
                    });
                }
                // Unknown names are reported where the instance is written.
                if let Some(prototype) = cx.prototype(name) {
                    stack.push(name);
                    visit(prototype, cx, stack)?;
                    stack.pop();
                }
                return Ok(());
            }
        };
        children
            .iter()
            .try_for_each(|child| visit(child, cx, stack))
    }
    for prototype in cx.prototypes {
        visit(prototype, cx, &mut vec![prototype.name()])?;
    }
    Ok(())
}

fn xform_prim(xform: &Xform<'_>, parent: &str, cx: &Context<'_, '_>) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", xform.name);
    let mut prim = Prim::def("Xform", &*xform.name);
    if let Some(kind) = &xform.kind {
        prim.metadata
            .push(Metadatum::new("kind", Value::Token(kind.to_string())));
    }
    let mut attrs = Vec::new();
    push_transform(&mut attrs, xform.transform);
    push_custom(&mut attrs, &xform.attributes);
    prim.properties
        .extend(attrs.into_iter().map(Property::Attribute));
    for child in &xform.children {
        prim.children.push(node_prim(child, &path, cx)?);
    }
    Ok(prim)
}

fn node_prim(node: &Node<'_>, parent: &str, cx: &Context<'_, '_>) -> Result<Prim, ExportError> {
    match node {
        Node::Xform(x) => xform_prim(x, parent, cx),
        Node::Mesh(m) => mesh_prim(m, parent, cx),
        Node::PointInstancer(p) => match cx.target.instancing {
            Instancing::PointInstancers => instancer_prim(p, parent, cx),
            Instancing::References => referenced_instancer_prim(p, parent, cx),
        },
        Node::Instance(i) => instance_prim(i, parent, cx),
    }
}

/// A point instancer written as instanced references
/// ([`Instancing::References`]): an `Xform` with the instancer's transform,
/// constant primvars and custom attributes, holding its own prototypes in
/// a `class` prim (abstract, so not drawn in place; AOUSD Core §7.6,
/// §12.2.1) and one [`reference_prim`] per instance.
///
/// Each instance's single `xformOp:transform` is what
/// `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime` computes for
/// it, prototype root transform included ("Computing an Instance
/// Transform", `pxr/usd/usdGeom/pointInstancer.h`): it replaces the
/// prototype root's own op, which the reference would otherwise bring.
/// Per-instance primvars become constant primvars on each instance, which
/// its geometry inherits, as `UsdGeomPointInstancer` applies them
/// ("Primvars on `PointInstancer`").
fn referenced_instancer_prim(
    instancer: &PointInstancer<'_>,
    parent: &str,
    cx: &Context<'_, '_>,
) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", instancer.name);
    let checked =
        crate::instancer::check(instancer).map_err(|problem| ExportError::InvalidInstancer {
            path: path.clone(),
            problem,
        })?;
    let class_path = format!("{path}/{PROTOTYPES_SCOPE}");
    let mut class = Prim::new(Specifier::Class, None, PROTOTYPES_SCOPE);
    // Where each prototype's instances point, the transform of its root
    // (which their own op replaces), and, for an `Instance` prototype, the
    // instance whose primvars and attributes they carry.
    let mut targets = Vec::with_capacity(instancer.prototypes.len());
    for prototype in &instancer.prototypes {
        let root = crate::instancer::local_transform(prototype, cx.prototypes);
        if let Node::Instance(instance) = prototype {
            // Shared: reference it where it is defined.
            let target = format!("{}/{}", cx.prototypes_scope, instance.prototype);
            if cx.prototype(&instance.prototype).is_none() {
                return Err(ExportError::UnknownPrototype {
                    path: format!("{class_path}/{}", instance.name),
                    prototype: instance.prototype.to_string(),
                });
            }
            let referenced = cx.prototype(&instance.prototype).unwrap_or(prototype);
            targets.push((target, root, Some(instance), referenced));
        } else {
            class.children.push(node_prim(prototype, &class_path, cx)?);
            targets.push((
                format!("{class_path}/{}", prototype.name()),
                root,
                None,
                prototype,
            ));
        }
    }

    let mut group = Prim::def("Xform", &*instancer.name);
    let mut attrs = Vec::new();
    for custom in &instancer.primvars {
        if custom.primvar.interpolation == Interpolation::Constant {
            push_primvar_unchecked(
                &mut attrs,
                &format!("primvars:{}", custom.name),
                &custom.primvar,
            );
        }
    }
    push_transform(&mut attrs, instancer.transform);
    push_custom(&mut attrs, &instancer.attributes);
    group
        .properties
        .extend(attrs.into_iter().map(Property::Attribute));
    if !class.children.is_empty() {
        group.children.push(class);
    }

    let per_instance: Vec<_> = instancer
        .primvars
        .iter()
        .filter(|p| p.primvar.interpolation != Interpolation::Constant)
        .collect();
    for (instance, &index) in checked.proto_indices.iter().enumerate() {
        let (target, root, shared, referenced) = &targets[index as usize];
        let name = match &instancer.names {
            Some(names) => names[instance].to_string(),
            None => format!("{}_{instance}", instancer.prototypes[index as usize].name()),
        };
        let placed = crate::instancer::instance_matrix(
            instancer.positions[instance],
            checked.rotations.as_ref().map(|r| r[instance]),
            instancer.scales.as_deref().map(|s| s[instance]),
        );
        let matrix = match root {
            Some(root) => crate::instancer::mul(&root.usd_rows(), &placed),
            None => placed,
        };
        let mut attrs = Vec::new();
        for custom in &per_instance {
            let primvar = &custom.primvar;
            let element = primvar
                .indices
                .as_deref()
                .map_or(instance, |indices| indices[instance] as usize);
            let name = format!("primvars:{}", custom.name);
            attrs.push(
                Attribute::new(
                    name.clone(),
                    primvar.values.type_name(),
                    primvar.values.element(element),
                )
                .with_metadata("interpolation", Value::Token("constant".into())),
            );
            block_inherited_indices(&mut attrs, &name, referenced, cx);
        }
        // An `Instance` prototype's own primvars apply to every instance of
        // it, unless the instancer gives the instance its own.
        if let Some(shared) = shared {
            for custom in &shared.primvars {
                if !per_instance.iter().any(|p| p.name == custom.name) {
                    let name = format!("primvars:{}", custom.name);
                    push_primvar_unchecked(&mut attrs, &name, &custom.primvar);
                    if custom.primvar.indices.is_none() {
                        block_inherited_indices(&mut attrs, &name, referenced, cx);
                    }
                }
            }
        }
        push_transform(&mut attrs, Some(Transform::from_usd_rows(matrix)));
        if let Some(shared) = shared {
            push_custom(&mut attrs, &shared.attributes);
        }
        if let Some(ids) = &instancer.ids {
            attrs.push(Attribute::new(INSTANCE_ID, "int64", Value::Int64(ids[instance])).custom());
        }
        group
            .children
            .push(reference_prim(&name, target.clone(), attrs, cx));
    }
    Ok(group)
}

/// An [`Instance`]: a typeless, instanceable prim with an internal
/// reference to a shared prototype (AOUSD Core §10.3.2.1, §11.3.3). Its
/// transform, when given, is authored after the prototype root's own.
fn instance_prim(
    instance: &Instance<'_>,
    parent: &str,
    cx: &Context<'_, '_>,
) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", instance.name);
    let prototype =
        cx.prototype(&instance.prototype)
            .ok_or_else(|| ExportError::UnknownPrototype {
                path: path.clone(),
                prototype: instance.prototype.to_string(),
            })?;
    for primvar in &instance.primvars {
        crate::instancer::check_primvar(primvar, 1)
            .and_then(|()| {
                if primvar.primvar.interpolation == Interpolation::Constant {
                    Ok(())
                } else {
                    Err(InstancerProblem::PrimvarInterpolation {
                        name: format!("primvars:{}", primvar.name),
                        interpolation: primvar.primvar.interpolation,
                    })
                }
            })
            .map_err(|problem| ExportError::InvalidInstancer {
                path: path.clone(),
                problem,
            })?;
    }
    // Only an own transform is authored; without one, the reference brings
    // the prototype root's.
    let transform = instance.transform.and_then(|_| {
        crate::instancer::instance_transform(&instance.prototype, instance.transform, cx.prototypes)
    });
    let mut attrs = Vec::new();
    for custom in &instance.primvars {
        let name = format!("primvars:{}", custom.name);
        push_primvar_unchecked(&mut attrs, &name, &custom.primvar);
        if custom.primvar.indices.is_none() {
            block_inherited_indices(&mut attrs, &name, prototype, cx);
        }
    }
    push_transform(&mut attrs, transform);
    push_custom(&mut attrs, &instance.attributes);
    Ok(reference_prim(
        &instance.name,
        format!("{}/{}", cx.prototypes_scope, instance.prototype),
        attrs,
        cx,
    ))
}

/// Blocks `<name>:indices` (`= None`, AOUSD Core §12.3) on a prim that
/// references `referenced` and authors the unindexed primvar `name`, when
/// the reference would otherwise bring indices for it: the two are
/// separate attributes, so the weaker indices would index the stronger
/// values (`pxr/usd/usdGeom/primvar.h`, "Indexed Primvars").
fn block_inherited_indices(
    attrs: &mut Vec<Attribute>,
    name: &str,
    referenced: &Node<'_>,
    cx: &Context<'_, '_>,
) {
    let primvar = name.strip_prefix("primvars:").unwrap_or(name);
    if crate::instancer::passes_indices(referenced, primvar, cx.prototypes) {
        attrs.push(Attribute::new(
            format!("{name}:indices"),
            "int[]",
            Value::Block,
        ));
    }
}

/// A typeless `def` prim named `name`, with one internal reference to
/// `target` and the attributes `attrs`, and marked `instanceable` unless
/// the target says otherwise ([`Target::ARKIT`]). Its type comes through
/// the reference: a type of its own (such as `Xform`) would be stronger
/// than the prototype's `Mesh` and hide its geometry.
fn reference_prim(name: &str, target: String, attrs: Vec<Attribute>, cx: &Context<'_, '_>) -> Prim {
    let mut prim = Prim::new(Specifier::Def, None, name);
    if cx.target.instanceable {
        prim.metadata
            .push(Metadatum::new("instanceable", Value::Bool(true)));
    }
    prim.references = Some(ListOp::prepend(vec![Reference {
        asset: None,
        prim_path: Some(target),
        offset: LayerOffset::default(),
    }]));
    prim.properties
        .extend(attrs.into_iter().map(Property::Attribute));
    prim
}

/// A `PointInstancer` prim (`pxr/usd/usdGeom/pointInstancer.h`) with its
/// prototypes in a `Prototypes` scope below it, targeted in order by the
/// `prototypes` relationship.
fn instancer_prim(
    instancer: &PointInstancer<'_>,
    parent: &str,
    cx: &Context<'_, '_>,
) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", instancer.name);
    let checked =
        crate::instancer::check(instancer).map_err(|problem| ExportError::InvalidInstancer {
            path: path.clone(),
            problem,
        })?;
    let scope_path = format!("{path}/{PROTOTYPES_SCOPE}");
    let mut scope = Prim::def("Scope", PROTOTYPES_SCOPE);
    let mut targets = Vec::with_capacity(instancer.prototypes.len());
    for prototype in &instancer.prototypes {
        scope.children.push(node_prim(prototype, &scope_path, cx)?);
        targets.push(format!("{scope_path}/{}", prototype.name()));
    }

    let mut attrs = Vec::new();
    // Prototype bounds come from the validated meshes built above.
    if let Some([lo, hi]) = crate::instancer::extent(instancer, &checked, cx.prototypes) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "`extent` is `float3[]`; OpenUSD rounds its double range the same way"
        )]
        let extent = vec![lo.map(|c| c as f32), hi.map(|c| c as f32)];
        attrs.push(Attribute::new(
            "extent",
            "float3[]",
            Value::Float3Array(extent),
        ));
    }
    if let Some(ids) = &instancer.ids {
        attrs.push(Attribute::new(
            "ids",
            "int64[]",
            Value::Int64Array(ids.to_vec()),
        ));
    }
    // `orientationsf` wins over `orientations` wherever it is known
    // (`UsdGeomPointInstancer::UsesOrientationsf`); see
    // `OrientationPrecision` for which are written.
    if let Some(orientations) = checked.half_orientations {
        attrs.push(Attribute::new(
            "orientations",
            "quath[]",
            Value::QuathArray(orientations),
        ));
    }
    if let Some(orientations) = instancer
        .orientations
        .as_deref()
        .filter(|_| instancer.orientation_precision.writes_float())
    {
        attrs.push(Attribute::new(
            "orientationsf",
            "quatf[]",
            Value::QuatfArray(orientations.to_vec()),
        ));
    }
    attrs.push(Attribute::new(
        "positions",
        "point3f[]",
        Value::Float3Array(instancer.positions.to_vec()),
    ));
    // Instance primvars, checked above: `vertex` is one element per
    // instance (`pxr/usd/usdGeom/pointInstancer.h`, "Primvars on
    // PointInstancer").
    for custom in &instancer.primvars {
        push_primvar_unchecked(
            &mut attrs,
            &format!("primvars:{}", custom.name),
            &custom.primvar,
        );
    }
    attrs.push(Attribute::new(
        "protoIndices",
        "int[]",
        Value::IntArray(checked.proto_indices),
    ));
    if let Some(scales) = &instancer.scales {
        attrs.push(Attribute::new(
            "scales",
            "float3[]",
            Value::Float3Array(scales.to_vec()),
        ));
    }
    push_transform(&mut attrs, instancer.transform);
    push_custom(&mut attrs, &instancer.attributes);
    if let Some(names) = &instancer.names {
        attrs.push(
            Attribute::new(
                INSTANCE_NAMES,
                "token[]",
                Value::TokenArray(names.iter().map(|n| n.to_string()).collect()),
            )
            .custom(),
        );
    }

    let mut prim = Prim::def("PointInstancer", &*instancer.name);
    prim.properties
        .extend(attrs.into_iter().map(Property::Attribute));
    // `prototypes` is an ordered target list: a prototype's position in it
    // is the value `protoIndices` uses for it.
    prim.push_property(Relationship {
        name: "prototypes".into(),
        custom: false,
        targets: Some(ListOp::explicit(targets)),
        metadata: Vec::new(),
    });
    prim.children.push(scope);
    Ok(prim)
}

/// Element counts for each interpolation on one mesh.
#[derive(Clone, Copy)]
struct Sites {
    points: usize,
    faces: usize,
    corners: usize,
}

impl Sites {
    /// Spec: `pxr/usd/usdGeom/mesh.h:75` — constant: 1; uniform: one
    /// per face; varying/vertex: one per point; faceVarying: one per face
    /// corner (<https://openusd.org/dev/api/class_usd_geom_primvar.html>).
    fn count(self, interpolation: Interpolation) -> usize {
        match interpolation {
            Interpolation::Constant => 1,
            Interpolation::Uniform => self.faces,
            Interpolation::Varying | Interpolation::Vertex => self.points,
            Interpolation::FaceVarying => self.corners,
        }
    }
}

fn mesh_prim(mesh: &Mesh<'_>, parent: &str, cx: &Context<'_, '_>) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", mesh.name);
    let fail = |problem| ExportError::InvalidMesh {
        path: path.clone(),
        problem,
    };

    let (counts, indices) = topology(&mesh.faces, mesh.points.len()).map_err(fail)?;
    let sites = Sites {
        points: mesh.points.len(),
        faces: counts.len(),
        corners: indices.len(),
    };

    let mut attrs = Vec::new();
    // `extent` is required on boundable prims for correct culling and
    // bounds; it is the local-space AABB of `points`, before this prim's
    // own transform (`pxr/usd/usdGeom/boundable.h:42`). Empty meshes have
    // no extent.
    if let Some(extent) = extent(&mesh.points).map_err(fail)? {
        attrs.push(Attribute::new(
            "extent",
            "float3[]",
            Value::Float3Array(extent.to_vec()),
        ));
    }
    attrs.push(Attribute::new(
        "faceVertexCounts",
        "int[]",
        Value::IntArray(counts),
    ));
    attrs.push(Attribute::new(
        "faceVertexIndices",
        "int[]",
        Value::IntArray(indices),
    ));

    if let Some(normals) = &mesh.normals {
        let values = Value::Float3Array(normals.values.to_vec());
        match normals.indices {
            // `normals` is a plain attribute with an `interpolation`, not a
            // primvar, so it cannot be indexed (`pxr/usd/usdGeom/pointBased.h`).
            None => {
                check_primvar("normals", normals.values.len(), normals, sites).map_err(fail)?;
                attrs.push(
                    Attribute::new("normals", "normal3f[]", values).with_metadata(
                        "interpolation",
                        Value::Token(normals.interpolation.token().into()),
                    ),
                );
            }
            // Indexed normals go to `primvars:normals`, which takes
            // precedence over `normals` (`pxr/usd/usdGeom/pointBased.h:204`).
            Some(_) => push_primvar(
                &mut attrs,
                "primvars:normals",
                "normal3f[]",
                values,
                normals.values.len(),
                normals,
                sites,
            )
            .map_err(fail)?,
        }
    }

    // `orientation` is authored even when it equals the `rightHanded`
    // fallback, so the winding convention is explicit in the file
    // (`pxr/usd/usdGeom/gprim.h:212`).
    let orientation = match mesh.orientation {
        Orientation::RightHanded => "rightHanded",
        Orientation::LeftHanded => "leftHanded",
    };
    attrs.push(Attribute::new("orientation", "token", Value::Token(orientation.into())).uniform());
    if mesh.double_sided {
        attrs.push(Attribute::new("doubleSided", "bool", Value::Bool(true)).uniform());
    }
    attrs.push(Attribute::new(
        "points",
        "point3f[]",
        Value::Float3Array(mesh.points.to_vec()),
    ));

    if let Some(uvs) = &mesh.uvs {
        // The conventional primary UV set is the `st` primvar of role
        // `texCoord2f` (`pxr/usd/usdGeom/primvar.h`).
        push_primvar(
            &mut attrs,
            "primvars:st",
            "texCoord2f[]",
            Value::Float2Array(uvs.values.to_vec()),
            uvs.values.len(),
            uvs,
            sites,
        )
        .map_err(fail)?;
    }
    for custom in &mesh.primvars {
        let data = &custom.primvar.values;
        push_primvar(
            &mut attrs,
            &format!("primvars:{}", custom.name),
            data.type_name(),
            data.to_value(),
            data.len(),
            &custom.primvar,
            sites,
        )
        .map_err(fail)?;
    }

    // The schema fallback is `catmullClark`; polygonal kernel output must
    // opt out explicitly or consumers will smooth it (`pxr/usd/usdGeom/mesh.h:62`).
    attrs.push(Attribute::new("subdivisionScheme", "token", Value::Token("none".into())).uniform());
    push_transform(&mut attrs, mesh.transform);
    push_custom(&mut attrs, &mesh.attributes);

    let mut prim = Prim::def("Mesh", &*mesh.name);
    prim.properties
        .extend(attrs.into_iter().map(Property::Attribute));
    let binding = mesh
        .material
        .as_deref()
        .map(|name| material_binding(mesh, &path, &path, name, cx))
        .transpose()?;
    if !mesh.material_subsets.is_empty() {
        check_family(&mesh.material_subsets, mesh.subset_family, sites.faces).map_err(fail)?;
        // `UsdShadeMaterialBindingAPI::SetMaterialBindSubsetsFamilyType`
        // (materialBindingAPI.h:934) authors this attribute on the mesh.
        prim.push_property(
            Attribute::new(
                "subsetFamily:materialBind:familyType",
                "token",
                Value::Token(mesh.subset_family.token().into()),
            )
            .uniform(),
        );
        for subset in &mesh.material_subsets {
            let subset_path = format!("{path}/{}", subset.name);
            let binding = material_binding(mesh, &path, &subset_path, &subset.material, cx)?;
            prim.children
                .push(subset_prim(subset, binding).map_err(fail)?);
        }
    }
    // Relationships follow the attributes.
    if let Some(binding) = binding {
        apply_binding(&mut prim, binding);
    }
    Ok(prim)
}

/// Checks that `subsets` form a valid `family` over `faces` faces, as
/// `UsdGeomSubset::ValidateFamily` does (`pxr/usd/usdGeom/subset.cpp`):
/// indices in range, no index twice (both restricted types), and full
/// coverage for a partition.
fn check_family(
    subsets: &[MaterialSubset<'_>],
    family: FamilyType,
    faces: usize,
) -> Result<(), MeshProblem> {
    let mut covered = vec![false; faces];
    for subset in subsets {
        for &face in subset.faces.iter() {
            let Some(seen) = covered.get_mut(face as usize) else {
                return Err(MeshProblem::SubsetFaceOutOfRange {
                    subset: subset.name.to_string(),
                    face,
                    faces,
                });
            };
            if core::mem::replace(seen, true) {
                return Err(MeshProblem::OverlappingSubsets {
                    subset: subset.name.to_string(),
                    face,
                });
            }
        }
    }
    if family == FamilyType::Partition
        && let Some(face) = covered.iter().position(|c| !c)
    {
        return Err(MeshProblem::IncompletePartition { face });
    }
    Ok(())
}

/// A `GeomSubset` of faces in the `materialBind` family, with its own
/// direct binding (`pxr/usd/usdGeom/subset.h`: `elementType` and
/// `familyName` are uniform tokens, `indices` an `int[]`).
fn subset_prim(subset: &MaterialSubset<'_>, material: String) -> Result<Prim, MeshProblem> {
    let indices = subset
        .faces
        .iter()
        .map(|&face| to_int(face))
        .collect::<Result<Vec<_>, _>>()?;
    let mut prim = Prim::def("GeomSubset", &*subset.name);
    prim.push_property(
        Attribute::new("elementType", "token", Value::Token("face".into())).uniform(),
    );
    prim.push_property(
        Attribute::new("familyName", "token", Value::Token("materialBind".into())).uniform(),
    );
    prim.push_property(Attribute::new("indices", "int[]", Value::IntArray(indices)));
    apply_binding(&mut prim, material);
    Ok(prim)
}

/// Resolves the binding on `binding_path` (the mesh or one of its
/// subsets) by material name, checking that the mesh authors every UV set
/// the material's textures read. Returns the material path.
fn material_binding(
    mesh: &Mesh<'_>,
    mesh_path: &str,
    binding_path: &str,
    name: &str,
    cx: &Context<'_, '_>,
) -> Result<String, ExportError> {
    let material = cx
        .materials
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| ExportError::UnknownMaterial {
            path: binding_path.into(),
            material: name.into(),
        })?;
    let has_uv_set = |uv_set: &str| {
        (uv_set == "st" && mesh.uvs.is_some())
            || mesh.primvars.iter().any(|p| {
                p.name == uv_set
                    && matches!(
                        p.primvar.values,
                        PrimvarData::TexCoord2(_) | PrimvarData::Float2(_)
                    )
            })
    };
    if let Some(texture) = material.textures().find(|t| !has_uv_set(t.uv_set)) {
        return Err(ExportError::InvalidMesh {
            path: mesh_path.into(),
            problem: MeshProblem::MissingTexCoords {
                material: name.into(),
                uv_set: texture.uv_set.into(),
            },
        });
    }
    Ok(format!("{}/{name}", cx.materials_scope))
}

/// A direct, all-purpose binding at the default strength: the
/// `MaterialBindingAPI` applied through `prepend apiSchemas`, and the
/// `material:binding` relationship targeting the material
/// (`pxr/usd/usdShade/materialBindingAPI.h:82`). `usdShadeValidators`'
/// `MaterialBindingApiAppliedValidator` rejects the relationship without
/// the applied schema.
fn apply_binding(prim: &mut Prim, material: String) {
    prim.metadata.push(Metadatum::new(
        "apiSchemas",
        Value::TokenListOp(ListOp::prepend(vec![String::from("MaterialBindingAPI")])),
    ));
    prim.push_property(Relationship::new("material:binding", material));
}

/// Converts topology to USD's `int[]` counts and indices, checking counts,
/// index ranges and the 32-bit signed limit, as `UsdGeomMesh::ValidateTopology`
/// does (`pxr/usd/usdGeom/mesh.h:575`).
fn topology(faces: &Faces<'_>, points: usize) -> Result<(Vec<i32>, Vec<i32>), MeshProblem> {
    let (counts, raw) = match faces {
        Faces::Triangles(indices) => {
            if indices.len() % 3 != 0 {
                return Err(MeshProblem::PartialTriangle { len: indices.len() });
            }
            (vec![3; indices.len() / 3], &**indices)
        }
        Faces::Polygons { counts, indices } => {
            let mut out = Vec::with_capacity(counts.len());
            let mut total = 0_usize;
            for (face, &count) in counts.iter().enumerate() {
                if count < 3 {
                    return Err(MeshProblem::DegenerateFace { face, count });
                }
                total = total.saturating_add(count as usize);
                out.push(to_int(count)?);
            }
            if total != indices.len() {
                return Err(MeshProblem::CornerCountMismatch {
                    expected: total,
                    actual: indices.len(),
                });
            }
            (out, &**indices)
        }
    };
    let mut indices = Vec::with_capacity(raw.len());
    for (corner, &index) in raw.iter().enumerate() {
        if index as usize >= points {
            return Err(MeshProblem::PointIndexOutOfRange {
                corner,
                index,
                points,
            });
        }
        indices.push(to_int(index)?);
    }
    Ok((counts, indices))
}

fn to_int(v: u32) -> Result<i32, MeshProblem> {
    i32::try_from(v).map_err(|_| MeshProblem::TooLarge)
}

fn extent(points: &[[f32; 3]]) -> Result<Option<[[f32; 3]; 2]>, MeshProblem> {
    let mut bounds: Option<[[f32; 3]; 2]> = None;
    for (point, p) in points.iter().enumerate() {
        if !p.iter().all(|c| c.is_finite()) {
            return Err(MeshProblem::NonFinitePoint { point });
        }
        let [lo, hi] = bounds.get_or_insert([*p, *p]);
        for axis in 0..3 {
            lo[axis] = lo[axis].min(p[axis]);
            hi[axis] = hi[axis].max(p[axis]);
        }
    }
    Ok(bounds)
}

/// Checks that a primvar's values (or indices, when indexed) match its
/// interpolation's element count and that indices are in range.
fn check_primvar<V>(
    name: &str,
    value_count: usize,
    primvar: &Primvar<'_, V>,
    sites: Sites,
) -> Result<(), MeshProblem> {
    let expected = sites.count(primvar.interpolation);
    let Some(indices) = &primvar.indices else {
        if value_count != expected {
            return Err(MeshProblem::PrimvarLength {
                name: name.into(),
                expected,
                actual: value_count,
            });
        }
        return Ok(());
    };
    if indices.len() != expected {
        return Err(MeshProblem::PrimvarLength {
            name: format!("{name}:indices"),
            expected,
            actual: indices.len(),
        });
    }
    if let Some(&index) = indices.iter().find(|&&i| i as usize >= value_count) {
        return Err(MeshProblem::PrimvarIndexOutOfRange {
            name: name.into(),
            index,
            values: value_count,
        });
    }
    Ok(())
}

/// Writes an already checked primvar of any type: `name` with its
/// `interpolation` metadata and, when indexed, `name:indices`
/// (`pxr/usd/usdGeom/primvar.h:497`).
fn push_primvar_unchecked(
    attrs: &mut Vec<Attribute>,
    name: &str,
    primvar: &Primvar<'_, PrimvarData<'_>>,
) {
    attrs.push(
        Attribute::new(name, primvar.values.type_name(), primvar.values.to_value()).with_metadata(
            "interpolation",
            Value::Token(primvar.interpolation.token().into()),
        ),
    );
    if let Some(indices) = &primvar.indices {
        #[allow(
            clippy::cast_possible_wrap,
            reason = "instance primvar indices were checked to fit in `int`"
        )]
        let indices = indices.iter().map(|&i| i as i32).collect();
        attrs.push(Attribute::new(
            format!("{name}:indices"),
            "int[]",
            Value::IntArray(indices),
        ));
    }
}

/// Writes `name` with its `interpolation` metadata and, when indexed, the
/// companion `name:indices` `int[]` attribute (`pxr/usd/usdGeom/primvar.h:497`).
/// Indices are copied verbatim: for face-varying data they define the
/// primvar's topology, so they must not be deduplicated by value.
fn push_primvar<V>(
    attrs: &mut Vec<Attribute>,
    name: &str,
    type_name: &str,
    values: Value,
    value_count: usize,
    primvar: &Primvar<'_, V>,
    sites: Sites,
) -> Result<(), MeshProblem> {
    check_primvar(name, value_count, primvar, sites)?;
    attrs.push(Attribute::new(name, type_name, values).with_metadata(
        "interpolation",
        Value::Token(primvar.interpolation.token().into()),
    ));
    if let Some(indices) = &primvar.indices {
        let indices = indices
            .iter()
            .map(|&i| to_int(i))
            .collect::<Result<Vec<_>, _>>()?;
        attrs.push(Attribute::new(
            format!("{name}:indices"),
            "int[]",
            Value::IntArray(indices),
        ));
    }
    Ok(())
}

/// A single `xformOp:transform` op, listed in the `uniform token[]
/// xformOpOrder` (`pxr/usd/usdGeom/xformable.h:136`). Nothing is written without a
/// transform, which composes as identity.
fn push_transform(attrs: &mut Vec<Attribute>, transform: Option<Transform>) {
    let Some(transform) = transform else {
        return;
    };
    attrs.push(Attribute::new(
        "xformOp:transform",
        "matrix4d",
        Value::Matrix4d(transform.usd_rows()),
    ));
    attrs.push(
        Attribute::new(
            "xformOpOrder",
            "token[]",
            Value::TokenArray(vec![String::from("xformOp:transform")]),
        )
        .uniform(),
    );
}

fn push_custom(attrs: &mut Vec<Attribute>, custom: &[CustomAttribute<'_>]) {
    for attribute in custom {
        attrs.push(
            Attribute::new(
                &*attribute.name,
                attribute.value.canonical_type_name(),
                attribute.value.clone(),
            )
            .custom(),
        );
    }
}
