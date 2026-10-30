// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Diagnostics in words, as `layerstack` writes its own: each
//! diagnostic's `display(store)` returns an `impl Display` that writes its
//! ids as text with the store they belong to. An error type's `Display`,
//! having no store, writes the same sentence with the ids themselves.

use alloc::{format, string::String};
use core::fmt;
use layerstack::{EditError, LayerId, LayerStore, PathId, PropertyPath, TokenId};

/// A `Display` that writes with a closure.
#[cfg(feature = "usd")]
pub(crate) struct Shown<F>(pub(crate) F);

#[cfg(feature = "usd")]
impl<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result> fmt::Display for Shown<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(f)
    }
}

/// What writes a diagnostic's ids: the store they belong to, or, without
/// one, the ids themselves.
#[derive(Clone, Copy)]
pub(crate) struct Names<'a>(pub(crate) Option<&'a dyn LayerStore>);

#[allow(dead_code, reason = "each method is used by some feature set")]
impl Names<'_> {
    /// A prim path (`/World/Rock`).
    pub(crate) fn path(self, id: PathId) -> String {
        match self.0 {
            Some(store) => store.paths().display(id, store.tokens()),
            None => format!("{id:?}"),
        }
    }

    /// A name (`primvars:height`).
    pub(crate) fn token(self, id: TokenId) -> String {
        match self.0 {
            Some(store) => store.tokens().resolve(id).into(),
            None => format!("{id:?}"),
        }
    }

    /// A property path (`/World/Rock.primvars:height`).
    pub(crate) fn property(self, path: PropertyPath) -> String {
        let mut out = self.path(path.prim_path());
        out.push('.');
        out.push_str(&self.token(path.property()));
        out
    }

    /// A layer: its identifier as OpenUSD writes a layer (`@./set.usda@`)
    /// when the store names it, else its id (`layer 3`).
    pub(crate) fn layer(self, id: LayerId) -> String {
        match self.0.and_then(|store| store.layer_identifier(id)) {
            Some(identifier) => format!("@{identifier}@"),
            None => format!("layer {}", id.0),
        }
    }

    /// An edit error, through [`EditError::display`] with a store.
    pub(crate) fn edit(self, f: &mut fmt::Formatter<'_>, error: &EditError) -> fmt::Result {
        match self.0 {
            Some(store) => write!(f, "{}", error.display(store)),
            None => write!(f, "{error}"),
        }
    }
}

/// A diagnostic that writes itself in words with [`Names`].
pub(crate) trait Describe {
    /// Writes the diagnostic, its ids written by `names`.
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result;
}

/// A diagnostic and the names it is written with.
pub(crate) struct Described<'a, T: ?Sized>(pub(crate) &'a T, pub(crate) Names<'a>);

impl<T: Describe + ?Sized> fmt::Display for Described<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.describe(f, self.1)
    }
}

/// `item` written with the store its ids belong to.
pub(crate) fn with_store<'a, T: Describe + ?Sized>(
    item: &'a T,
    store: &'a dyn LayerStore,
) -> impl fmt::Display + 'a {
    Described(item, Names(Some(store)))
}

#[cfg(all(test, feature = "usd-geom", feature = "usd-shade"))]
mod tests {
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
                "cannot author geometry subset: the indices do not fit the geometry: \
                 InvalidIndex(-1)",
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
                "cannot compute bounds: /World/Lamp: invalid point instancer: `prototypes` is \
                 empty, or targets a property where a prim is needed",
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
}
