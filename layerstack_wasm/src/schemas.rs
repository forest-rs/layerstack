// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reads of OpenUSD's shading, geometry subset and coordinate-system
//! schemas (`layerstack_schemas`), as records for the wrapper's
//! `Material`, `Shader`, `ShaderPort`, `GeomSubset` and binding objects.
//!
//! Every read composes nothing new: it asks the schema crate's views of
//! the stage as it is now, and names prims and properties by their stage
//! paths as text.
//!
//! Spec: AOUSD Core §12 (value and relationship resolution), §12.4 (list
//! ops for connections and binding targets). OpenUSD: `UsdShade` and
//! `UsdGeomSubset`.

use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};

use layerstack::{InMemoryStore, PathId, PropertyPath, TargetPath, Time, TypedArray};
use layerstack_schemas::{
    BindingCache, BindingKind, BindingOptions, BindingStrength, BoundMaterial, CoordSysCache,
    MaterialPurpose, PrimView, Scene,
    shading::{ConnectionSourceInfo, MaterialTerminal, PortKind, ValueSourceKind},
    subset::{MeshTopologyError, SubsetError, SubsetProblem, SubsetProblemKind, SubsetValidation},
    usd_geom::{GeomSubsetElementType, Imageable, Mesh},
    usd_shade::{Material, Shader},
};

use crate::{array::ArrayHandle, error::Error, stage::Stage, wire::Wire};

/// A prim path as text.
pub(crate) fn path_text(store: &InMemoryStore, path: PathId) -> Wire {
    Wire::Text(store.paths().display(path, store.tokens()))
}

/// A property path as text.
pub(crate) fn property_text(store: &InMemoryStore, path: PropertyPath) -> Wire {
    Wire::Text(path.display(store.paths(), store.tokens()))
}

/// A target path as text.
pub(crate) fn target_text(store: &InMemoryStore, path: TargetPath) -> Wire {
    Wire::Text(path.display(store.paths(), store.tokens()))
}

/// A binding strength as the wrapper spells it, the `bindMaterialAs` token.
pub(crate) fn strength_name(strength: BindingStrength) -> &'static str {
    match strength {
        BindingStrength::WeakerThanDescendants => "weakerThanDescendants",
        BindingStrength::StrongerThanDescendants => "strongerThanDescendants",
    }
}

/// `[material, target, relationship, kind, collection, strength, purpose,
/// withoutBindingApi]` for a prim's bound material, or `null` when nothing
/// binds it: the material's path (`null` when no `Material` is at the
/// winning binding's target), the target, the deciding relationship,
/// `direct` or `collection` with the collection's path, its strength, the
/// purpose it was found for (`""` all-purpose), and whether the prim
/// authoring it lacks `MaterialBindingAPI` (a legacy binding).
fn binding_record(bound: &BoundMaterial, store: &InMemoryStore) -> Wire {
    let Some(binding) = &bound.binding else {
        return Wire::Null;
    };
    let (kind, collection) = match &binding.kind {
        BindingKind::Direct => ("direct", Wire::Null),
        BindingKind::Collection { collection } => ("collection", property_text(store, *collection)),
    };
    Wire::List(vec![
        bound
            .material
            .map_or(Wire::Null, |material| path_text(store, material)),
        path_text(store, binding.target),
        property_text(store, binding.relationship),
        Wire::text(kind),
        collection,
        Wire::text(strength_name(binding.strength)),
        Wire::text(binding.purpose.as_str()),
        Wire::Bool(binding.without_binding_api),
    ])
}

/// The name of a subset problem's kind, and the index or pair it names.
fn problem_kind(kind: &SubsetProblemKind) -> (&'static str, Wire) {
    let pair =
        |[a, b]: [i32; 2]| Wire::List(vec![Wire::Number(f64::from(a)), Wire::Number(f64::from(b))]);
    match kind {
        SubsetProblemKind::InvalidGeometry => ("invalidGeometry", Wire::Null),
        SubsetProblemKind::ElementTypeMismatch => ("elementTypeMismatch", Wire::Null),
        SubsetProblemKind::InvalidTopology => ("invalidTopology", Wire::Null),
        SubsetProblemKind::NoIndices => ("noIndices", Wire::Null),
        SubsetProblemKind::OddPairCount => ("oddPairCount", Wire::Null),
        SubsetProblemKind::DuplicateIndex(index) => {
            ("duplicateIndex", Wire::Number(f64::from(*index)))
        }
        SubsetProblemKind::DuplicatePair(at) => ("duplicatePair", pair(*at)),
        SubsetProblemKind::InvalidIndex(index) => ("invalidIndex", Wire::Number(f64::from(*index))),
        SubsetProblemKind::InvalidPair(at) => ("invalidPair", pair(*at)),
        SubsetProblemKind::IncompletePartition => ("incompletePartition", Wire::Null),
        // A problem this build does not name.
        _ => ("other", Wire::Null),
    }
}

/// `[prim, time, kind, at, message]` per problem: the family member (or
/// the geometry for a family-wide problem), the stage time (`null` for the
/// default time), the problem's kind, the index or `[a, b]` pair it names
/// (else `null`), and the problem in words.
fn problems_record(validation: &SubsetValidation, store: &InMemoryStore) -> Wire {
    Wire::List(
        validation
            .problems
            .iter()
            .map(|problem: &SubsetProblem| {
                let (kind, at) = problem_kind(&problem.kind);
                Wire::List(vec![
                    path_text(store, problem.path),
                    time_record(problem.time),
                    Wire::text(kind),
                    at,
                    Wire::Text(problem.kind.to_string()),
                ])
            })
            .collect(),
    )
}

/// A time as a number, `null` for the default time.
fn time_record(time: Time) -> Wire {
    match time {
        Time::Default => Wire::Null,
        Time::At { code, .. } => Wire::Number(code),
    }
}

/// An element type as `UsdGeomSubset` spells it.
pub(crate) fn element_type(name: &str) -> Result<GeomSubsetElementType, Error> {
    match GeomSubsetElementType::from_token(name) {
        GeomSubsetElementType::Other(_) => Err(Error::new(
            "argument.invalid",
            alloc::format!(
                "`{name}` is not a subset element type: face, point, edge, segment or tetrahedron"
            ),
        )),
        element => Ok(element),
    }
}

/// The error a subset family read fails with.
fn subset_error(error: &SubsetError, store: &InMemoryStore, path: &str) -> Error {
    let code = match error {
        SubsetError::Decode { .. } => "value.decode",
        _ => "subset.geometry",
    };
    Error::new(code, error.display(store).to_string()).at_path(path)
}

/// A port's record: `[path, kind, typeName, sdrMetadata, connections,
/// invalidConnections]`, its `input` or `output` kind, its declared type
/// name (`null` when nothing declares one), its `sdrMetadata` entries as
/// `[key, value]` pairs, its direct valid sources in composed order, and
/// its targets that name no shading attribute.
fn port_record(port: &layerstack_schemas::shading::Port<'_>, store: &InMemoryStore) -> Wire {
    let connected = port.connected_sources();
    Wire::List(vec![
        property_text(store, port.path()),
        Wire::text(match port.kind() {
            PortKind::Input => "input",
            PortKind::Output => "output",
        }),
        port.property_type()
            .map_or(Wire::Null, |ty| Wire::text(ty.type_name())),
        metadata_pairs(port.sdr_metadata_strings()),
        Wire::List(
            connected
                .sources
                .into_iter()
                .map(|source| property_text(store, source))
                .collect(),
        ),
        Wire::List(
            connected
                .invalid
                .into_iter()
                .map(|target| target_text(store, target))
                .collect(),
        ),
    ])
}

/// `sdrMetadata` entries as `[key, value]` pairs.
fn metadata_pairs(entries: Vec<(alloc::sync::Arc<str>, String)>) -> Wire {
    Wire::List(
        entries
            .into_iter()
            .map(|(key, value)| Wire::List(vec![Wire::text(&*key), Wire::Text(value)]))
            .collect(),
    )
}

impl Stage {
    /// The composed scene the schema views read.
    fn scene(&self) -> Scene<'_> {
        Scene::new(self.document().stage().stage(), self.document().store())
    }

    /// The prim at `path`, which must exist.
    fn schema_prim(&self, path: &str) -> Result<PathId, Error> {
        Ok(self.existing_prim(path)?.path())
    }

    /// The bound material of each prim of `paths` for `purpose` (`""` for
    /// all purposes), in order, as binding records
    /// ([`binding_record`]): one [`BindingCache`] shares the ancestors'
    /// bindings and the collections' membership across the prims. Legacy
    /// bindings (on prims without `MaterialBindingAPI`) count, as OpenUSD
    /// counts them by default.
    ///
    /// OpenUSD: `UsdShadeMaterialBindingAPI::ComputeBoundMaterials`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` naming the first path no prim
    /// is at.
    pub(crate) fn bound_materials(&self, paths: &[String], purpose: &str) -> Result<Wire, Error> {
        let scene = self.scene();
        let store = self.document().store();
        let mut cache = BindingCache::new(
            MaterialPurpose::from_token(purpose),
            BindingOptions::default(),
        );
        let mut records = Vec::with_capacity(paths.len());
        for path in paths {
            let prim = self.schema_prim(path)?;
            records.push(binding_record(
                &cache.compute_bound_material(&scene, prim),
                store,
            ));
        }
        Ok(Wire::List(records))
    }

    /// The mesh's `materialBind` face subsets at `time` for `purpose`:
    /// `[familyType, fallback, subsets, problems]`, the family's type
    /// (`unrestricted` when unauthored), the binding of faces in no subset
    /// (the mesh's own, a binding record or `null`), each subset as
    /// `[path, indices, binding]` (its face indices an `int[]` array
    /// handle, unchecked against the mesh), and the family's validation
    /// problems ([`problems_record`]), which never hide a subset.
    ///
    /// OpenUSD: `UsdShadeMaterialBindingAPI::GetMaterialBindSubsets`,
    /// `UsdGeomSubset::ValidateFamily`, `ComputeBoundMaterial`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`; `subset.geometry` when the prim is
    /// not a mesh or tetrahedral mesh; `value.decode` for indices or
    /// topology that fail to decode.
    pub(crate) fn material_subsets(
        &self,
        path: &str,
        purpose: &str,
        time: Option<f64>,
    ) -> Result<Wire, Error> {
        let scene = self.scene();
        let store = self.document().store();
        let prim = self.schema_prim(path)?;
        let mut cache = BindingCache::new(
            MaterialPurpose::from_token(purpose),
            BindingOptions::default(),
        );
        let subsets = cache
            .material_binding_subsets(&scene, prim, crate::stage::time_of(time))
            .map_err(|error| {
                let code = match error {
                    layerstack_schemas::MaterialSubsetError::Decode { .. } => "value.decode",
                    _ => "subset.geometry",
                };
                Error::new(code, error.display(store).to_string()).at_path(path)
            })?;
        let rows = subsets
            .subsets
            .iter()
            .map(|subset| {
                let indices = ArrayHandle::new(
                    Some("int[]"),
                    &TypedArray::Int(alloc::sync::Arc::clone(&subset.indices)),
                )
                .map_or(Wire::Null, Wire::Array);
                Wire::List(vec![
                    path_text(store, subset.path),
                    indices,
                    binding_record(&subset.material, store),
                ])
            })
            .collect();
        Ok(Wire::List(vec![
            Wire::Text(subsets.family_type.clone()),
            binding_record(&subsets.fallback, store),
            Wire::List(rows),
            problems_record(&subsets.validation, store),
        ]))
    }

    /// The `Material` at `path`: `[baseMaterial]`, the material it derives
    /// from through a specialize or inherit arc to another material
    /// (`null` without one); `null` when no material is there.
    ///
    /// OpenUSD: `UsdShadeMaterial::GetBaseMaterialPath`.
    ///
    /// # Errors
    ///
    /// `path.invalid`.
    pub(crate) fn material(&self, path: &str) -> Result<Wire, Error> {
        crate::stage::check_prim_path(path)?;
        let scene = self.scene();
        let store = self.document().store();
        let Some(material) = scene
            .prim_path(path)
            .and_then(|at| Material::get(&scene, at))
        else {
            return Ok(Wire::Null);
        };
        Ok(Wire::List(vec![
            material
                .base_material_path()
                .map_or(Wire::Null, |base| path_text(store, base)),
        ]))
    }

    /// The source of the material's `terminal` output (`surface`,
    /// `displacement` or `volume`) for the render `contexts`, tried in
    /// order and then the universal one: `[shader, context, sources]`, the
    /// first source's prim when it is a `Shader` (else `null`), the context
    /// that supplied the sources (`null` when none did, `""` universal),
    /// and every terminal source as `[output, chain]`, the shader output
    /// and the outputs and inputs the connection passed through, requested
    /// output first.
    ///
    /// OpenUSD: `UsdShadeMaterial::ComputeSurfaceSource` (and its
    /// displacement and volume siblings).
    ///
    /// # Errors
    ///
    /// `path.invalid`; `prim.not_found` when no `Material` is at `path`;
    /// `argument.invalid` for an unknown terminal.
    pub(crate) fn material_terminal(
        &self,
        path: &str,
        terminal: &str,
        contexts: &[String],
    ) -> Result<Wire, Error> {
        crate::stage::check_prim_path(path)?;
        let scene = self.scene();
        let store = self.document().store();
        let terminal = match terminal {
            "surface" => MaterialTerminal::Surface,
            "displacement" => MaterialTerminal::Displacement,
            "volume" => MaterialTerminal::Volume,
            other => {
                return Err(Error::new(
                    "argument.invalid",
                    alloc::format!(
                        "`{other}` is not a material terminal: surface, displacement or volume"
                    ),
                ));
            }
        };
        let material = scene
            .prim_path(path)
            .and_then(|at| Material::get(&scene, at))
            .ok_or_else(|| {
                Error::new("prim.not_found", "no Material is at this path").at_path(path)
            })?;
        let contexts: Vec<&str> = contexts.iter().map(String::as_str).collect();
        let source = material.compute_terminal_source(terminal, &contexts);
        let sources = source
            .trace
            .sources
            .iter()
            .map(|source| {
                Wire::List(vec![
                    property_text(store, source.output),
                    Wire::List(
                        source
                            .chain
                            .iter()
                            .map(|link| property_text(store, *link))
                            .collect(),
                    ),
                ])
            })
            .collect();
        Ok(Wire::List(vec![
            source
                .shader
                .map_or(Wire::Null, |shader| path_text(store, shader)),
            Wire::text_or_null(source.context.clone()),
            Wire::List(sources),
        ]))
    }

    /// The `Shader` at `path`: `[id, implementationSource, sdrMetadata]`,
    /// its `info:id` (`null` unauthored), its `info:implementationSource`
    /// (`id` unauthored) and its `sdrMetadata` as `[key, value]` pairs;
    /// `null` when no shader is there.
    ///
    /// OpenUSD: `UsdShadeShader::GetShaderId`, `GetImplementationSource`,
    /// `GetSdrMetadata`.
    ///
    /// # Errors
    ///
    /// `path.invalid`.
    pub(crate) fn shader(&self, path: &str) -> Result<Wire, Error> {
        crate::stage::check_prim_path(path)?;
        let scene = self.scene();
        let Some(shader) = scene.prim_path(path).and_then(|at| Shader::get(&scene, at)) else {
            return Ok(Wire::Null);
        };
        let node = shader.node_def_api();
        Ok(Wire::List(vec![
            Wire::text_or_null(node.id()),
            Wire::text(
                node.implementation_source()
                    .as_ref()
                    .map_or("id", |source| source.as_str()),
            ),
            metadata_pairs(shader.sdr_metadata_strings()),
        ]))
    }

    /// The prim's shading ports of `kind` (`input` or `output`), in
    /// property order, schema-defined ones included, as port records
    /// ([`port_record`]).
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`; `argument.invalid` for another
    /// kind.
    pub(crate) fn ports(&self, path: &str, kind: &str) -> Result<Wire, Error> {
        let prim = self.schema_prim(path)?;
        let kind = match kind {
            "input" => PortKind::Input,
            "output" => PortKind::Output,
            other => {
                return Err(Error::new(
                    "argument.invalid",
                    alloc::format!("`{other}` is not a port kind: input or output"),
                ));
            }
        };
        let store = self.document().store();
        Ok(Wire::List(
            PrimView::new(self.scene(), prim)
                .ports(kind)
                .iter()
                .map(|port| port_record(port, store))
                .collect(),
        ))
    }

    /// The input or output attribute at the property path `path`
    /// (`/Looks/Red/Surface.inputs:diffuseColor`) as a port record, or
    /// `null` when no shading attribute is there.
    ///
    /// # Errors
    ///
    /// `path.invalid`.
    pub(crate) fn port(&self, path: &str) -> Result<Wire, Error> {
        crate::stage::check_property_path(path)?;
        let store = self.document().store();
        let Ok(property) = store.try_property_path(path) else {
            return Ok(Wire::Null);
        };
        Ok(
            layerstack_schemas::shading::Port::get(&self.scene(), property)
                .map_or(Wire::Null, |port| port_record(&port, store)),
        )
    }

    /// The connection source the path text `path` names, checked against
    /// the stage: `[prim, name, kind, typeName, source]`, the source prim
    /// (`null` when the stage has none there), the attribute's base name
    /// (empty when `path` is not a property path), its `input` or `output`
    /// kind (`null` for a name in neither namespace), the attribute's type
    /// name (`null` when no attribute is there), and the source's property
    /// path to connect to (`null` unless the info is valid). Text that is
    /// not a property path, a relational attribute path included, reads as
    /// an empty info rather than failing.
    ///
    /// OpenUSD: `UsdShadeConnectionSourceInfo(stage, sourcePath)` and
    /// `IsValid`; see [`ConnectionSourceInfo`] for each case and the one
    /// difference.
    pub(crate) fn connection_source_info(&self, path: &str) -> Wire {
        let scene = self.scene();
        let store = self.document().store();
        let info = ConnectionSourceInfo::new(&scene, path);
        Wire::List(vec![
            info.prim()
                .map_or(Wire::Null, |prim| path_text(store, prim)),
            Wire::text(info.name()),
            info.kind().map_or(Wire::Null, |kind| {
                Wire::text(match kind {
                    PortKind::Input => "input",
                    PortKind::Output => "output",
                })
            }),
            info.type_name()
                .map_or(Wire::Null, |ty| Wire::text(ty.type_name())),
            info.source().map_or(Wire::Null, |source| {
                Wire::Text(alloc::format!(
                    "{}.{}",
                    store.paths().display(source.prim(), store.tokens()),
                    source.name()
                ))
            }),
        ])
    }

    /// Where the port at `path` gets its value, following connections
    /// through node graphs: `[sources, issues]`, each source
    /// `[attribute, kind, chain]` in depth-first order (`shaderOutput` for
    /// an output to evaluate, `authoredValue` for an input or output whose
    /// value to read), and each recoverable problem on a branch as
    /// `[kind, at]` (`invalidTarget`, `cycle`, `nonContainerInput`). An
    /// unconnected input with a value is its own `authoredValue` source.
    ///
    /// OpenUSD: `UsdShadeUtils::GetValueProducingAttributes`.
    ///
    /// # Errors
    ///
    /// `path.invalid`; `prim.not_found` when no shading attribute is at
    /// `path`.
    pub(crate) fn value_sources(&self, path: &str) -> Result<Wire, Error> {
        crate::stage::check_property_path(path)?;
        let scene = self.scene();
        let store = self.document().store();
        let port = store
            .try_property_path(path)
            .ok()
            .and_then(|property| layerstack_schemas::shading::Port::get(&scene, property))
            .ok_or_else(|| {
                Error::new(
                    "prim.not_found",
                    "no shading input or output is at this path",
                )
                .at_path(path)
            })?;
        let found = port.value_sources();
        let sources = found
            .sources
            .iter()
            .map(|source| {
                Wire::List(vec![
                    property_text(store, source.attribute),
                    Wire::text(match source.kind {
                        ValueSourceKind::ShaderOutput => "shaderOutput",
                        ValueSourceKind::AuthoredValue => "authoredValue",
                    }),
                    Wire::List(
                        source
                            .chain
                            .iter()
                            .map(|link| property_text(store, *link))
                            .collect(),
                    ),
                ])
            })
            .collect();
        let issues = found
            .issues
            .iter()
            .map(|issue| match issue {
                layerstack_schemas::shading::ShadingIssue::InvalidTarget { target, .. } => {
                    Wire::List(vec![
                        Wire::text("invalidTarget"),
                        target_text(store, *target),
                    ])
                }
                layerstack_schemas::shading::ShadingIssue::Cycle(at) => {
                    Wire::List(vec![Wire::text("cycle"), property_text(store, *at)])
                }
                layerstack_schemas::shading::ShadingIssue::NonContainerInput(at) => {
                    Wire::List(vec![
                        Wire::text("nonContainerInput"),
                        property_text(store, *at),
                    ])
                }
                // An issue this build does not name.
                _ => Wire::List(vec![Wire::text("other"), Wire::Null]),
            })
            .collect();
        Ok(Wire::List(vec![Wire::List(sources), Wire::List(issues)]))
    }

    /// The prim's geometry subsets at `time`: its active, defined
    /// `GeomSubset` children in namespace order, of `element` type and
    /// `family` when given, each `[path, elementType, familyName, indices]`
    /// (`indices` an `int[]` array handle, `null` when none resolves).
    ///
    /// OpenUSD: `UsdGeomSubset::GetGeomSubsets`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`; `argument.invalid` for an unknown
    /// element type; `value.decode` for indices that fail to decode.
    pub(crate) fn geom_subsets(
        &self,
        path: &str,
        element: Option<&str>,
        family: Option<&str>,
        time: Option<f64>,
    ) -> Result<Wire, Error> {
        let prim = self.schema_prim(path)?;
        let element = element.map(element_type).transpose()?;
        let scene = self.scene();
        let store = self.document().store();
        let Some(geom) = Imageable::get(&scene, prim) else {
            return Ok(Wire::List(Vec::new()));
        };
        let time = crate::stage::time_of(time);
        let mut rows = Vec::new();
        for subset in geom.geom_subsets(element.as_ref(), family) {
            let indices = subset
                .try_indices(time)
                .map_err(|error| Error::new("value.decode", error.to_string()).at_path(path))?;
            rows.push(Wire::List(vec![
                path_text(store, subset.path()),
                subset
                    .element_type()
                    .map_or(Wire::Null, |element| Wire::text(element.as_str())),
                Wire::text_or_null(subset.family_name()),
                indices
                    .and_then(|indices| ArrayHandle::new(Some("int[]"), &TypedArray::Int(indices)))
                    .map_or(Wire::Null, Wire::Array),
            ]));
        }
        Ok(Wire::List(rows))
    }

    /// The mesh's topology at `time` (`Mesh::topology`), `null` when the
    /// prim is not a `Mesh`: `[faceVertexCounts, faceVertexIndices,
    /// holeIndices, orientation, subdivisionScheme, pointCount, problem]`,
    /// the arrays as JS-owned `Int32Array`s and `problem` `null` for a
    /// valid topology, else `[code, reason]` ([`topology_problem_code`]).
    ///
    /// OpenUSD: the `UsdGeomMesh` attribute reads and
    /// `UsdGeomMesh::ValidateTopology`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`.
    pub(crate) fn mesh_topology(&self, path: &str, time: Option<f64>) -> Result<Wire, Error> {
        let prim = self.schema_prim(path)?;
        let scene = self.scene();
        let Some(mesh) = Mesh::get(&scene, prim) else {
            return Ok(Wire::Null);
        };
        let topology = mesh.topology(crate::stage::time_of(time));
        let problem = match &topology.validation {
            Ok(()) => Wire::Null,
            Err(error) => Wire::List(vec![
                Wire::Text(topology_problem_code(error).into()),
                Wire::Text(error.display(self.document().store()).to_string()),
            ]),
        };
        Ok(Wire::List(vec![
            Wire::Ints(topology.face_vertex_counts.to_vec()),
            Wire::Ints(topology.face_vertex_indices.to_vec()),
            Wire::Ints(topology.hole_indices.to_vec()),
            Wire::Text(topology.orientation.as_str().into()),
            Wire::Text(topology.subdivision_scheme.as_str().into()),
            Wire::Number(topology.point_count as f64),
            problem,
        ]))
    }

    /// The type of the prim's subset family `family`
    /// (`subsetFamily:<family>:familyType`), `unrestricted` when
    /// unauthored.
    ///
    /// OpenUSD: `UsdGeomSubset::GetFamilyType`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`.
    pub(crate) fn subset_family_type(&self, path: &str, family: &str) -> Result<String, Error> {
        let prim = self.schema_prim(path)?;
        let scene = self.scene();
        Ok(Imageable::get(&scene, prim).map_or_else(
            || "unrestricted".into(),
            |geom| geom.subset_family_type(family),
        ))
    }

    /// Validates the prim's subset family `family` of `element` subsets,
    /// at `time` or, without one, at the default time and every composed
    /// sample of the members' indices: the problems ([`problems_record`]),
    /// none for a valid family.
    ///
    /// OpenUSD: `UsdGeomSubset::ValidateFamily`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`, `argument.invalid` for an unknown
    /// element type; `value.decode` for topology or indices that fail to
    /// decode.
    pub(crate) fn validate_subset_family(
        &self,
        path: &str,
        element: &str,
        family: &str,
        time: Option<f64>,
    ) -> Result<Wire, Error> {
        let prim = self.schema_prim(path)?;
        let element = element_type(element)?;
        let scene = self.scene();
        let store = self.document().store();
        let Some(geom) = Imageable::get(&scene, prim) else {
            return Err(subset_error(&SubsetError::InvalidGeometry, store, path));
        };
        let validation = match time {
            Some(time) => geom.try_validate_subset_family_at(
                &element,
                family,
                crate::stage::time_of(Some(time)),
            ),
            None => geom.try_validate_subset_family(&element, family),
        }
        .map_err(|error| subset_error(&error, store, path))?;
        Ok(problems_record(&validation, store))
    }

    /// The prim's coordinate-system bindings, each `[name, relationship,
    /// target]`: with `inherited`, every name bound on the prim or an
    /// ancestor, the nearest binding of each, the prim's own first; else
    /// only those the prim's applied `CoordSysAPI` instances author.
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::FindBindingsWithInheritance` and
    /// `GetLocalBindingsForPrim`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`.
    pub(crate) fn coord_sys_bindings(&self, path: &str, inherited: bool) -> Result<Wire, Error> {
        let prim = self.schema_prim(path)?;
        let scene = self.scene();
        let store = self.document().store();
        let bindings = if inherited {
            CoordSysCache::new().bindings(&scene, prim).to_vec()
        } else {
            layerstack_schemas::local_coord_sys_bindings(&scene, prim)
        };
        Ok(Wire::List(
            bindings
                .into_iter()
                .map(|binding| {
                    Wire::List(vec![
                        Wire::text(&*binding.name),
                        property_text(store, binding.relationship),
                        target_text(store, binding.target),
                    ])
                })
                .collect(),
        ))
    }
}

/// The code of a topology problem in a `meshTopology` record:
/// `faceCounts` (a negative count, or counts that overflow),
/// `sizeMismatch`, `vertexIndex` or `decode`.
fn topology_problem_code(error: &MeshTopologyError) -> &'static str {
    match error {
        MeshTopologyError::InvalidFaceCounts => "faceCounts",
        MeshTopologyError::SizeMismatch => "sizeMismatch",
        MeshTopologyError::InvalidVertexIndex(_) => "vertexIndex",
        _ => "decode",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use alloc::{format, string::String};

    use super::*;
    use crate::stage::BROWSER_LIMITS;

    /// A rock mesh bound to a stone material through its parent, with a
    /// face subset bound to moss, a lamp bound to a material that is not
    /// there, and a coordinate system bound on the parent.
    pub(crate) const SHADED: &str = r#"#usda 1.0
def Xform "World" (
    prepend apiSchemas = ["MaterialBindingAPI", "CoordSysAPI:paint"]
) {
    rel material:binding = </Looks/Stone>
    rel coordSys:paint:binding = </World/Projector>
    def Xform "Projector" {
    }
    def Mesh "Rock" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    ) {
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 2, 3, 3, 2, 4, 5]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0), (1, 2, 0), (0, 2, 0)]
        uniform token subsetFamily:materialBind:familyType = "partition"
        def GeomSubset "Top" (
            prepend apiSchemas = ["MaterialBindingAPI"]
        ) {
            uniform token elementType = "face"
            uniform token familyName = "materialBind"
            int[] indices = [1]
            rel material:binding = </Looks/Moss>
        }
    }
    def Mesh "Lamp" {
        rel material:binding = </Looks/Missing>
    }
}
def Scope "Looks" {
    def Material "Stone" {
        token outputs:surface.connect = </Looks/Stone/Surface.outputs:surface>
        def Shader "Surface" (
            sdrMetadata = {
                string role = "surface"
            }
        ) {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/Stone/Texture.outputs:rgb>
            float inputs:roughness = 0.75 (
                sdrMetadata = {
                    string page = "basic"
                }
            )
            token outputs:surface
        }
        def Shader "Texture" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @stone.png@
            float3 outputs:rgb
        }
    }
    def Material "Moss" (
        specializes = </Looks/Stone>
    ) {
    }
}
"#;

    pub(crate) fn open(text: &str) -> Stage {
        Stage::open(
            "scene.usda",
            text.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap()
    }

    /// A record as compact text: lists in brackets, `null`, numbers, and an
    /// array handle as its type and elements.
    pub(crate) fn show(wire: &Wire) -> String {
        match wire {
            Wire::Null => "null".into(),
            Wire::Bool(value) => format!("{value}"),
            Wire::Number(value) => format!("{value}"),
            Wire::Int64(value) => format!("{value}"),
            Wire::UInt64(value) => format!("{value}"),
            Wire::Text(text) => text.clone(),
            Wire::List(items) => {
                let items: Vec<String> = items.iter().map(show).collect();
                format!("[{}]", items.join(", "))
            }
            Wire::Bytes(bytes) => format!("<{} bytes>", bytes.len()),
            Wire::Floats(values) => show_numbers(values),
            Wire::Ints(values) => show_numbers(values),
            Wire::Int64s(values) => show_numbers(values),
            Wire::Array(handle) => format!("{:?}", handle.array()),
        }
    }

    /// Numbers in brackets, as `Wire::Number`s show.
    fn show_numbers<T: core::fmt::Display>(values: &[T]) -> String {
        let items: Vec<String> = values.iter().map(|value| format!("{value}")).collect();
        format!("[{}]", items.join(", "))
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| String::from(*item)).collect()
    }

    #[test]
    fn a_renderer_resolves_bound_materials_inherited_legacy_and_missing() {
        let stage = open(SHADED);
        let bound = stage
            .bound_materials(
                &texts(&["/World/Rock", "/World/Lamp", "/World/Rock/Top"]),
                "",
            )
            .unwrap();
        assert_eq!(
            show(&bound),
            "[[/Looks/Stone, /Looks/Stone, /World.material:binding, direct, null, \
             weakerThanDescendants, , false], \
             [null, /Looks/Missing, /World/Lamp.material:binding, direct, null, \
             weakerThanDescendants, , true], \
             [/Looks/Moss, /Looks/Moss, /World/Rock/Top.material:binding, direct, null, \
             weakerThanDescendants, , false]]"
        );
        let preview = stage
            .bound_materials(&texts(&["/World/Rock"]), "preview")
            .unwrap();
        assert!(
            show(&preview).ends_with(", , false]]"),
            "all-purpose fallback"
        );
        let missing = stage
            .bound_materials(&texts(&["/World/Gone"]), "")
            .unwrap_err();
        assert_eq!(
            (missing.code, missing.path.as_deref()),
            ("prim.not_found", Some("/World/Gone"))
        );
    }

    #[test]
    fn a_renderer_reads_a_meshes_material_subsets_and_their_validation() {
        let stage = open(SHADED);
        let subsets = stage.material_subsets("/World/Rock", "", None).unwrap();
        let shown = show(&subsets);
        assert!(shown.starts_with("[partition, [/Looks/Stone, "), "{shown}");
        assert!(
            shown.contains("[/World/Rock/Top, Int([1]), [/Looks/Moss, "),
            "{shown}"
        );
        // A partition that leaves face 0 unassigned.
        assert!(
            shown.ends_with(
                "[[/World/Rock, null, incompletePartition, null, \
                 the partition leaves some of the geometry unassigned]]]"
            ),
            "{shown}"
        );
        let error = stage.material_subsets("/World", "", None).unwrap_err();
        assert_eq!(error.code, "subset.geometry");
        assert!(
            error.message.contains("/World is not a mesh"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_renderer_reads_a_meshes_topology_and_its_validation() {
        let stage = open(SHADED);
        let topology = stage.mesh_topology("/World/Rock", None).unwrap();
        assert_eq!(
            show(&topology),
            "[[4, 4], [0, 1, 2, 3, 3, 2, 4, 5], [], rightHanded, catmullClark, 6, null]"
        );
        // No points nor faces: an empty topology is valid and draws nothing.
        let empty = stage.mesh_topology("/World/Lamp", None).unwrap();
        assert_eq!(
            show(&empty),
            "[[], [], [], rightHanded, catmullClark, 0, null]"
        );
        // Not a mesh.
        assert_eq!(show(&stage.mesh_topology("/World", None).unwrap()), "null");
    }

    #[test]
    fn a_renderer_walks_a_material_network_to_its_inputs() {
        let stage = open(SHADED);
        assert_eq!(show(&stage.material("/Looks/Stone").unwrap()), "[null]");
        assert_eq!(
            show(&stage.material("/Looks/Moss").unwrap()),
            "[/Looks/Stone]"
        );
        assert_eq!(stage.material("/World/Rock").unwrap(), Wire::Null);
        assert_eq!(
            show(
                &stage
                    .material_terminal("/Looks/Moss", "surface", &[])
                    .unwrap()
            ),
            "[/Looks/Moss/Surface, , [[/Looks/Moss/Surface.outputs:surface, \
             [/Looks/Moss.outputs:surface, /Looks/Moss/Surface.outputs:surface]]]]"
        );
        assert_eq!(
            show(&stage.shader("/Looks/Stone/Surface").unwrap()),
            "[UsdPreviewSurface, id, [[role, surface]]]"
        );
        assert_eq!(stage.shader("/Looks/Stone").unwrap(), Wire::Null);
        assert_eq!(
            show(&stage.ports("/Looks/Stone/Surface", "input").unwrap()),
            "[[/Looks/Stone/Surface.inputs:diffuseColor, input, color3f, [], \
             [/Looks/Stone/Texture.outputs:rgb], []], \
             [/Looks/Stone/Surface.inputs:roughness, input, float, [[page, basic]], [], []]]"
        );
        // A connected input's value comes from the texture's output; an
        // unconnected one is its own authored value.
        assert_eq!(
            show(
                &stage
                    .value_sources("/Looks/Stone/Surface.inputs:diffuseColor")
                    .unwrap()
            ),
            "[[[/Looks/Stone/Texture.outputs:rgb, shaderOutput, \
             [/Looks/Stone/Surface.inputs:diffuseColor, /Looks/Stone/Texture.outputs:rgb]]], []]"
        );
        assert_eq!(
            show(
                &stage
                    .value_sources("/Looks/Stone/Surface.inputs:roughness")
                    .unwrap()
            ),
            "[[[/Looks/Stone/Surface.inputs:roughness, authoredValue, \
             [/Looks/Stone/Surface.inputs:roughness]]], []]"
        );
        assert_eq!(
            stage.port("/Looks/Stone/Surface.info:id").unwrap(),
            Wire::Null
        );
        assert_eq!(
            stage
                .value_sources("/Looks/Stone/Surface.inputs:none")
                .unwrap_err()
                .code,
            "prim.not_found"
        );
        assert_eq!(
            stage
                .material_terminal("/Looks/Stone", "glow", &[])
                .unwrap_err()
                .code,
            "argument.invalid"
        );
    }

    #[test]
    fn an_editor_checks_a_connection_source_path_against_the_stage() {
        let stage = open(SHADED);
        let info = |path: &str| show(&stage.connection_source_info(path));
        assert_eq!(
            info("/Looks/Stone/Texture.outputs:rgb"),
            "[/Looks/Stone/Texture, rgb, output, float3, /Looks/Stone/Texture.outputs:rgb]"
        );
        // A port that is not there yet is still a source to connect to.
        assert_eq!(
            info("/Looks/Stone/Texture.outputs:alpha"),
            "[/Looks/Stone/Texture, alpha, output, null, /Looks/Stone/Texture.outputs:alpha]"
        );
        assert_eq!(
            info("/Looks/Gone.outputs:rgb"),
            "[null, rgb, output, null, null]"
        );
        assert_eq!(
            info("/Looks/Stone/Texture.info:id"),
            "[/Looks/Stone/Texture, info:id, null, token, null]"
        );
        // Neither a prim path nor a relational attribute path names a
        // source.
        for path in [
            "/Looks/Stone/Texture",
            "/Looks/Stone/Texture.outputs:rgb[/X].inputs:y",
            "not a path",
        ] {
            assert_eq!(info(path), "[null, , null, null, null]", "{path}");
        }
    }

    #[test]
    fn subsets_and_coordinate_systems_read_as_records() {
        let stage = open(SHADED);
        assert_eq!(
            show(
                &stage
                    .geom_subsets("/World/Rock", Some("face"), None, None)
                    .unwrap()
            ),
            "[[/World/Rock/Top, face, materialBind, Int([1])]]"
        );
        assert_eq!(
            show(
                &stage
                    .geom_subsets("/World/Rock", Some("point"), None, None)
                    .unwrap()
            ),
            "[]"
        );
        assert_eq!(
            stage
                .geom_subsets("/World/Rock", Some("corner"), None, None)
                .unwrap_err()
                .code,
            "argument.invalid"
        );
        assert_eq!(
            stage
                .subset_family_type("/World/Rock", "materialBind")
                .unwrap(),
            "partition"
        );
        assert_eq!(
            show(
                &stage
                    .validate_subset_family("/World/Rock", "face", "materialBind", Some(0.0))
                    .unwrap()
            ),
            "[[/World/Rock, 0, incompletePartition, null, \
             the partition leaves some of the geometry unassigned]]"
        );
        assert_eq!(
            show(&stage.coord_sys_bindings("/World/Rock", true).unwrap()),
            "[[paint, /World.coordSys:paint:binding, /World/Projector]]"
        );
        assert_eq!(
            show(&stage.coord_sys_bindings("/World/Rock", false).unwrap()),
            "[]"
        );
        assert_eq!(
            show(&stage.coord_sys_bindings("/World", false).unwrap()),
            "[[paint, /World.coordSys:paint:binding, /World/Projector]]"
        );
    }
}
