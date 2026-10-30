// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{
    CannotEdit, MeshPublicationError,
    bounds::BoundsError,
    extent::{ExtentError, ExtentRegistrationError},
    geometry::GeometryError,
    point_instancer::PointInstancerError,
    shading::{ConnectionError, ConnectionIssue, MaterialCaptureError, PortError},
    subset::{MeshTopologyError, SubsetAuthoringError, SubsetError},
};
use alloc::{string::ToString, vec::Vec};
use layerstack::{ArrayReadError, InMemoryStore, LayerStore};

/// Each error as `display(store)` writes it.
type Case = (alloc::string::String, &'static str);

#[test]
fn errors_name_paths_and_tokens_with_the_store() {
    let store = InMemoryStore::default();
    let lamp = store.path("/World/Lamp");
    let points = store.property_path("/World/Rock.points");
    let budget = || ArrayReadError::BudgetExceeded { limit: 8 };
    let s: &dyn LayerStore = &store;
    let mut cases: Vec<Case> = Vec::new();
    for (error, text) in [
        (
            CannotEdit::BeneathInstance { instance: lamp },
            "the path is beneath the instance /World/Lamp, whose descendants are instance \
             proxies; edit the instance itself or its prototype's source",
        ),
        (
            CannotEdit::InPrototype,
            "the path is in an instancing prototype; edit the prototype's source instead",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    for (error, text) in [
        (
            MeshTopologyError::MissingAttribute,
            "invalid mesh topology: `points`, `faceVertexCounts` or `faceVertexIndices` is \
             absent or has another value type",
        ),
        (
            MeshTopologyError::InvalidFaceCounts,
            "invalid mesh topology: a `faceVertexCounts` entry is negative or the counts \
             overflow their total",
        ),
        (
            MeshTopologyError::SizeMismatch,
            "invalid mesh topology: the `faceVertexCounts` sum differs from the number of \
             `faceVertexIndices`",
        ),
        (
            MeshTopologyError::InvalidVertexIndex(9),
            "invalid mesh topology: the face vertex index 9 names no point",
        ),
        (
            MeshTopologyError::Decode {
                property: points,
                error: budget(),
            },
            "invalid mesh topology: cannot decode /World/Rock.points: array decode budget \
             exceeded (8 units)",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    for (error, text) in [
        (
            SubsetError::InvalidGeometry,
            "invalid subset geometry: the element type does not apply to the owning geometry",
        ),
        (
            SubsetError::MissingTopology,
            "invalid subset geometry: the geometry's topology is absent or has another value \
             type",
        ),
        (
            SubsetError::InvalidTopology,
            "invalid subset geometry: the geometry's topology has invalid counts, indices or \
             curve tokens",
        ),
        (
            SubsetError::Decode {
                property: points,
                error: budget(),
            },
            "invalid subset geometry: cannot decode /World/Rock.points: array decode budget \
             exceeded (8 units)",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    for (error, text) in [
        (
            SubsetAuthoringError::CannotEdit(CannotEdit::InPrototype),
            "cannot author geometry subset: the path is in an instancing prototype; edit the \
             prototype's source instead",
        ),
        (
            SubsetAuthoringError::InvalidName,
            "cannot author geometry subset: the name is not a single prim identifier",
        ),
        (
            SubsetAuthoringError::Geometry(SubsetError::MissingTopology),
            "cannot author geometry subset: invalid subset geometry: the geometry's topology \
             is absent or has another value type",
        ),
        (
            SubsetAuthoringError::Indices(crate::subset::SubsetProblemKind::InvalidIndex(-1)),
            "cannot author geometry subset: the indices do not fit the geometry: the index -1 \
             is negative or past the geometry's elements",
        ),
        (
            SubsetAuthoringError::IncompatibleFamilyDeclaration,
            "cannot author geometry subset: the family attribute is declared as something \
             other than a scalar `token`",
        ),
        (
            SubsetAuthoringError::NameTaken(lamp),
            "cannot author geometry subset: /World/Lamp already exists and is not a \
             `GeomSubset`; choose another name",
        ),
        (
            SubsetAuthoringError::UnrestrictedMaterialBindFamily,
            "cannot author geometry subset: a `materialBind` family cannot be \
             `unrestricted`: bound subsets must not overlap",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    for (error, text) in [
        (
            MeshPublicationError::Topology(MeshTopologyError::InvalidVertexIndex(9)),
            "invalid mesh publication: invalid mesh topology: the face vertex index 9 names \
             no point",
        ),
        (
            MeshPublicationError::DegenerateFace(2),
            "invalid mesh publication: face 2 has fewer than three corners",
        ),
        (
            MeshPublicationError::NonFinitePoint(3),
            "invalid mesh publication: point 3 has a nonfinite coordinate",
        ),
        (
            MeshPublicationError::PrimvarName("st st".into()),
            "invalid mesh publication: the primvar name `st st` is malformed or repeated",
        ),
        (
            MeshPublicationError::PrimvarType("st".into()),
            "invalid mesh publication: the values of the primvar `st` do not match its \
             declared array type",
        ),
        (
            MeshPublicationError::Interpolation("perPoint".into()),
            "invalid mesh publication: `perPoint` is not a geometry interpolation \
             (`constant`, `uniform`, `vertex`, `varying` or `faceVarying`)",
        ),
        (
            MeshPublicationError::ElementSize,
            "invalid mesh publication: the element size is zero, too large or does not \
             divide the values",
        ),
        (
            MeshPublicationError::Cardinality {
                expected: 4,
                actual: 3,
            },
            "invalid mesh publication: 3 elements were supplied where the interpolation \
             needs 4",
        ),
        (
            MeshPublicationError::Index {
                position: 1,
                index: -2,
            },
            "invalid mesh publication: the primvar index -2 at position 1 is negative or out \
             of bounds",
        ),
        (
            MeshPublicationError::InvalidTarget,
            "invalid mesh publication: the target layer is missing or the edit target does \
             not map the prim",
        ),
        (
            MeshPublicationError::InstanceProxy(lamp),
            "invalid mesh publication: /World/Lamp is an instance proxy, which cannot be \
             edited; publish beneath no instance",
        ),
        (
            MeshPublicationError::NotMesh(lamp),
            "invalid mesh publication: /World/Lamp is authored with another type, or cannot \
             be read as a composed `Mesh`",
        ),
        (
            MeshPublicationError::PropertyConflict("points".into()),
            "invalid mesh publication: the publication site already declares `points` \
             differently",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    for (error, text) in [
        (
            PointInstancerError::MissingAttribute("protoIndices"),
            "invalid point instancer: `protoIndices` is absent or cannot be read with its \
             declared type",
        ),
        (
            PointInstancerError::Decode {
                property: points,
                error: budget(),
            },
            "invalid point instancer: cannot decode /World/Rock.points: array decode budget \
             exceeded (8 units)",
        ),
        (
            PointInstancerError::LengthMismatch("orientations"),
            "invalid point instancer: the length of `orientations` differs from the instance \
             count",
        ),
        (
            PointInstancerError::InvalidPrototypes,
            "invalid point instancer: `prototypes` is empty, or targets a property where a \
             prim is needed",
        ),
        (
            PointInstancerError::InvalidPrototypeIndex {
                instance: 4,
                index: 7,
            },
            "invalid point instancer: instance 4 has the prototype index 7, which is \
             negative or past the `prototypes` targets",
        ),
        (
            PointInstancerError::InvalidTime,
            "invalid point instancer: the times are not finite or of different kinds, or the \
             stage's time-code rate is not finite and positive",
        ),
        (
            PointInstancerError::UnsupportedMotionSource("velocities"),
            "invalid point instancer: `velocities` has no array samples to anchor motion to; \
             scalar splines are unsupported",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    let failed = || ExtentError::Failed {
        prim: lamp,
        message: "no cached geometry".into(),
    };
    for (error, text) in [
        (
            ExtentError::Decode {
                property: points,
                error: budget(),
            },
            "cannot decode the extent input /World/Rock.points: array decode budget exceeded \
             (8 units)",
        ),
        (
            failed(),
            "the extent provider failed for /World/Lamp: no cached geometry",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    let boundable = store.tokens().intern("Boundable");
    for (error, text) in [
        (
            ExtentRegistrationError::NotBoundable(boundable),
            "cannot register an extent provider: `Boundable` is not a registered schema \
             derived from `Boundable`",
        ),
        (
            ExtentRegistrationError::AlreadyRegistered(boundable),
            "cannot register an extent provider: `Boundable` already has one; registration \
             leaves it in place",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    for (error, text) in [
        (
            BoundsError::MissingPrim(lamp),
            "cannot compute bounds: no composed prim is at /World/Lamp",
        ),
        (
            BoundsError::ExtentUnavailable(lamp),
            "cannot compute bounds: the boundable /World/Lamp has no valid `extent` and no \
             registered extent provider",
        ),
        (
            BoundsError::ExtentProvider {
                prim: lamp,
                source: failed(),
            },
            "cannot compute bounds: the extent of /World/Lamp is unavailable: the extent \
             provider failed for /World/Lamp: no cached geometry",
        ),
        (
            BoundsError::InvalidPointInstancer {
                prim: lamp,
                source: PointInstancerError::InvalidPrototypes,
            },
            "cannot compute bounds: the point instancer /World/Lamp is invalid: `prototypes` \
             is empty, or targets a property where a prim is needed",
        ),
        (
            BoundsError::PointInstancerCycle(lamp),
            "cannot compute bounds: the point instancer /World/Lamp reaches an instancer it \
             is inside, or nests more than 64 instancers",
        ),
        (
            BoundsError::SingularTransform(lamp),
            "cannot compute bounds: the world transform of /World/Lamp is singular, so it \
             has no component space",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    let target = store.property_path("/World/Lamp.constraintTargets:aim");
    for (error, text) in [
        (
            GeometryError::MissingAttribute("points"),
            "invalid geometry input: `points` is absent, blocked or of another type",
        ),
        (
            GeometryError::InvalidVertexIndex {
                element: 1,
                index: 8,
                point_count: Some(4),
            },
            "invalid geometry input: tetrahedron 1 has the vertex index 8, outside its 4 \
             points",
        ),
        (
            GeometryError::InvalidVertexIndex {
                element: 1,
                index: -1,
                point_count: None,
            },
            "invalid geometry input: tetrahedron 1 has the negative vertex index -1",
        ),
        (
            GeometryError::RepeatedVertex { element: 2 },
            "invalid geometry input: tetrahedron 2 repeats a vertex index",
        ),
        (
            GeometryError::NonFinite,
            "invalid geometry input: a point or intermediate result is not finite",
        ),
        (
            GeometryError::InvalidOrientation("upward".into()),
            "invalid geometry input: `upward` is not an orientation (`rightHanded` or \
             `leftHanded`)",
        ),
        (
            GeometryError::InvalidCurveToken {
                attribute: "basis",
                token: "hermite".into(),
            },
            "invalid geometry input: `hermite` is not a supported value of `basis`",
        ),
        (
            GeometryError::NegativeCurveCount {
                curve: 0,
                count: -3,
            },
            "invalid geometry input: curve 0 has the negative vertex count -3",
        ),
        (
            GeometryError::SizeOverflow,
            "invalid geometry input: a summed layout size overflows",
        ),
        (
            GeometryError::InvalidConstraintTarget(target),
            "invalid geometry input: the constraint target \
             /World/Lamp.constraintTargets:aim is not a `matrix4d` in a model's \
             `constraintTargets` namespace, or its model has no world transform",
        ),
        (
            GeometryError::EmptyTetMesh,
            "invalid geometry input: an inversion query needs at least four points and one \
             tetrahedron",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    for (error, text) in [
        (
            PortError::MissingPrim(lamp),
            "cannot edit the shading port: no prim is at /World/Lamp",
        ),
        (
            PortError::InvalidName("diffuse color".into()),
            "cannot edit the shading port: `diffuse color` is not a nonempty, \
             colon-separated sequence of identifiers",
        ),
        (
            PortError::InvalidAttribute {
                prim: lamp,
                name: "inputs:color".into(),
            },
            "cannot edit the shading port: /World/Lamp.inputs:color exists but is not a \
             typed shading attribute",
        ),
        (
            PortError::TypeMismatch {
                name: "inputs:color".into(),
            },
            "cannot edit the shading port: `inputs:color` is already declared with another \
             type; the port would change it",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    let destination = store.property_path("/Looks/Rock/Shader.inputs:diffuseColor");
    let source = store.property_path("/Looks/Rock/Texture.outputs:rgb");
    for (issue, reason) in [
        (
            ConnectionIssue::InvalidDestination,
            "the destination is not an existing shading attribute",
        ),
        (
            ConnectionIssue::InvalidSource,
            "the source is not an existing attribute",
        ),
        (
            ConnectionIssue::UnsupportedBehavior,
            "the destination's prim has no supported connectability behavior",
        ),
        (
            ConnectionIssue::OutputNotConnectable,
            "this prim's outputs may not receive connections",
        ),
        (
            ConnectionIssue::InterfaceOnly,
            "an `interfaceOnly` input may only connect to another `interfaceOnly` input",
        ),
        (
            ConnectionIssue::UnknownConnectability,
            "the input's `connectability` is neither `full` nor `interfaceOnly`",
        ),
        (
            ConnectionIssue::PassthroughForbidden,
            "a derived container forbids connecting an output to an input of itself",
        ),
        (
            ConnectionIssue::Encapsulation,
            "the source is outside the container boundary of the destination's owner",
        ),
    ] {
        let error = ConnectionError {
            destination,
            source,
            issue,
        };
        assert_eq!(
            error.display(s).to_string(),
            alloc::format!(
                "cannot connect /Looks/Rock/Shader.inputs:diffuseColor to \
                 /Looks/Rock/Texture.outputs:rgb: {reason}"
            )
        );
    }
    for (error, text) in [
        (
            MaterialCaptureError::SceneDomain,
            "cannot capture the material: the stage was not composed from the supplied \
             store; pass the store it was composed from",
        ),
        (
            MaterialCaptureError::CacheDomain,
            "cannot capture the material: the cache belongs to another store; clear it \
             before using it with this one",
        ),
        (
            MaterialCaptureError::MissingMaterial(lamp),
            "cannot capture the material: /World/Lamp is not a composed, typed `Material`",
        ),
        (
            MaterialCaptureError::InvalidTime,
            "cannot capture the material: the time is NaN or infinite",
        ),
    ] {
        cases.push((error.display(s).to_string(), text));
    }
    for (shown, text) in cases {
        assert_eq!(shown, text);
    }
    // Without a store, `Display` writes the same sentence with ids.
    assert_eq!(
        BoundsError::MissingPrim(lamp).to_string(),
        alloc::format!("cannot compute bounds: no composed prim is at {lamp:?}")
    );
}

/// Asserts that each error's `display(store)` writes its text.
macro_rules! assert_words {
    ($store:expr; $($error:expr => $text:expr),* $(,)?) => {
        $(assert_eq!($error.display($store).to_string(), $text);)*
    };
}

#[test]
fn geometry_scene_and_skeleton_errors_name_paths_with_the_store() {
    use crate::{
        MaterialSubsetError, MembershipCacheError,
        asset_previews::AssetPreviewsError,
        assets::AssetReadError,
        color::ColorError,
        point_motion::PointMotionError,
        primvar::PrimvarError,
        scene_records::SceneError,
        skel::{SkelError, SkinningBakeError},
        subset::SubsetProblemKind,
        validation::ValidationError,
    };
    use layerstack::{EditError, LayerId, TargetPath};

    let mut store = InMemoryStore::default();
    store.set_layer_identifier(LayerId(2), "./robot.usda");
    let robot = store.path("/World/Robot");
    let points = store.property_path("/World/Robot.points");
    let budget = || ArrayReadError::BudgetExceeded { limit: 8 };
    let s: &dyn LayerStore = &store;

    assert_words!(s;
        PointMotionError::MissingPoints => "invalid point motion: `points` cannot be read",
        PointMotionError::Decode { property: points, error: budget() } =>
            "invalid point motion: cannot decode /World/Robot.points: array decode budget \
             exceeded (8 units)",
        PointMotionError::InvalidTime =>
            "invalid point motion: the times are not finite or of different kinds, or the \
             stage's time-code rate is not finite and positive",
        PointMotionError::LengthMismatch =>
            "invalid point motion: a nonempty motion array differs in length from `points`",
        PointMotionError::UnsupportedSource("velocities") =>
            "invalid point motion: `velocities` cannot be anchored as discrete samples",
    );
    let invalid_index = || PrimvarError::InvalidIndex {
        position: 2,
        index: 9,
    };
    assert_words!(s;
        PrimvarError::MissingPrim(robot) => "invalid primvar: no prim is at /World/Robot",
        PrimvarError::InvalidName("st:indices".into()) =>
            "invalid primvar: `st:indices` is empty, malformed or ends in the reserved \
             `:indices` suffix",
        PrimvarError::TypeMismatch("primvars:st".into()) =>
            "invalid primvar: `primvars:st` already exists with an incompatible kind or type",
        PrimvarError::InvalidInterpolation("perPoint".into()) =>
            "invalid primvar: `perPoint` is not a geometry interpolation (`constant`, \
             `uniform`, `vertex`, `varying` or `faceVarying`)",
        PrimvarError::InvalidElementSize(0) => "invalid primvar: the element size 0 is not positive",
        PrimvarError::MissingIndices =>
            "invalid primvar: the indexed primvar has no readable `int[]` indices at the \
             requested time",
        invalid_index() =>
            "invalid primvar: the index 9 at position 2 does not address a complete element of \
             the values",
        PrimvarError::Decode { property: points, error: budget() } =>
            "invalid primvar: cannot decode /World/Robot.points: array decode budget exceeded \
             (8 units)",
        PrimvarError::IdTargetUnsupported => "invalid primvar: id-target primvars cannot be read",
    );
    assert_words!(s;
        MaterialSubsetError::InvalidGeometry(robot) =>
            "cannot read material subsets: /World/Robot is not a mesh or tetrahedral mesh that \
             supports face subsets",
        MaterialSubsetError::Decode { property: points, error: budget() } =>
            "cannot read material subsets: cannot decode /World/Robot.points: array decode \
             budget exceeded (8 units)",
    );
    for (problem, text) in [
        (
            SubsetProblemKind::InvalidGeometry,
            "the geometry cannot support the element type",
        ),
        (
            SubsetProblemKind::ElementTypeMismatch,
            "a family member declares a different element type",
        ),
        (
            SubsetProblemKind::InvalidTopology,
            "the geometry's topology is missing or malformed",
        ),
        (
            SubsetProblemKind::NoIndices,
            "no family member supplies indices at any evaluated time",
        ),
        (
            SubsetProblemKind::OddPairCount,
            "an edge or segment has an incomplete index pair",
        ),
        (
            SubsetProblemKind::DuplicateIndex(3),
            "the restricted family repeats the element index 3",
        ),
        (
            SubsetProblemKind::DuplicatePair([1, 2]),
            "the restricted family repeats the pair (1, 2)",
        ),
        (
            SubsetProblemKind::InvalidIndex(-1),
            "the index -1 is negative or past the geometry's elements",
        ),
        (
            SubsetProblemKind::InvalidPair([1, 5]),
            "the pair (1, 5) is not an edge or segment of the geometry",
        ),
        (
            SubsetProblemKind::IncompletePartition,
            "the partition leaves some of the geometry unassigned",
        ),
    ] {
        assert_eq!(problem.to_string(), text);
    }
    assert_words!(s;
        SceneError::ForeignStore =>
            "cannot extract the scene: the store's interners differ from the observer's or the \
             composed stage's; use the store the stage was composed from",
        SceneError::InvalidTime => "cannot extract the scene: the time is not finite",
        SceneError::MissingSchemas =>
            "cannot extract the scene: the stage has no geometry and shading schema registry; \
             compose it with one",
        SceneError::UnsupportedGeometry { prim: robot, type_name: "NurbsPatch".into() } =>
            "cannot extract the scene: /World/Robot is a `NurbsPatch`, which is outside the \
             supported meshes and point instancers",
        SceneError::MissingAttribute(points) =>
            "cannot extract the scene: /World/Robot.points is missing or has an incompatible \
             type",
        SceneError::MissingProperty { prim: robot, name: "normals".into() } =>
            "cannot extract the scene: the schema registry does not define `normals`, which \
             /World/Robot needs",
        SceneError::Decode { property: points, source: budget() } =>
            "cannot extract the scene: cannot decode /World/Robot.points: array decode budget \
             exceeded (8 units)",
        SceneError::Mesh { prim: robot, source: MeshPublicationError::DegenerateFace(2) } =>
            "cannot extract the scene: the mesh /World/Robot is invalid: face 2 has fewer than \
             three corners",
        SceneError::Hole { prim: robot, index: 7 } =>
            "cannot extract the scene: the hole index 7 of /World/Robot names no face",
        SceneError::Primvar { prim: robot, name: "st".into(), source: invalid_index() } =>
            "cannot extract the scene: the primvar `st` of /World/Robot is invalid: the index 9 \
             at position 2 does not address a complete element of the values",
        SceneError::Materials {
            prim: robot,
            source: MaterialSubsetError::InvalidGeometry(robot),
        } =>
            "cannot extract the scene: cannot read the material subsets of /World/Robot: \
             /World/Robot is not a mesh or tetrahedral mesh that supports face subsets",
        SceneError::PointInstancer {
            prim: robot,
            source: PointInstancerError::InvalidPrototypes,
        } =>
            "cannot extract the scene: the point instancer /World/Robot is invalid: \
             `prototypes` is empty, or targets a property where a prim is needed",
        SceneError::Transform(robot) =>
            "cannot extract the scene: cannot compute the transform of /World/Robot",
        SceneError::MaterialNetwork {
            prim: robot,
            source: MaterialCaptureError::MissingMaterial(robot),
        } =>
            "cannot extract the scene: cannot capture the material /World/Robot: /World/Robot \
             is not a composed, typed `Material`",
    );
    let arm = store.path("/World/Robot/Arm");
    assert_words!(s;
        SkelError::Decode { property: points, error: budget() } =>
            "invalid skeleton input: cannot decode /World/Robot.points: array decode budget \
             exceeded (8 units)",
        SkelError::InvalidTopology { joint: 3 } =>
            "invalid skeleton input: joint 3 is duplicated, malformed or ordered before its \
             parent",
        SkelError::InvalidInput { prim: robot, property: "joints" } =>
            "invalid skeleton input: /World/Robot.joints is missing, blocked or of an \
             incompatible type",
        SkelError::ArrayLength { prim: robot, property: "bindTransforms", expected: 3, actual: 2 } =>
            "invalid skeleton input: /World/Robot.bindTransforms has 2 values where 3 are \
             needed",
        SkelError::InvalidTarget {
            prim: robot,
            relationship: "skel:skeleton",
            target: TargetPath::Prim(arm),
        } =>
            "invalid skeleton input: /World/Robot.skel:skeleton targets /World/Robot/Arm, which \
             is missing or of an incompatible type",
        SkelError::Primvar { prim: robot, source: PrimvarError::MissingIndices } =>
            "invalid skeleton input: the joint influences of /World/Robot cannot be flattened: \
             the indexed primvar has no readable `int[]` indices at the requested time",
        SkelError::InvalidDeformation { element: Some(4), reason: "nonfinite weight" } =>
            "invalid skeleton input: element 4 of the deformation input is invalid: nonfinite \
             weight",
        SkelError::InvalidDeformation { element: None, reason: "length mismatch" } =>
            "invalid skeleton input: the deformation input is invalid: length mismatch",
        SkelError::UnsupportedSkinningMethod { prim: robot } =>
            "invalid skeleton input: the skinning method of /World/Robot is neither \
             `classicLinear` nor `dualQuaternion`",
        SkelError::SingularNormalTransform { joint: Some(1) } =>
            "invalid skeleton input: the normal transform of joint 1 is singular or not finite",
        SkelError::SingularNormalTransform { joint: None } =>
            "invalid skeleton input: the normal transform of the geometry bind transform is \
             singular or not finite",
        SkelError::SingularBind { joint: 2 } =>
            "invalid skeleton input: the bind transform of joint 2 cannot be inverted",
    );
    assert_words!(s;
        SkinningBakeError::NotSkelRoot(robot) =>
            "cannot bake skinning: the binding root /World/Robot is not a `SkelRoot` on the stage",
        SkinningBakeError::Instanced(robot) =>
            "cannot bake skinning: /World/Robot is a native instance or an instance proxy, \
             which is not baked",
        SkinningBakeError::MissingLayer(LayerId(2)) =>
            "cannot bake skinning: the binding's @./robot.usda@ is not in the store",
        SkinningBakeError::Edit(EditError::StaleGeneration { layer: LayerId(2), expected: 1, found: 2 }) =>
            "cannot bake skinning: the baked values did not apply: @./robot.usda@ changed since \
             the edit was prepared (generation 2, expected 1)",
    );
    let texture = store.property_path("/World/Robot.inputs:file");
    assert_words!(s;
        AssetReadError::MissingAttribute(texture) =>
            "cannot read the asset: no attribute is at /World/Robot.inputs:file",
        AssetReadError::InvalidValue(texture) =>
            "cannot read the asset: the value of /World/Robot.inputs:file is not an asset",
        AssetPreviewsError::NoSuchLayer(LayerId(5)) =>
            "cannot read the default previews: the store holds no layer 5",
        AssetPreviewsError::NoDefaultPrim =>
            "cannot read the default previews: the layer authors no `defaultPrim`",
        AssetPreviewsError::InvalidDefaultPrim("a b".into()) =>
            "cannot read the default previews: the layer's `defaultPrim` `a b` is not a prim \
             name or an absolute prim path",
        ValidationError::InvalidTimes =>
            "invalid validation request: the time list is empty or has a time that is not finite",
        ValidationError::MissingScopePrim(robot) =>
            "invalid validation request: no composed prim is at /World/Robot, the root of the \
             requested scope",
        MembershipCacheError::MissingPrim(robot) =>
            "cannot capture the collection: no prim is at /World/Robot",
        MembershipCacheError::InvalidName("".into()) =>
            "cannot capture the collection: `` is empty, malformed or a reserved collection name",
    );
    let color_space = store.property_path("/World/Robot.colorSpace:name");
    assert_words!(s;
        ColorError::MissingPrim(robot) => "invalid color-space input: no prim is at /World/Robot",
        ColorError::MissingAttribute(color_space) =>
            "invalid color-space input: /World/Robot.colorSpace:name does not exist or is not \
             an attribute",
        ColorError::UnknownColorSpace { name: "studio".into(), prim: robot } =>
            "invalid color-space input: `studio`, looked up from /World/Robot, is neither a \
             built-in color space nor one defined in scope",
        ColorError::InvalidAttribute {
            prim: robot,
            instance: "studio".into(),
            attribute: "gamma",
        } =>
            "invalid color-space input: the `gamma` of the color space definition `studio` on \
             /World/Robot is missing, of another type or not finite",
        ColorError::InvalidChromaticities =>
            "invalid color-space input: the chromaticities do not define a finite, invertible \
             RGB-to-XYZ matrix",
        ColorError::InvalidTransferFunction =>
            "invalid color-space input: the gamma and linear bias do not define a finite, \
             continuous transfer function",
        ColorError::InvalidMatrix =>
            "invalid color-space input: a matrix is not invertible or has entries that are not \
             finite",
        ColorError::InvalidColor => "invalid color-space input: an input or output color is not finite",
    );
}
