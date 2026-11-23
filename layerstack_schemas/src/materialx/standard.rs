// Copyright Contributors to the MaterialX Project
// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0

//! Selected MaterialX shader nodes, keyed by `info:id`.
//! Inputs read authored values at the requested time; definition defaults are
//! explicit associated functions. These views do not evaluate shaders or follow
//! input connections. Port creation uses ordinary shading transactions.
//! Spec: AOUSD Core §7.6.4.1 (typed attributes), §12.3 (value resolution),
//! §12.4 (connections). Node interfaces are copied from the selected MaterialX NodeDefs.
#![allow(
    clippy::doc_markdown,
    clippy::too_long_first_doc_paragraph,
    reason = "documentation is copied verbatim from MaterialX"
)]

extern crate alloc;
use alloc::sync::Arc;
use core::ops::Deref;
use layerstack::{PathId, PropertyType};
use layerstack_schemas::materialx::{MaterialXNode, MaterialXPort};
use layerstack_schemas::shading::PortKind;
use layerstack_schemas::shading::{Port, PortEdit, PortError};
use layerstack_schemas::usd_shade::{NodeDefApiImplementationSource, Shader, ShaderEdit};
use layerstack_schemas::{CannotEdit, Scene, SchemaEdit};

/// The `ND_add_color3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdAddColor3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdAddColor3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdAddColor3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_add_color3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<NdAddColor3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdAddColor3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in1",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:in1",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "in2",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:in2",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color3",
                usd_name: "outputs:out",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "in1",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:in1` port, if it exists. "]
    #[must_use]
    pub fn in1_input(&self) -> Option<Port<'a>> {
        self.shader.input("in1")
    }
    #[doc = "The composed `inputs:in1`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in1(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in1", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in1` at `time`.
    #[must_use]
    pub fn in1_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in1", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in1` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in1_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:in2` port, if it exists. "]
    #[must_use]
    pub fn in2_input(&self) -> Option<Port<'a>> {
        self.shader.input("in2")
    }
    #[doc = "The composed `inputs:in2`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in2(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in2", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in2` at `time`.
    #[must_use]
    pub fn in2_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in2", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in2` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in2_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_add_color3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdAddColor3Edit {
    shader: ShaderEdit,
}
impl Deref for NdAddColor3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdAddColor3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in1` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in1_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "in1", ty)
    }
    /// Authors `inputs:in1` at default time, creating the typed input if needed.
    pub fn set_in1(&self, edit: &mut SchemaEdit<'_>, value: [f32; 3]) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in1` sample in stage time through the edit target.
    pub fn set_in1_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in2` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in2_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "in2", ty)
    }
    /// Authors `inputs:in2` at default time, creating the typed input if needed.
    pub fn set_in2(&self, edit: &mut SchemaEdit<'_>, value: [f32; 3]) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in2` sample in stage time through the edit target.
    pub fn set_in2_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_add_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdAddFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdAddFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdAddFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_add_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<NdAddFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdAddFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in1",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in1",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "in2",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in2",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "in1",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:in1` port, if it exists. "]
    #[must_use]
    pub fn in1_input(&self) -> Option<Port<'a>> {
        self.shader.input("in1")
    }
    #[doc = "The composed `inputs:in1`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in1(&self) -> Option<f32> {
        self.read_value("inputs:in1", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in1` at `time`.
    #[must_use]
    pub fn in1_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in1", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in1` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in1_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:in2` port, if it exists. "]
    #[must_use]
    pub fn in2_input(&self) -> Option<Port<'a>> {
        self.shader.input("in2")
    }
    #[doc = "The composed `inputs:in2`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in2(&self) -> Option<f32> {
        self.read_value("inputs:in2", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in2` at `time`.
    #[must_use]
    pub fn in2_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in2", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in2` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in2_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_add_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdAddFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdAddFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdAddFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in1` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in1_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in1", ty)
    }
    /// Authors `inputs:in1` at default time, creating the typed input if needed.
    pub fn set_in1(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in1` sample in stage time through the edit target.
    pub fn set_in1_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in2` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in2_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in2", ty)
    }
    /// Authors `inputs:in2` at default time, creating the typed input if needed.
    pub fn set_in2(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in2` sample in stage time through the edit target.
    pub fn set_in2_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_bump_vector3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdBumpVector3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdBumpVector3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdBumpVector3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_bump_vector3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdBumpVector3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdBumpVector3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "bitangent",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:bitangent",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Bworld",
                )),
            },
            MaterialXPort {
                name: "height",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:height",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "normal",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:normal",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Nworld",
                )),
            },
            MaterialXPort {
                name: "scale",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:scale",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "tangent",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:tangent",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Tworld",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "vector3",
                usd_name: "outputs:out",
                usd_type_name: "float3",
                default: None,
            },
        ],
    };
    #[doc = "The authored `inputs:bitangent` port, if it exists. Surface bitangent vector, defaults to the current world-space bitangent vector."]
    #[must_use]
    pub fn bitangent_input(&self) -> Option<Port<'a>> {
        self.shader.input("bitangent")
    }
    #[doc = "The composed `inputs:bitangent`, without node defaults or connection evaluation. Surface bitangent vector, defaults to the current world-space bitangent vector."]
    #[must_use]
    pub fn bitangent(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:bitangent", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:bitangent` at `time`.
    #[must_use]
    pub fn bitangent_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:bitangent",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:height` port, if it exists. Amount to offset the surface normal."]
    #[must_use]
    pub fn height_input(&self) -> Option<Port<'a>> {
        self.shader.input("height")
    }
    #[doc = "The composed `inputs:height`, without node defaults or connection evaluation. Amount to offset the surface normal."]
    #[must_use]
    pub fn height(&self) -> Option<f32> {
        self.read_value("inputs:height", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:height` at `time`.
    #[must_use]
    pub fn height_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:height", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:height` default in the node definition, separate from authored values.
    #[must_use]
    pub fn height_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:normal` port, if it exists. Surface normal; defaults to the current world-space normal."]
    #[must_use]
    pub fn normal_input(&self) -> Option<Port<'a>> {
        self.shader.input("normal")
    }
    #[doc = "The composed `inputs:normal`, without node defaults or connection evaluation. Surface normal; defaults to the current world-space normal."]
    #[must_use]
    pub fn normal(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:normal", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:normal` at `time`.
    #[must_use]
    pub fn normal_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:normal",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:scale` port, if it exists. Scalar to adjust the height amount."]
    #[must_use]
    pub fn scale_input(&self) -> Option<Port<'a>> {
        self.shader.input("scale")
    }
    #[doc = "The composed `inputs:scale`, without node defaults or connection evaluation. Scalar to adjust the height amount."]
    #[must_use]
    pub fn scale(&self) -> Option<f32> {
        self.read_value("inputs:scale", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:scale` at `time`.
    #[must_use]
    pub fn scale_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:scale", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:scale` default in the node definition, separate from authored values.
    #[must_use]
    pub fn scale_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:tangent` port, if it exists. Surface tangent vector, defaults to the current world-space tangent vector."]
    #[must_use]
    pub fn tangent_input(&self) -> Option<Port<'a>> {
        self.shader.input("tangent")
    }
    #[doc = "The composed `inputs:tangent`, without node defaults or connection evaluation. Surface tangent vector, defaults to the current world-space tangent vector."]
    #[must_use]
    pub fn tangent(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:tangent", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:tangent` at `time`.
    #[must_use]
    pub fn tangent_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:tangent",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `outputs:out` port, if it exists. Offset surface normal; connect this to a shader's 'normal' input."]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_bump_vector3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdBumpVector3Edit {
    shader: ShaderEdit,
}
impl Deref for NdBumpVector3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdBumpVector3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:bitangent` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_bitangent_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "bitangent", ty)
    }
    /// Authors `inputs:bitangent` at default time, creating the typed input if needed.
    pub fn set_bitangent(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_bitangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:bitangent` sample in stage time through the edit target.
    pub fn set_bitangent_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_bitangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:height` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_height_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "height", ty)
    }
    /// Authors `inputs:height` at default time, creating the typed input if needed.
    pub fn set_height(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_height_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:height` sample in stage time through the edit target.
    pub fn set_height_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_height_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:normal` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_normal_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "normal", ty)
    }
    /// Authors `inputs:normal` at default time, creating the typed input if needed.
    pub fn set_normal(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:normal` sample in stage time through the edit target.
    pub fn set_normal_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:scale` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_scale_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "scale", ty)
    }
    /// Authors `inputs:scale` at default time, creating the typed input if needed.
    pub fn set_scale(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:scale` sample in stage time through the edit target.
    pub fn set_scale_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:tangent` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_tangent_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "tangent", ty)
    }
    /// Authors `inputs:tangent` at default time, creating the typed input if needed.
    pub fn set_tangent(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:tangent` sample in stage time through the edit target.
    pub fn set_tangent_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_clamp_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdClampFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdClampFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdClampFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_clamp_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<NdClampFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdClampFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "high",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:high",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "in",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "low",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:low",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "in",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:high` port, if it exists. "]
    #[must_use]
    pub fn high_input(&self) -> Option<Port<'a>> {
        self.shader.input("high")
    }
    #[doc = "The composed `inputs:high`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn high(&self) -> Option<f32> {
        self.read_value("inputs:high", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:high` at `time`.
    #[must_use]
    pub fn high_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:high", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:high` default in the node definition, separate from authored values.
    #[must_use]
    pub fn high_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:in` port, if it exists. "]
    #[must_use]
    pub fn in_value_input(&self) -> Option<Port<'a>> {
        self.shader.input("in")
    }
    #[doc = "The composed `inputs:in`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in_value(&self) -> Option<f32> {
        self.read_value("inputs:in", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in` at `time`.
    #[must_use]
    pub fn in_value_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in_value_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:low` port, if it exists. "]
    #[must_use]
    pub fn low_input(&self) -> Option<Port<'a>> {
        self.shader.input("low")
    }
    #[doc = "The composed `inputs:low`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn low(&self) -> Option<f32> {
        self.read_value("inputs:low", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:low` at `time`.
    #[must_use]
    pub fn low_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:low", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:low` default in the node definition, separate from authored values.
    #[must_use]
    pub fn low_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_clamp_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdClampFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdClampFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdClampFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:high` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_high_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "high", ty)
    }
    /// Authors `inputs:high` at default time, creating the typed input if needed.
    pub fn set_high(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_high_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:high` sample in stage time through the edit target.
    pub fn set_high_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_high_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in_value_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in", ty)
    }
    /// Authors `inputs:in` at default time, creating the typed input if needed.
    pub fn set_in_value(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in` sample in stage time through the edit target.
    pub fn set_in_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:low` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_low_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "low", ty)
    }
    /// Authors `inputs:low` at default time, creating the typed input if needed.
    pub fn set_low(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_low_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:low` sample in stage time through the edit target.
    pub fn set_low_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_low_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_colorcorrect_color3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdColorcorrectColor3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdColorcorrectColor3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdColorcorrectColor3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_colorcorrect_color3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdColorcorrectColor3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdColorcorrectColor3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "contrast",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:contrast",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "contrastpivot",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:contrastpivot",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.5",
                )),
            },
            MaterialXPort {
                name: "exposure",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:exposure",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "gain",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:gain",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "gamma",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:gamma",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "hue",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:hue",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "in",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:in",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "lift",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:lift",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "saturation",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:saturation",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color3",
                usd_name: "outputs:out",
                usd_type_name: "color3f",
                default: None,
            },
        ],
    };
    #[doc = "The authored `inputs:contrast` port, if it exists. Linearly increase or decrease the color contrast."]
    #[must_use]
    pub fn contrast_input(&self) -> Option<Port<'a>> {
        self.shader.input("contrast")
    }
    #[doc = "The composed `inputs:contrast`, without node defaults or connection evaluation. Linearly increase or decrease the color contrast."]
    #[must_use]
    pub fn contrast(&self) -> Option<f32> {
        self.read_value("inputs:contrast", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:contrast` at `time`.
    #[must_use]
    pub fn contrast_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:contrast",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:contrast` default in the node definition, separate from authored values.
    #[must_use]
    pub fn contrast_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:contrastpivot` port, if it exists. Pivot value around which contrast applies. This value will not change as contrast is adjusted."]
    #[must_use]
    pub fn contrastpivot_input(&self) -> Option<Port<'a>> {
        self.shader.input("contrastpivot")
    }
    #[doc = "The composed `inputs:contrastpivot`, without node defaults or connection evaluation. Pivot value around which contrast applies. This value will not change as contrast is adjusted."]
    #[must_use]
    pub fn contrastpivot(&self) -> Option<f32> {
        self.read_value(
            "inputs:contrastpivot",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:contrastpivot` at `time`.
    #[must_use]
    pub fn contrastpivot_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:contrastpivot",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:contrastpivot` default in the node definition, separate from authored values.
    #[must_use]
    pub fn contrastpivot_default() -> f32 {
        0.5_f32
    }
    #[doc = "The authored `inputs:exposure` port, if it exists. Multiplier which increases or decreases color brightness by 2^value."]
    #[must_use]
    pub fn exposure_input(&self) -> Option<Port<'a>> {
        self.shader.input("exposure")
    }
    #[doc = "The composed `inputs:exposure`, without node defaults or connection evaluation. Multiplier which increases or decreases color brightness by 2^value."]
    #[must_use]
    pub fn exposure(&self) -> Option<f32> {
        self.read_value("inputs:exposure", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:exposure` at `time`.
    #[must_use]
    pub fn exposure_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:exposure",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:exposure` default in the node definition, separate from authored values.
    #[must_use]
    pub fn exposure_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:gain` port, if it exists. Multiplier increases lighter color values, leaving black values unchanged."]
    #[must_use]
    pub fn gain_input(&self) -> Option<Port<'a>> {
        self.shader.input("gain")
    }
    #[doc = "The composed `inputs:gain`, without node defaults or connection evaluation. Multiplier increases lighter color values, leaving black values unchanged."]
    #[must_use]
    pub fn gain(&self) -> Option<f32> {
        self.read_value("inputs:gain", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:gain` at `time`.
    #[must_use]
    pub fn gain_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:gain", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:gain` default in the node definition, separate from authored values.
    #[must_use]
    pub fn gain_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:gamma` port, if it exists. Applies a gamma correction to the color."]
    #[must_use]
    pub fn gamma_input(&self) -> Option<Port<'a>> {
        self.shader.input("gamma")
    }
    #[doc = "The composed `inputs:gamma`, without node defaults or connection evaluation. Applies a gamma correction to the color."]
    #[must_use]
    pub fn gamma(&self) -> Option<f32> {
        self.read_value("inputs:gamma", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:gamma` at `time`.
    #[must_use]
    pub fn gamma_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:gamma", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:gamma` default in the node definition, separate from authored values.
    #[must_use]
    pub fn gamma_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:hue` port, if it exists. Rotates the color hue, with values wrapping at 0-1 boundaries."]
    #[must_use]
    pub fn hue_input(&self) -> Option<Port<'a>> {
        self.shader.input("hue")
    }
    #[doc = "The composed `inputs:hue`, without node defaults or connection evaluation. Rotates the color hue, with values wrapping at 0-1 boundaries."]
    #[must_use]
    pub fn hue(&self) -> Option<f32> {
        self.read_value("inputs:hue", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:hue` at `time`.
    #[must_use]
    pub fn hue_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:hue", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:hue` default in the node definition, separate from authored values.
    #[must_use]
    pub fn hue_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:in` port, if it exists. The input color to be adjusted."]
    #[must_use]
    pub fn in_value_input(&self) -> Option<Port<'a>> {
        self.shader.input("in")
    }
    #[doc = "The composed `inputs:in`, without node defaults or connection evaluation. The input color to be adjusted."]
    #[must_use]
    pub fn in_value(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in` at `time`.
    #[must_use]
    pub fn in_value_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in_value_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:lift` port, if it exists. Raise the dark color values, leaving the white values unchanged."]
    #[must_use]
    pub fn lift_input(&self) -> Option<Port<'a>> {
        self.shader.input("lift")
    }
    #[doc = "The composed `inputs:lift`, without node defaults or connection evaluation. Raise the dark color values, leaving the white values unchanged."]
    #[must_use]
    pub fn lift(&self) -> Option<f32> {
        self.read_value("inputs:lift", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:lift` at `time`.
    #[must_use]
    pub fn lift_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:lift", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:lift` default in the node definition, separate from authored values.
    #[must_use]
    pub fn lift_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:saturation` port, if it exists. Adjusts the input color saturation level."]
    #[must_use]
    pub fn saturation_input(&self) -> Option<Port<'a>> {
        self.shader.input("saturation")
    }
    #[doc = "The composed `inputs:saturation`, without node defaults or connection evaluation. Adjusts the input color saturation level."]
    #[must_use]
    pub fn saturation(&self) -> Option<f32> {
        self.read_value("inputs:saturation", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:saturation` at `time`.
    #[must_use]
    pub fn saturation_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:saturation",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:saturation` default in the node definition, separate from authored values.
    #[must_use]
    pub fn saturation_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_colorcorrect_color3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdColorcorrectColor3Edit {
    shader: ShaderEdit,
}
impl Deref for NdColorcorrectColor3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdColorcorrectColor3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:contrast` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_contrast_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "contrast", ty)
    }
    /// Authors `inputs:contrast` at default time, creating the typed input if needed.
    pub fn set_contrast(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_contrast_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:contrast` sample in stage time through the edit target.
    pub fn set_contrast_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_contrast_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:contrastpivot` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_contrastpivot_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "contrastpivot", ty)
    }
    /// Authors `inputs:contrastpivot` at default time, creating the typed input if needed.
    pub fn set_contrastpivot(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_contrastpivot_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:contrastpivot` sample in stage time through the edit target.
    pub fn set_contrastpivot_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_contrastpivot_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:exposure` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_exposure_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "exposure", ty)
    }
    /// Authors `inputs:exposure` at default time, creating the typed input if needed.
    pub fn set_exposure(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_exposure_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:exposure` sample in stage time through the edit target.
    pub fn set_exposure_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_exposure_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:gain` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_gain_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "gain", ty)
    }
    /// Authors `inputs:gain` at default time, creating the typed input if needed.
    pub fn set_gain(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_gain_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:gain` sample in stage time through the edit target.
    pub fn set_gain_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_gain_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:gamma` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_gamma_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "gamma", ty)
    }
    /// Authors `inputs:gamma` at default time, creating the typed input if needed.
    pub fn set_gamma(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_gamma_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:gamma` sample in stage time through the edit target.
    pub fn set_gamma_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_gamma_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:hue` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_hue_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "hue", ty)
    }
    /// Authors `inputs:hue` at default time, creating the typed input if needed.
    pub fn set_hue(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_hue_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:hue` sample in stage time through the edit target.
    pub fn set_hue_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_hue_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in_value_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "in", ty)
    }
    /// Authors `inputs:in` at default time, creating the typed input if needed.
    pub fn set_in_value(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in` sample in stage time through the edit target.
    pub fn set_in_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:lift` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_lift_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "lift", ty)
    }
    /// Authors `inputs:lift` at default time, creating the typed input if needed.
    pub fn set_lift(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_lift_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:lift` sample in stage time through the edit target.
    pub fn set_lift_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_lift_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:saturation` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_saturation_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "saturation", ty)
    }
    /// Authors `inputs:saturation` at default time, creating the typed input if needed.
    pub fn set_saturation(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_saturation_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:saturation` sample in stage time through the edit target.
    pub fn set_saturation_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_saturation_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_combine3_color3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdCombine3Color3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdCombine3Color3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdCombine3Color3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_combine3_color3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdCombine3Color3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdCombine3Color3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in1",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in1",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "in2",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in2",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "in3",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in3",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color3",
                usd_name: "outputs:out",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:in1` port, if it exists. "]
    #[must_use]
    pub fn in1_input(&self) -> Option<Port<'a>> {
        self.shader.input("in1")
    }
    #[doc = "The composed `inputs:in1`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in1(&self) -> Option<f32> {
        self.read_value("inputs:in1", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in1` at `time`.
    #[must_use]
    pub fn in1_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in1", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in1` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in1_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:in2` port, if it exists. "]
    #[must_use]
    pub fn in2_input(&self) -> Option<Port<'a>> {
        self.shader.input("in2")
    }
    #[doc = "The composed `inputs:in2`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in2(&self) -> Option<f32> {
        self.read_value("inputs:in2", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in2` at `time`.
    #[must_use]
    pub fn in2_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in2", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in2` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in2_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:in3` port, if it exists. "]
    #[must_use]
    pub fn in3_input(&self) -> Option<Port<'a>> {
        self.shader.input("in3")
    }
    #[doc = "The composed `inputs:in3`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in3(&self) -> Option<f32> {
        self.read_value("inputs:in3", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in3` at `time`.
    #[must_use]
    pub fn in3_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in3", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in3` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in3_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_combine3_color3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdCombine3Color3Edit {
    shader: ShaderEdit,
}
impl Deref for NdCombine3Color3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdCombine3Color3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in1` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in1_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in1", ty)
    }
    /// Authors `inputs:in1` at default time, creating the typed input if needed.
    pub fn set_in1(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in1` sample in stage time through the edit target.
    pub fn set_in1_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in2` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in2_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in2", ty)
    }
    /// Authors `inputs:in2` at default time, creating the typed input if needed.
    pub fn set_in2(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in2` sample in stage time through the edit target.
    pub fn set_in2_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in3` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in3_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in3", ty)
    }
    /// Authors `inputs:in3` at default time, creating the typed input if needed.
    pub fn set_in3(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in3_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in3` sample in stage time through the edit target.
    pub fn set_in3_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in3_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_extract_color3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdExtractColor3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdExtractColor3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdExtractColor3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_extract_color3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdExtractColor3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdExtractColor3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:in",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "index",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:index",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:in` port, if it exists. "]
    #[must_use]
    pub fn in_value_input(&self) -> Option<Port<'a>> {
        self.shader.input("in")
    }
    #[doc = "The composed `inputs:in`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in_value(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in` at `time`.
    #[must_use]
    pub fn in_value_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in_value_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:index` port, if it exists. "]
    #[must_use]
    pub fn index_input(&self) -> Option<Port<'a>> {
        self.shader.input("index")
    }
    #[doc = "The composed `inputs:index`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn index(&self) -> Option<i32> {
        self.read_value("inputs:index", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:index` at `time`.
    #[must_use]
    pub fn index_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at("inputs:index", time, layerstack_schemas::value::read_int)
    }
    /// The `inputs:index` default in the node definition, separate from authored values.
    #[must_use]
    pub fn index_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_extract_color3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdExtractColor3Edit {
    shader: ShaderEdit,
}
impl Deref for NdExtractColor3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdExtractColor3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in_value_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "in", ty)
    }
    /// Authors `inputs:in` at default time, creating the typed input if needed.
    pub fn set_in_value(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in` sample in stage time through the edit target.
    pub fn set_in_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:index` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_index_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "index", ty)
    }
    /// Authors `inputs:index` at default time, creating the typed input if needed.
    pub fn set_index(&self, edit: &mut SchemaEdit<'_>, value: i32) -> Result<&Self, PortError> {
        let port = self.create_index_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:index` sample in stage time through the edit target.
    pub fn set_index_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_index_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_extract_vector3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdExtractVector3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdExtractVector3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdExtractVector3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_extract_vector3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdExtractVector3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdExtractVector3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:in",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "index",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:index",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:in` port, if it exists. "]
    #[must_use]
    pub fn in_value_input(&self) -> Option<Port<'a>> {
        self.shader.input("in")
    }
    #[doc = "The composed `inputs:in`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in_value(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in` at `time`.
    #[must_use]
    pub fn in_value_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in_value_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:index` port, if it exists. "]
    #[must_use]
    pub fn index_input(&self) -> Option<Port<'a>> {
        self.shader.input("index")
    }
    #[doc = "The composed `inputs:index`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn index(&self) -> Option<i32> {
        self.read_value("inputs:index", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:index` at `time`.
    #[must_use]
    pub fn index_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at("inputs:index", time, layerstack_schemas::value::read_int)
    }
    /// The `inputs:index` default in the node definition, separate from authored values.
    #[must_use]
    pub fn index_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_extract_vector3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdExtractVector3Edit {
    shader: ShaderEdit,
}
impl Deref for NdExtractVector3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdExtractVector3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in_value_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "in", ty)
    }
    /// Authors `inputs:in` at default time, creating the typed input if needed.
    pub fn set_in_value(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in` sample in stage time through the edit target.
    pub fn set_in_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:index` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_index_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "index", ty)
    }
    /// Authors `inputs:index` at default time, creating the typed input if needed.
    pub fn set_index(&self, edit: &mut SchemaEdit<'_>, value: i32) -> Result<&Self, PortError> {
        let port = self.create_index_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:index` sample in stage time through the edit target.
    pub fn set_index_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_index_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_gltf_pbr_surfaceshader` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdGltfPbrSurfaceshader<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdGltfPbrSurfaceshader<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdGltfPbrSurfaceshader<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_gltf_pbr_surfaceshader";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdGltfPbrSurfaceshaderEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdGltfPbrSurfaceshaderEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "alpha",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:alpha",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "alpha_cutoff",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:alpha_cutoff",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.5",
                )),
            },
            MaterialXPort {
                name: "alpha_mode",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:alpha_mode",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "anisotropy_rotation",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:anisotropy_rotation",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "anisotropy_strength",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:anisotropy_strength",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "attenuation_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:attenuation_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "attenuation_distance",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:attenuation_distance",
                usd_type_name: "float",
                default: None,
            },
            MaterialXPort {
                name: "base_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:base_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "clearcoat",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:clearcoat",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "clearcoat_normal",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:clearcoat_normal",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Nworld",
                )),
            },
            MaterialXPort {
                name: "clearcoat_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:clearcoat_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "dispersion",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:dispersion",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "emissive",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:emissive",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0, 0, 0",
                )),
            },
            MaterialXPort {
                name: "emissive_strength",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:emissive_strength",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "ior",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:ior",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.5",
                )),
            },
            MaterialXPort {
                name: "iridescence",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:iridescence",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "iridescence_ior",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:iridescence_ior",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.3",
                )),
            },
            MaterialXPort {
                name: "iridescence_thickness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:iridescence_thickness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "100",
                )),
            },
            MaterialXPort {
                name: "metallic",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:metallic",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "normal",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:normal",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Nworld",
                )),
            },
            MaterialXPort {
                name: "occlusion",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:occlusion",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "sheen_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:sheen_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0, 0, 0",
                )),
            },
            MaterialXPort {
                name: "sheen_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:sheen_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "specular",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "specular_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:specular_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "tangent",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:tangent",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Tworld",
                )),
            },
            MaterialXPort {
                name: "thickness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:thickness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "transmission",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "surfaceshader",
                usd_name: "outputs:surface",
                usd_type_name: "token",
                default: None,
            },
        ],
    };
    #[doc = "The authored `inputs:alpha` port, if it exists. "]
    #[must_use]
    pub fn alpha_input(&self) -> Option<Port<'a>> {
        self.shader.input("alpha")
    }
    #[doc = "The composed `inputs:alpha`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn alpha(&self) -> Option<f32> {
        self.read_value("inputs:alpha", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:alpha` at `time`.
    #[must_use]
    pub fn alpha_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:alpha", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:alpha` default in the node definition, separate from authored values.
    #[must_use]
    pub fn alpha_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:alpha_cutoff` port, if it exists. "]
    #[must_use]
    pub fn alpha_cutoff_input(&self) -> Option<Port<'a>> {
        self.shader.input("alpha_cutoff")
    }
    #[doc = "The composed `inputs:alpha_cutoff`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn alpha_cutoff(&self) -> Option<f32> {
        self.read_value("inputs:alpha_cutoff", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:alpha_cutoff` at `time`.
    #[must_use]
    pub fn alpha_cutoff_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:alpha_cutoff",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:alpha_cutoff` default in the node definition, separate from authored values.
    #[must_use]
    pub fn alpha_cutoff_default() -> f32 {
        0.5_f32
    }
    #[doc = "The authored `inputs:alpha_mode` port, if it exists. "]
    #[must_use]
    pub fn alpha_mode_input(&self) -> Option<Port<'a>> {
        self.shader.input("alpha_mode")
    }
    #[doc = "The composed `inputs:alpha_mode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn alpha_mode(&self) -> Option<i32> {
        self.read_value("inputs:alpha_mode", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:alpha_mode` at `time`.
    #[must_use]
    pub fn alpha_mode_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:alpha_mode",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:alpha_mode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn alpha_mode_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:anisotropy_rotation` port, if it exists. "]
    #[must_use]
    pub fn anisotropy_rotation_input(&self) -> Option<Port<'a>> {
        self.shader.input("anisotropy_rotation")
    }
    #[doc = "The composed `inputs:anisotropy_rotation`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn anisotropy_rotation(&self) -> Option<f32> {
        self.read_value(
            "inputs:anisotropy_rotation",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:anisotropy_rotation` at `time`.
    #[must_use]
    pub fn anisotropy_rotation_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:anisotropy_rotation",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:anisotropy_rotation` default in the node definition, separate from authored values.
    #[must_use]
    pub fn anisotropy_rotation_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:anisotropy_strength` port, if it exists. "]
    #[must_use]
    pub fn anisotropy_strength_input(&self) -> Option<Port<'a>> {
        self.shader.input("anisotropy_strength")
    }
    #[doc = "The composed `inputs:anisotropy_strength`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn anisotropy_strength(&self) -> Option<f32> {
        self.read_value(
            "inputs:anisotropy_strength",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:anisotropy_strength` at `time`.
    #[must_use]
    pub fn anisotropy_strength_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:anisotropy_strength",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:anisotropy_strength` default in the node definition, separate from authored values.
    #[must_use]
    pub fn anisotropy_strength_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:attenuation_color` port, if it exists. "]
    #[must_use]
    pub fn attenuation_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("attenuation_color")
    }
    #[doc = "The composed `inputs:attenuation_color`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn attenuation_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:attenuation_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:attenuation_color` at `time`.
    #[must_use]
    pub fn attenuation_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:attenuation_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:attenuation_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn attenuation_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:attenuation_distance` port, if it exists. "]
    #[must_use]
    pub fn attenuation_distance_input(&self) -> Option<Port<'a>> {
        self.shader.input("attenuation_distance")
    }
    #[doc = "The composed `inputs:attenuation_distance`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn attenuation_distance(&self) -> Option<f32> {
        self.read_value(
            "inputs:attenuation_distance",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:attenuation_distance` at `time`.
    #[must_use]
    pub fn attenuation_distance_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:attenuation_distance",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    #[doc = "The authored `inputs:base_color` port, if it exists. "]
    #[must_use]
    pub fn base_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("base_color")
    }
    #[doc = "The composed `inputs:base_color`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn base_color(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:base_color", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:base_color` at `time`.
    #[must_use]
    pub fn base_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:base_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:base_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn base_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:clearcoat` port, if it exists. "]
    #[must_use]
    pub fn clearcoat_input(&self) -> Option<Port<'a>> {
        self.shader.input("clearcoat")
    }
    #[doc = "The composed `inputs:clearcoat`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn clearcoat(&self) -> Option<f32> {
        self.read_value("inputs:clearcoat", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:clearcoat` at `time`.
    #[must_use]
    pub fn clearcoat_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:clearcoat",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:clearcoat` default in the node definition, separate from authored values.
    #[must_use]
    pub fn clearcoat_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:clearcoat_normal` port, if it exists. "]
    #[must_use]
    pub fn clearcoat_normal_input(&self) -> Option<Port<'a>> {
        self.shader.input("clearcoat_normal")
    }
    #[doc = "The composed `inputs:clearcoat_normal`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn clearcoat_normal(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:clearcoat_normal",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:clearcoat_normal` at `time`.
    #[must_use]
    pub fn clearcoat_normal_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:clearcoat_normal",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:clearcoat_roughness` port, if it exists. "]
    #[must_use]
    pub fn clearcoat_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("clearcoat_roughness")
    }
    #[doc = "The composed `inputs:clearcoat_roughness`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn clearcoat_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:clearcoat_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:clearcoat_roughness` at `time`.
    #[must_use]
    pub fn clearcoat_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:clearcoat_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:clearcoat_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn clearcoat_roughness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:dispersion` port, if it exists. "]
    #[must_use]
    pub fn dispersion_input(&self) -> Option<Port<'a>> {
        self.shader.input("dispersion")
    }
    #[doc = "The composed `inputs:dispersion`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn dispersion(&self) -> Option<f32> {
        self.read_value("inputs:dispersion", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:dispersion` at `time`.
    #[must_use]
    pub fn dispersion_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:dispersion",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:dispersion` default in the node definition, separate from authored values.
    #[must_use]
    pub fn dispersion_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:emissive` port, if it exists. "]
    #[must_use]
    pub fn emissive_input(&self) -> Option<Port<'a>> {
        self.shader.input("emissive")
    }
    #[doc = "The composed `inputs:emissive`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn emissive(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:emissive", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:emissive` at `time`.
    #[must_use]
    pub fn emissive_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:emissive",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:emissive` default in the node definition, separate from authored values.
    #[must_use]
    pub fn emissive_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:emissive_strength` port, if it exists. "]
    #[must_use]
    pub fn emissive_strength_input(&self) -> Option<Port<'a>> {
        self.shader.input("emissive_strength")
    }
    #[doc = "The composed `inputs:emissive_strength`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn emissive_strength(&self) -> Option<f32> {
        self.read_value(
            "inputs:emissive_strength",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:emissive_strength` at `time`.
    #[must_use]
    pub fn emissive_strength_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:emissive_strength",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:emissive_strength` default in the node definition, separate from authored values.
    #[must_use]
    pub fn emissive_strength_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:ior` port, if it exists. "]
    #[must_use]
    pub fn ior_input(&self) -> Option<Port<'a>> {
        self.shader.input("ior")
    }
    #[doc = "The composed `inputs:ior`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn ior(&self) -> Option<f32> {
        self.read_value("inputs:ior", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:ior` at `time`.
    #[must_use]
    pub fn ior_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:ior", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:ior` default in the node definition, separate from authored values.
    #[must_use]
    pub fn ior_default() -> f32 {
        1.5_f32
    }
    #[doc = "The authored `inputs:iridescence` port, if it exists. "]
    #[must_use]
    pub fn iridescence_input(&self) -> Option<Port<'a>> {
        self.shader.input("iridescence")
    }
    #[doc = "The composed `inputs:iridescence`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn iridescence(&self) -> Option<f32> {
        self.read_value("inputs:iridescence", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:iridescence` at `time`.
    #[must_use]
    pub fn iridescence_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:iridescence",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:iridescence` default in the node definition, separate from authored values.
    #[must_use]
    pub fn iridescence_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:iridescence_ior` port, if it exists. "]
    #[must_use]
    pub fn iridescence_ior_input(&self) -> Option<Port<'a>> {
        self.shader.input("iridescence_ior")
    }
    #[doc = "The composed `inputs:iridescence_ior`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn iridescence_ior(&self) -> Option<f32> {
        self.read_value(
            "inputs:iridescence_ior",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:iridescence_ior` at `time`.
    #[must_use]
    pub fn iridescence_ior_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:iridescence_ior",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:iridescence_ior` default in the node definition, separate from authored values.
    #[must_use]
    pub fn iridescence_ior_default() -> f32 {
        1.3_f32
    }
    #[doc = "The authored `inputs:iridescence_thickness` port, if it exists. "]
    #[must_use]
    pub fn iridescence_thickness_input(&self) -> Option<Port<'a>> {
        self.shader.input("iridescence_thickness")
    }
    #[doc = "The composed `inputs:iridescence_thickness`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn iridescence_thickness(&self) -> Option<f32> {
        self.read_value(
            "inputs:iridescence_thickness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:iridescence_thickness` at `time`.
    #[must_use]
    pub fn iridescence_thickness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:iridescence_thickness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:iridescence_thickness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn iridescence_thickness_default() -> f32 {
        100.0_f32
    }
    #[doc = "The authored `inputs:metallic` port, if it exists. "]
    #[must_use]
    pub fn metallic_input(&self) -> Option<Port<'a>> {
        self.shader.input("metallic")
    }
    #[doc = "The composed `inputs:metallic`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn metallic(&self) -> Option<f32> {
        self.read_value("inputs:metallic", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:metallic` at `time`.
    #[must_use]
    pub fn metallic_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:metallic",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:metallic` default in the node definition, separate from authored values.
    #[must_use]
    pub fn metallic_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:normal` port, if it exists. "]
    #[must_use]
    pub fn normal_input(&self) -> Option<Port<'a>> {
        self.shader.input("normal")
    }
    #[doc = "The composed `inputs:normal`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn normal(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:normal", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:normal` at `time`.
    #[must_use]
    pub fn normal_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:normal",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:occlusion` port, if it exists. "]
    #[must_use]
    pub fn occlusion_input(&self) -> Option<Port<'a>> {
        self.shader.input("occlusion")
    }
    #[doc = "The composed `inputs:occlusion`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn occlusion(&self) -> Option<f32> {
        self.read_value("inputs:occlusion", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:occlusion` at `time`.
    #[must_use]
    pub fn occlusion_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:occlusion",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:occlusion` default in the node definition, separate from authored values.
    #[must_use]
    pub fn occlusion_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:roughness` port, if it exists. "]
    #[must_use]
    pub fn roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("roughness")
    }
    #[doc = "The composed `inputs:roughness`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn roughness(&self) -> Option<f32> {
        self.read_value("inputs:roughness", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:roughness` at `time`.
    #[must_use]
    pub fn roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn roughness_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:sheen_color` port, if it exists. "]
    #[must_use]
    pub fn sheen_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("sheen_color")
    }
    #[doc = "The composed `inputs:sheen_color`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn sheen_color(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:sheen_color", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:sheen_color` at `time`.
    #[must_use]
    pub fn sheen_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:sheen_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:sheen_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn sheen_color_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:sheen_roughness` port, if it exists. "]
    #[must_use]
    pub fn sheen_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("sheen_roughness")
    }
    #[doc = "The composed `inputs:sheen_roughness`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn sheen_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:sheen_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:sheen_roughness` at `time`.
    #[must_use]
    pub fn sheen_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:sheen_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:sheen_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn sheen_roughness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:specular` port, if it exists. "]
    #[must_use]
    pub fn specular_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular")
    }
    #[doc = "The composed `inputs:specular`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn specular(&self) -> Option<f32> {
        self.read_value("inputs:specular", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:specular` at `time`.
    #[must_use]
    pub fn specular_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:specular_color` port, if it exists. "]
    #[must_use]
    pub fn specular_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_color")
    }
    #[doc = "The composed `inputs:specular_color`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn specular_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:specular_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:specular_color` at `time`.
    #[must_use]
    pub fn specular_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:specular_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:specular_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:tangent` port, if it exists. "]
    #[must_use]
    pub fn tangent_input(&self) -> Option<Port<'a>> {
        self.shader.input("tangent")
    }
    #[doc = "The composed `inputs:tangent`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn tangent(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:tangent", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:tangent` at `time`.
    #[must_use]
    pub fn tangent_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:tangent",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:thickness` port, if it exists. "]
    #[must_use]
    pub fn thickness_input(&self) -> Option<Port<'a>> {
        self.shader.input("thickness")
    }
    #[doc = "The composed `inputs:thickness`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn thickness(&self) -> Option<f32> {
        self.read_value("inputs:thickness", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:thickness` at `time`.
    #[must_use]
    pub fn thickness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:thickness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:thickness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn thickness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission` port, if it exists. "]
    #[must_use]
    pub fn transmission_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission")
    }
    #[doc = "The composed `inputs:transmission`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn transmission(&self) -> Option<f32> {
        self.read_value("inputs:transmission", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:transmission` at `time`.
    #[must_use]
    pub fn transmission_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:surface` port, if it exists. "]
    #[must_use]
    pub fn surface_output(&self) -> Option<Port<'a>> {
        self.shader.output("surface")
    }
}

/// Authors `ND_gltf_pbr_surfaceshader` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdGltfPbrSurfaceshaderEdit {
    shader: ShaderEdit,
}
impl Deref for NdGltfPbrSurfaceshaderEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdGltfPbrSurfaceshaderEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:alpha` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_alpha_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "alpha", ty)
    }
    /// Authors `inputs:alpha` at default time, creating the typed input if needed.
    pub fn set_alpha(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_alpha_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:alpha` sample in stage time through the edit target.
    pub fn set_alpha_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_alpha_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:alpha_cutoff` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_alpha_cutoff_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "alpha_cutoff", ty)
    }
    /// Authors `inputs:alpha_cutoff` at default time, creating the typed input if needed.
    pub fn set_alpha_cutoff(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_alpha_cutoff_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:alpha_cutoff` sample in stage time through the edit target.
    pub fn set_alpha_cutoff_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_alpha_cutoff_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:alpha_mode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_alpha_mode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "alpha_mode", ty)
    }
    /// Authors `inputs:alpha_mode` at default time, creating the typed input if needed.
    pub fn set_alpha_mode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_alpha_mode_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:alpha_mode` sample in stage time through the edit target.
    pub fn set_alpha_mode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_alpha_mode_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:anisotropy_rotation` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_anisotropy_rotation_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "anisotropy_rotation", ty)
    }
    /// Authors `inputs:anisotropy_rotation` at default time, creating the typed input if needed.
    pub fn set_anisotropy_rotation(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_anisotropy_rotation_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:anisotropy_rotation` sample in stage time through the edit target.
    pub fn set_anisotropy_rotation_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_anisotropy_rotation_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:anisotropy_strength` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_anisotropy_strength_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "anisotropy_strength", ty)
    }
    /// Authors `inputs:anisotropy_strength` at default time, creating the typed input if needed.
    pub fn set_anisotropy_strength(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_anisotropy_strength_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:anisotropy_strength` sample in stage time through the edit target.
    pub fn set_anisotropy_strength_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_anisotropy_strength_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:attenuation_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_attenuation_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "attenuation_color", ty)
    }
    /// Authors `inputs:attenuation_color` at default time, creating the typed input if needed.
    pub fn set_attenuation_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_attenuation_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:attenuation_color` sample in stage time through the edit target.
    pub fn set_attenuation_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_attenuation_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:attenuation_distance` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_attenuation_distance_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "attenuation_distance", ty)
    }
    /// Authors `inputs:attenuation_distance` at default time, creating the typed input if needed.
    pub fn set_attenuation_distance(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_attenuation_distance_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:attenuation_distance` sample in stage time through the edit target.
    pub fn set_attenuation_distance_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_attenuation_distance_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:base_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_base_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "base_color", ty)
    }
    /// Authors `inputs:base_color` at default time, creating the typed input if needed.
    pub fn set_base_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_base_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:base_color` sample in stage time through the edit target.
    pub fn set_base_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_base_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:clearcoat` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_clearcoat_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "clearcoat", ty)
    }
    /// Authors `inputs:clearcoat` at default time, creating the typed input if needed.
    pub fn set_clearcoat(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_clearcoat_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:clearcoat` sample in stage time through the edit target.
    pub fn set_clearcoat_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_clearcoat_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:clearcoat_normal` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_clearcoat_normal_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "clearcoat_normal", ty)
    }
    /// Authors `inputs:clearcoat_normal` at default time, creating the typed input if needed.
    pub fn set_clearcoat_normal(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_clearcoat_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:clearcoat_normal` sample in stage time through the edit target.
    pub fn set_clearcoat_normal_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_clearcoat_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:clearcoat_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_clearcoat_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "clearcoat_roughness", ty)
    }
    /// Authors `inputs:clearcoat_roughness` at default time, creating the typed input if needed.
    pub fn set_clearcoat_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_clearcoat_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:clearcoat_roughness` sample in stage time through the edit target.
    pub fn set_clearcoat_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_clearcoat_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:dispersion` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_dispersion_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "dispersion", ty)
    }
    /// Authors `inputs:dispersion` at default time, creating the typed input if needed.
    pub fn set_dispersion(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_dispersion_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:dispersion` sample in stage time through the edit target.
    pub fn set_dispersion_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_dispersion_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:emissive` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_emissive_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "emissive", ty)
    }
    /// Authors `inputs:emissive` at default time, creating the typed input if needed.
    pub fn set_emissive(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_emissive_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:emissive` sample in stage time through the edit target.
    pub fn set_emissive_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_emissive_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:emissive_strength` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_emissive_strength_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "emissive_strength", ty)
    }
    /// Authors `inputs:emissive_strength` at default time, creating the typed input if needed.
    pub fn set_emissive_strength(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_emissive_strength_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:emissive_strength` sample in stage time through the edit target.
    pub fn set_emissive_strength_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_emissive_strength_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:ior` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_ior_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "ior", ty)
    }
    /// Authors `inputs:ior` at default time, creating the typed input if needed.
    pub fn set_ior(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:ior` sample in stage time through the edit target.
    pub fn set_ior_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:iridescence` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_iridescence_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "iridescence", ty)
    }
    /// Authors `inputs:iridescence` at default time, creating the typed input if needed.
    pub fn set_iridescence(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_iridescence_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:iridescence` sample in stage time through the edit target.
    pub fn set_iridescence_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_iridescence_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:iridescence_ior` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_iridescence_ior_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "iridescence_ior", ty)
    }
    /// Authors `inputs:iridescence_ior` at default time, creating the typed input if needed.
    pub fn set_iridescence_ior(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_iridescence_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:iridescence_ior` sample in stage time through the edit target.
    pub fn set_iridescence_ior_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_iridescence_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:iridescence_thickness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_iridescence_thickness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "iridescence_thickness", ty)
    }
    /// Authors `inputs:iridescence_thickness` at default time, creating the typed input if needed.
    pub fn set_iridescence_thickness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_iridescence_thickness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:iridescence_thickness` sample in stage time through the edit target.
    pub fn set_iridescence_thickness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_iridescence_thickness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:metallic` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_metallic_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "metallic", ty)
    }
    /// Authors `inputs:metallic` at default time, creating the typed input if needed.
    pub fn set_metallic(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_metallic_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:metallic` sample in stage time through the edit target.
    pub fn set_metallic_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_metallic_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:normal` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_normal_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "normal", ty)
    }
    /// Authors `inputs:normal` at default time, creating the typed input if needed.
    pub fn set_normal(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:normal` sample in stage time through the edit target.
    pub fn set_normal_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:occlusion` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_occlusion_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "occlusion", ty)
    }
    /// Authors `inputs:occlusion` at default time, creating the typed input if needed.
    pub fn set_occlusion(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_occlusion_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:occlusion` sample in stage time through the edit target.
    pub fn set_occlusion_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_occlusion_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_roughness_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "roughness", ty)
    }
    /// Authors `inputs:roughness` at default time, creating the typed input if needed.
    pub fn set_roughness(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:roughness` sample in stage time through the edit target.
    pub fn set_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:sheen_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_sheen_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "sheen_color", ty)
    }
    /// Authors `inputs:sheen_color` at default time, creating the typed input if needed.
    pub fn set_sheen_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:sheen_color` sample in stage time through the edit target.
    pub fn set_sheen_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:sheen_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_sheen_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "sheen_roughness", ty)
    }
    /// Authors `inputs:sheen_roughness` at default time, creating the typed input if needed.
    pub fn set_sheen_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:sheen_roughness` sample in stage time through the edit target.
    pub fn set_sheen_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular", ty)
    }
    /// Authors `inputs:specular` at default time, creating the typed input if needed.
    pub fn set_specular(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_specular_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular` sample in stage time through the edit target.
    pub fn set_specular_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "specular_color", ty)
    }
    /// Authors `inputs:specular_color` at default time, creating the typed input if needed.
    pub fn set_specular_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_color` sample in stage time through the edit target.
    pub fn set_specular_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:tangent` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_tangent_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "tangent", ty)
    }
    /// Authors `inputs:tangent` at default time, creating the typed input if needed.
    pub fn set_tangent(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:tangent` sample in stage time through the edit target.
    pub fn set_tangent_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:thickness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_thickness_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "thickness", ty)
    }
    /// Authors `inputs:thickness` at default time, creating the typed input if needed.
    pub fn set_thickness(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_thickness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:thickness` sample in stage time through the edit target.
    pub fn set_thickness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thickness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "transmission", ty)
    }
    /// Authors `inputs:transmission` at default time, creating the typed input if needed.
    pub fn set_transmission(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission` sample in stage time through the edit target.
    pub fn set_transmission_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:surface` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_surface_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("token").expect("a registered value type");
        self.shader.create_output(edit, "surface", ty)
    }
}

/// The `ND_image_color3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdImageColor3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdImageColor3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdImageColor3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_image_color3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdImageColor3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdImageColor3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "default",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:default",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "file",
                direction: PortKind::Input,
                type_name: "filename",
                usd_name: "inputs:file",
                usd_type_name: "asset",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "filtertype",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:filtertype",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "linear",
                )),
            },
            MaterialXPort {
                name: "frameendaction",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:frameendaction",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "constant",
                )),
            },
            MaterialXPort {
                name: "frameoffset",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:frameoffset",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "framerange",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:framerange",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "layer",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:layer",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "texcoord",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:texcoord",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "UV0",
                )),
            },
            MaterialXPort {
                name: "uaddressmode",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:uaddressmode",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "periodic",
                )),
            },
            MaterialXPort {
                name: "vaddressmode",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:vaddressmode",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "periodic",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color3",
                usd_name: "outputs:out",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:default` port, if it exists. "]
    #[must_use]
    pub fn default_input(&self) -> Option<Port<'a>> {
        self.shader.input("default")
    }
    #[doc = "The composed `inputs:default`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn default(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:default", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:default` at `time`.
    #[must_use]
    pub fn default_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:default",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:default` default in the node definition, separate from authored values.
    #[must_use]
    pub fn default_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:file` port, if it exists. "]
    #[must_use]
    pub fn file_input(&self) -> Option<Port<'a>> {
        self.shader.input("file")
    }
    #[doc = "The composed `inputs:file`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn file(&self) -> Option<Arc<str>> {
        self.read_value("inputs:file", layerstack_schemas::value::read_asset)
    }
    /// The composed `inputs:file` at `time`.
    #[must_use]
    pub fn file_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:file", time, layerstack_schemas::value::read_asset)
    }
    /// The `inputs:file` default in the node definition, separate from authored values.
    #[must_use]
    pub fn file_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:filtertype` port, if it exists. "]
    #[must_use]
    pub fn filtertype_input(&self) -> Option<Port<'a>> {
        self.shader.input("filtertype")
    }
    #[doc = "The composed `inputs:filtertype`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn filtertype(&self) -> Option<Arc<str>> {
        self.read_value("inputs:filtertype", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:filtertype` at `time`.
    #[must_use]
    pub fn filtertype_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:filtertype",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:filtertype` default in the node definition, separate from authored values.
    #[must_use]
    pub fn filtertype_default() -> Arc<str> {
        Arc::from("linear")
    }
    #[doc = "The authored `inputs:frameendaction` port, if it exists. "]
    #[must_use]
    pub fn frameendaction_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameendaction")
    }
    #[doc = "The composed `inputs:frameendaction`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameendaction(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:frameendaction",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:frameendaction` at `time`.
    #[must_use]
    pub fn frameendaction_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:frameendaction",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:frameendaction` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameendaction_default() -> Arc<str> {
        Arc::from("constant")
    }
    #[doc = "The authored `inputs:frameoffset` port, if it exists. "]
    #[must_use]
    pub fn frameoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameoffset")
    }
    #[doc = "The composed `inputs:frameoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameoffset(&self) -> Option<i32> {
        self.read_value("inputs:frameoffset", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:frameoffset` at `time`.
    #[must_use]
    pub fn frameoffset_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:frameoffset",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:frameoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameoffset_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:framerange` port, if it exists. "]
    #[must_use]
    pub fn framerange_input(&self) -> Option<Port<'a>> {
        self.shader.input("framerange")
    }
    #[doc = "The composed `inputs:framerange`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn framerange(&self) -> Option<Arc<str>> {
        self.read_value("inputs:framerange", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:framerange` at `time`.
    #[must_use]
    pub fn framerange_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:framerange",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:framerange` default in the node definition, separate from authored values.
    #[must_use]
    pub fn framerange_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:layer` port, if it exists. "]
    #[must_use]
    pub fn layer_input(&self) -> Option<Port<'a>> {
        self.shader.input("layer")
    }
    #[doc = "The composed `inputs:layer`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn layer(&self) -> Option<Arc<str>> {
        self.read_value("inputs:layer", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:layer` at `time`.
    #[must_use]
    pub fn layer_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:layer", time, layerstack_schemas::value::read_string)
    }
    /// The `inputs:layer` default in the node definition, separate from authored values.
    #[must_use]
    pub fn layer_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:texcoord` port, if it exists. "]
    #[must_use]
    pub fn texcoord_input(&self) -> Option<Port<'a>> {
        self.shader.input("texcoord")
    }
    #[doc = "The composed `inputs:texcoord`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn texcoord(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:texcoord", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:texcoord` at `time`.
    #[must_use]
    pub fn texcoord_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:texcoord",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    #[doc = "The authored `inputs:uaddressmode` port, if it exists. "]
    #[must_use]
    pub fn uaddressmode_input(&self) -> Option<Port<'a>> {
        self.shader.input("uaddressmode")
    }
    #[doc = "The composed `inputs:uaddressmode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uaddressmode(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:uaddressmode",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:uaddressmode` at `time`.
    #[must_use]
    pub fn uaddressmode_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:uaddressmode",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:uaddressmode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uaddressmode_default() -> Arc<str> {
        Arc::from("periodic")
    }
    #[doc = "The authored `inputs:vaddressmode` port, if it exists. "]
    #[must_use]
    pub fn vaddressmode_input(&self) -> Option<Port<'a>> {
        self.shader.input("vaddressmode")
    }
    #[doc = "The composed `inputs:vaddressmode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn vaddressmode(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:vaddressmode",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:vaddressmode` at `time`.
    #[must_use]
    pub fn vaddressmode_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:vaddressmode",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:vaddressmode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn vaddressmode_default() -> Arc<str> {
        Arc::from("periodic")
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_image_color3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdImageColor3Edit {
    shader: ShaderEdit,
}
impl Deref for NdImageColor3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdImageColor3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:default` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_default_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "default", ty)
    }
    /// Authors `inputs:default` at default time, creating the typed input if needed.
    pub fn set_default(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:default` sample in stage time through the edit target.
    pub fn set_default_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:file` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_file_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("asset").expect("a registered value type");
        self.shader.create_input(edit, "file", ty)
    }
    /// Authors `inputs:file` at default time, creating the typed input if needed.
    pub fn set_file(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:file` sample in stage time through the edit target.
    pub fn set_file_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:filtertype` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_filtertype_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "filtertype", ty)
    }
    /// Authors `inputs:filtertype` at default time, creating the typed input if needed.
    pub fn set_filtertype(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:filtertype` sample in stage time through the edit target.
    pub fn set_filtertype_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameendaction` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameendaction_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "frameendaction", ty)
    }
    /// Authors `inputs:frameendaction` at default time, creating the typed input if needed.
    pub fn set_frameendaction(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameendaction` sample in stage time through the edit target.
    pub fn set_frameendaction_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameoffset_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "frameoffset", ty)
    }
    /// Authors `inputs:frameoffset` at default time, creating the typed input if needed.
    pub fn set_frameoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameoffset` sample in stage time through the edit target.
    pub fn set_frameoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:framerange` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_framerange_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "framerange", ty)
    }
    /// Authors `inputs:framerange` at default time, creating the typed input if needed.
    pub fn set_framerange(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:framerange` sample in stage time through the edit target.
    pub fn set_framerange_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:layer` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_layer_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "layer", ty)
    }
    /// Authors `inputs:layer` at default time, creating the typed input if needed.
    pub fn set_layer(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_layer_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:layer` sample in stage time through the edit target.
    pub fn set_layer_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_layer_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:texcoord` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_texcoord_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "texcoord", ty)
    }
    /// Authors `inputs:texcoord` at default time, creating the typed input if needed.
    pub fn set_texcoord(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:texcoord` sample in stage time through the edit target.
    pub fn set_texcoord_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uaddressmode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uaddressmode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "uaddressmode", ty)
    }
    /// Authors `inputs:uaddressmode` at default time, creating the typed input if needed.
    pub fn set_uaddressmode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_uaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uaddressmode` sample in stage time through the edit target.
    pub fn set_uaddressmode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_uaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:vaddressmode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_vaddressmode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "vaddressmode", ty)
    }
    /// Authors `inputs:vaddressmode` at default time, creating the typed input if needed.
    pub fn set_vaddressmode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_vaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:vaddressmode` sample in stage time through the edit target.
    pub fn set_vaddressmode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_vaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_image_color4` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdImageColor4<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdImageColor4<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdImageColor4<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_image_color4";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdImageColor4Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdImageColor4Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "default",
                direction: PortKind::Input,
                type_name: "color4",
                usd_name: "inputs:default",
                usd_type_name: "color4f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "file",
                direction: PortKind::Input,
                type_name: "filename",
                usd_name: "inputs:file",
                usd_type_name: "asset",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "filtertype",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:filtertype",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "linear",
                )),
            },
            MaterialXPort {
                name: "frameendaction",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:frameendaction",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "constant",
                )),
            },
            MaterialXPort {
                name: "frameoffset",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:frameoffset",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "framerange",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:framerange",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "layer",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:layer",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "texcoord",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:texcoord",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "UV0",
                )),
            },
            MaterialXPort {
                name: "uaddressmode",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:uaddressmode",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "periodic",
                )),
            },
            MaterialXPort {
                name: "vaddressmode",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:vaddressmode",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "periodic",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color4",
                usd_name: "outputs:out",
                usd_type_name: "color4f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0, 0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:default` port, if it exists. "]
    #[must_use]
    pub fn default_input(&self) -> Option<Port<'a>> {
        self.shader.input("default")
    }
    #[doc = "The composed `inputs:default`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn default(&self) -> Option<[f32; 4]> {
        self.read_value("inputs:default", layerstack_schemas::value::read_float4)
    }
    /// The composed `inputs:default` at `time`.
    #[must_use]
    pub fn default_at(&self, time: layerstack::Time) -> Option<[f32; 4]> {
        self.read_value_at(
            "inputs:default",
            time,
            layerstack_schemas::value::read_float4,
        )
    }
    /// The `inputs:default` default in the node definition, separate from authored values.
    #[must_use]
    pub fn default_default() -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:file` port, if it exists. "]
    #[must_use]
    pub fn file_input(&self) -> Option<Port<'a>> {
        self.shader.input("file")
    }
    #[doc = "The composed `inputs:file`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn file(&self) -> Option<Arc<str>> {
        self.read_value("inputs:file", layerstack_schemas::value::read_asset)
    }
    /// The composed `inputs:file` at `time`.
    #[must_use]
    pub fn file_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:file", time, layerstack_schemas::value::read_asset)
    }
    /// The `inputs:file` default in the node definition, separate from authored values.
    #[must_use]
    pub fn file_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:filtertype` port, if it exists. "]
    #[must_use]
    pub fn filtertype_input(&self) -> Option<Port<'a>> {
        self.shader.input("filtertype")
    }
    #[doc = "The composed `inputs:filtertype`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn filtertype(&self) -> Option<Arc<str>> {
        self.read_value("inputs:filtertype", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:filtertype` at `time`.
    #[must_use]
    pub fn filtertype_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:filtertype",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:filtertype` default in the node definition, separate from authored values.
    #[must_use]
    pub fn filtertype_default() -> Arc<str> {
        Arc::from("linear")
    }
    #[doc = "The authored `inputs:frameendaction` port, if it exists. "]
    #[must_use]
    pub fn frameendaction_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameendaction")
    }
    #[doc = "The composed `inputs:frameendaction`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameendaction(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:frameendaction",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:frameendaction` at `time`.
    #[must_use]
    pub fn frameendaction_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:frameendaction",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:frameendaction` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameendaction_default() -> Arc<str> {
        Arc::from("constant")
    }
    #[doc = "The authored `inputs:frameoffset` port, if it exists. "]
    #[must_use]
    pub fn frameoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameoffset")
    }
    #[doc = "The composed `inputs:frameoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameoffset(&self) -> Option<i32> {
        self.read_value("inputs:frameoffset", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:frameoffset` at `time`.
    #[must_use]
    pub fn frameoffset_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:frameoffset",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:frameoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameoffset_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:framerange` port, if it exists. "]
    #[must_use]
    pub fn framerange_input(&self) -> Option<Port<'a>> {
        self.shader.input("framerange")
    }
    #[doc = "The composed `inputs:framerange`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn framerange(&self) -> Option<Arc<str>> {
        self.read_value("inputs:framerange", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:framerange` at `time`.
    #[must_use]
    pub fn framerange_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:framerange",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:framerange` default in the node definition, separate from authored values.
    #[must_use]
    pub fn framerange_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:layer` port, if it exists. "]
    #[must_use]
    pub fn layer_input(&self) -> Option<Port<'a>> {
        self.shader.input("layer")
    }
    #[doc = "The composed `inputs:layer`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn layer(&self) -> Option<Arc<str>> {
        self.read_value("inputs:layer", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:layer` at `time`.
    #[must_use]
    pub fn layer_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:layer", time, layerstack_schemas::value::read_string)
    }
    /// The `inputs:layer` default in the node definition, separate from authored values.
    #[must_use]
    pub fn layer_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:texcoord` port, if it exists. "]
    #[must_use]
    pub fn texcoord_input(&self) -> Option<Port<'a>> {
        self.shader.input("texcoord")
    }
    #[doc = "The composed `inputs:texcoord`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn texcoord(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:texcoord", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:texcoord` at `time`.
    #[must_use]
    pub fn texcoord_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:texcoord",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    #[doc = "The authored `inputs:uaddressmode` port, if it exists. "]
    #[must_use]
    pub fn uaddressmode_input(&self) -> Option<Port<'a>> {
        self.shader.input("uaddressmode")
    }
    #[doc = "The composed `inputs:uaddressmode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uaddressmode(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:uaddressmode",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:uaddressmode` at `time`.
    #[must_use]
    pub fn uaddressmode_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:uaddressmode",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:uaddressmode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uaddressmode_default() -> Arc<str> {
        Arc::from("periodic")
    }
    #[doc = "The authored `inputs:vaddressmode` port, if it exists. "]
    #[must_use]
    pub fn vaddressmode_input(&self) -> Option<Port<'a>> {
        self.shader.input("vaddressmode")
    }
    #[doc = "The composed `inputs:vaddressmode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn vaddressmode(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:vaddressmode",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:vaddressmode` at `time`.
    #[must_use]
    pub fn vaddressmode_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:vaddressmode",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:vaddressmode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn vaddressmode_default() -> Arc<str> {
        Arc::from("periodic")
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_image_color4` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdImageColor4Edit {
    shader: ShaderEdit,
}
impl Deref for NdImageColor4Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdImageColor4Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:default` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_default_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color4f").expect("a registered value type");
        self.shader.create_input(edit, "default", ty)
    }
    /// Authors `inputs:default` at default time, creating the typed input if needed.
    pub fn set_default(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 4],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float4)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:default` sample in stage time through the edit target.
    pub fn set_default_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 4],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float4)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:file` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_file_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("asset").expect("a registered value type");
        self.shader.create_input(edit, "file", ty)
    }
    /// Authors `inputs:file` at default time, creating the typed input if needed.
    pub fn set_file(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:file` sample in stage time through the edit target.
    pub fn set_file_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:filtertype` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_filtertype_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "filtertype", ty)
    }
    /// Authors `inputs:filtertype` at default time, creating the typed input if needed.
    pub fn set_filtertype(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:filtertype` sample in stage time through the edit target.
    pub fn set_filtertype_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameendaction` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameendaction_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "frameendaction", ty)
    }
    /// Authors `inputs:frameendaction` at default time, creating the typed input if needed.
    pub fn set_frameendaction(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameendaction` sample in stage time through the edit target.
    pub fn set_frameendaction_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameoffset_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "frameoffset", ty)
    }
    /// Authors `inputs:frameoffset` at default time, creating the typed input if needed.
    pub fn set_frameoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameoffset` sample in stage time through the edit target.
    pub fn set_frameoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:framerange` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_framerange_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "framerange", ty)
    }
    /// Authors `inputs:framerange` at default time, creating the typed input if needed.
    pub fn set_framerange(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:framerange` sample in stage time through the edit target.
    pub fn set_framerange_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:layer` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_layer_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "layer", ty)
    }
    /// Authors `inputs:layer` at default time, creating the typed input if needed.
    pub fn set_layer(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_layer_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:layer` sample in stage time through the edit target.
    pub fn set_layer_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_layer_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:texcoord` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_texcoord_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "texcoord", ty)
    }
    /// Authors `inputs:texcoord` at default time, creating the typed input if needed.
    pub fn set_texcoord(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:texcoord` sample in stage time through the edit target.
    pub fn set_texcoord_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uaddressmode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uaddressmode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "uaddressmode", ty)
    }
    /// Authors `inputs:uaddressmode` at default time, creating the typed input if needed.
    pub fn set_uaddressmode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_uaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uaddressmode` sample in stage time through the edit target.
    pub fn set_uaddressmode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_uaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:vaddressmode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_vaddressmode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "vaddressmode", ty)
    }
    /// Authors `inputs:vaddressmode` at default time, creating the typed input if needed.
    pub fn set_vaddressmode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_vaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:vaddressmode` sample in stage time through the edit target.
    pub fn set_vaddressmode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_vaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color4f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_image_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdImageFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdImageFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdImageFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_image_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<NdImageFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdImageFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "default",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:default",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "file",
                direction: PortKind::Input,
                type_name: "filename",
                usd_name: "inputs:file",
                usd_type_name: "asset",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "filtertype",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:filtertype",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "linear",
                )),
            },
            MaterialXPort {
                name: "frameendaction",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:frameendaction",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "constant",
                )),
            },
            MaterialXPort {
                name: "frameoffset",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:frameoffset",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "framerange",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:framerange",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "layer",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:layer",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "texcoord",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:texcoord",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "UV0",
                )),
            },
            MaterialXPort {
                name: "uaddressmode",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:uaddressmode",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "periodic",
                )),
            },
            MaterialXPort {
                name: "vaddressmode",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:vaddressmode",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "periodic",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:default` port, if it exists. "]
    #[must_use]
    pub fn default_input(&self) -> Option<Port<'a>> {
        self.shader.input("default")
    }
    #[doc = "The composed `inputs:default`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn default(&self) -> Option<f32> {
        self.read_value("inputs:default", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:default` at `time`.
    #[must_use]
    pub fn default_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:default",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:default` default in the node definition, separate from authored values.
    #[must_use]
    pub fn default_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:file` port, if it exists. "]
    #[must_use]
    pub fn file_input(&self) -> Option<Port<'a>> {
        self.shader.input("file")
    }
    #[doc = "The composed `inputs:file`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn file(&self) -> Option<Arc<str>> {
        self.read_value("inputs:file", layerstack_schemas::value::read_asset)
    }
    /// The composed `inputs:file` at `time`.
    #[must_use]
    pub fn file_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:file", time, layerstack_schemas::value::read_asset)
    }
    /// The `inputs:file` default in the node definition, separate from authored values.
    #[must_use]
    pub fn file_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:filtertype` port, if it exists. "]
    #[must_use]
    pub fn filtertype_input(&self) -> Option<Port<'a>> {
        self.shader.input("filtertype")
    }
    #[doc = "The composed `inputs:filtertype`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn filtertype(&self) -> Option<Arc<str>> {
        self.read_value("inputs:filtertype", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:filtertype` at `time`.
    #[must_use]
    pub fn filtertype_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:filtertype",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:filtertype` default in the node definition, separate from authored values.
    #[must_use]
    pub fn filtertype_default() -> Arc<str> {
        Arc::from("linear")
    }
    #[doc = "The authored `inputs:frameendaction` port, if it exists. "]
    #[must_use]
    pub fn frameendaction_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameendaction")
    }
    #[doc = "The composed `inputs:frameendaction`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameendaction(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:frameendaction",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:frameendaction` at `time`.
    #[must_use]
    pub fn frameendaction_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:frameendaction",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:frameendaction` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameendaction_default() -> Arc<str> {
        Arc::from("constant")
    }
    #[doc = "The authored `inputs:frameoffset` port, if it exists. "]
    #[must_use]
    pub fn frameoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameoffset")
    }
    #[doc = "The composed `inputs:frameoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameoffset(&self) -> Option<i32> {
        self.read_value("inputs:frameoffset", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:frameoffset` at `time`.
    #[must_use]
    pub fn frameoffset_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:frameoffset",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:frameoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameoffset_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:framerange` port, if it exists. "]
    #[must_use]
    pub fn framerange_input(&self) -> Option<Port<'a>> {
        self.shader.input("framerange")
    }
    #[doc = "The composed `inputs:framerange`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn framerange(&self) -> Option<Arc<str>> {
        self.read_value("inputs:framerange", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:framerange` at `time`.
    #[must_use]
    pub fn framerange_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:framerange",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:framerange` default in the node definition, separate from authored values.
    #[must_use]
    pub fn framerange_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:layer` port, if it exists. "]
    #[must_use]
    pub fn layer_input(&self) -> Option<Port<'a>> {
        self.shader.input("layer")
    }
    #[doc = "The composed `inputs:layer`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn layer(&self) -> Option<Arc<str>> {
        self.read_value("inputs:layer", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:layer` at `time`.
    #[must_use]
    pub fn layer_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:layer", time, layerstack_schemas::value::read_string)
    }
    /// The `inputs:layer` default in the node definition, separate from authored values.
    #[must_use]
    pub fn layer_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:texcoord` port, if it exists. "]
    #[must_use]
    pub fn texcoord_input(&self) -> Option<Port<'a>> {
        self.shader.input("texcoord")
    }
    #[doc = "The composed `inputs:texcoord`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn texcoord(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:texcoord", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:texcoord` at `time`.
    #[must_use]
    pub fn texcoord_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:texcoord",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    #[doc = "The authored `inputs:uaddressmode` port, if it exists. "]
    #[must_use]
    pub fn uaddressmode_input(&self) -> Option<Port<'a>> {
        self.shader.input("uaddressmode")
    }
    #[doc = "The composed `inputs:uaddressmode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uaddressmode(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:uaddressmode",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:uaddressmode` at `time`.
    #[must_use]
    pub fn uaddressmode_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:uaddressmode",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:uaddressmode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uaddressmode_default() -> Arc<str> {
        Arc::from("periodic")
    }
    #[doc = "The authored `inputs:vaddressmode` port, if it exists. "]
    #[must_use]
    pub fn vaddressmode_input(&self) -> Option<Port<'a>> {
        self.shader.input("vaddressmode")
    }
    #[doc = "The composed `inputs:vaddressmode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn vaddressmode(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:vaddressmode",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:vaddressmode` at `time`.
    #[must_use]
    pub fn vaddressmode_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:vaddressmode",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:vaddressmode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn vaddressmode_default() -> Arc<str> {
        Arc::from("periodic")
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_image_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdImageFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdImageFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdImageFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:default` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_default_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "default", ty)
    }
    /// Authors `inputs:default` at default time, creating the typed input if needed.
    pub fn set_default(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:default` sample in stage time through the edit target.
    pub fn set_default_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:file` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_file_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("asset").expect("a registered value type");
        self.shader.create_input(edit, "file", ty)
    }
    /// Authors `inputs:file` at default time, creating the typed input if needed.
    pub fn set_file(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:file` sample in stage time through the edit target.
    pub fn set_file_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:filtertype` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_filtertype_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "filtertype", ty)
    }
    /// Authors `inputs:filtertype` at default time, creating the typed input if needed.
    pub fn set_filtertype(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:filtertype` sample in stage time through the edit target.
    pub fn set_filtertype_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameendaction` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameendaction_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "frameendaction", ty)
    }
    /// Authors `inputs:frameendaction` at default time, creating the typed input if needed.
    pub fn set_frameendaction(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameendaction` sample in stage time through the edit target.
    pub fn set_frameendaction_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameoffset_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "frameoffset", ty)
    }
    /// Authors `inputs:frameoffset` at default time, creating the typed input if needed.
    pub fn set_frameoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameoffset` sample in stage time through the edit target.
    pub fn set_frameoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:framerange` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_framerange_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "framerange", ty)
    }
    /// Authors `inputs:framerange` at default time, creating the typed input if needed.
    pub fn set_framerange(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:framerange` sample in stage time through the edit target.
    pub fn set_framerange_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:layer` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_layer_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "layer", ty)
    }
    /// Authors `inputs:layer` at default time, creating the typed input if needed.
    pub fn set_layer(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_layer_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:layer` sample in stage time through the edit target.
    pub fn set_layer_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_layer_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:texcoord` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_texcoord_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "texcoord", ty)
    }
    /// Authors `inputs:texcoord` at default time, creating the typed input if needed.
    pub fn set_texcoord(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:texcoord` sample in stage time through the edit target.
    pub fn set_texcoord_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uaddressmode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uaddressmode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "uaddressmode", ty)
    }
    /// Authors `inputs:uaddressmode` at default time, creating the typed input if needed.
    pub fn set_uaddressmode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_uaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uaddressmode` sample in stage time through the edit target.
    pub fn set_uaddressmode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_uaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:vaddressmode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_vaddressmode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "vaddressmode", ty)
    }
    /// Authors `inputs:vaddressmode` at default time, creating the typed input if needed.
    pub fn set_vaddressmode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_vaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:vaddressmode` sample in stage time through the edit target.
    pub fn set_vaddressmode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_vaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_image_vector3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdImageVector3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdImageVector3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdImageVector3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_image_vector3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdImageVector3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdImageVector3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "default",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:default",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "file",
                direction: PortKind::Input,
                type_name: "filename",
                usd_name: "inputs:file",
                usd_type_name: "asset",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "filtertype",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:filtertype",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "linear",
                )),
            },
            MaterialXPort {
                name: "frameendaction",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:frameendaction",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "constant",
                )),
            },
            MaterialXPort {
                name: "frameoffset",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:frameoffset",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "framerange",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:framerange",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "layer",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:layer",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "texcoord",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:texcoord",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "UV0",
                )),
            },
            MaterialXPort {
                name: "uaddressmode",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:uaddressmode",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "periodic",
                )),
            },
            MaterialXPort {
                name: "vaddressmode",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:vaddressmode",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "periodic",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "vector3",
                usd_name: "outputs:out",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:default` port, if it exists. "]
    #[must_use]
    pub fn default_input(&self) -> Option<Port<'a>> {
        self.shader.input("default")
    }
    #[doc = "The composed `inputs:default`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn default(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:default", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:default` at `time`.
    #[must_use]
    pub fn default_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:default",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:default` default in the node definition, separate from authored values.
    #[must_use]
    pub fn default_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:file` port, if it exists. "]
    #[must_use]
    pub fn file_input(&self) -> Option<Port<'a>> {
        self.shader.input("file")
    }
    #[doc = "The composed `inputs:file`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn file(&self) -> Option<Arc<str>> {
        self.read_value("inputs:file", layerstack_schemas::value::read_asset)
    }
    /// The composed `inputs:file` at `time`.
    #[must_use]
    pub fn file_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:file", time, layerstack_schemas::value::read_asset)
    }
    /// The `inputs:file` default in the node definition, separate from authored values.
    #[must_use]
    pub fn file_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:filtertype` port, if it exists. "]
    #[must_use]
    pub fn filtertype_input(&self) -> Option<Port<'a>> {
        self.shader.input("filtertype")
    }
    #[doc = "The composed `inputs:filtertype`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn filtertype(&self) -> Option<Arc<str>> {
        self.read_value("inputs:filtertype", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:filtertype` at `time`.
    #[must_use]
    pub fn filtertype_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:filtertype",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:filtertype` default in the node definition, separate from authored values.
    #[must_use]
    pub fn filtertype_default() -> Arc<str> {
        Arc::from("linear")
    }
    #[doc = "The authored `inputs:frameendaction` port, if it exists. "]
    #[must_use]
    pub fn frameendaction_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameendaction")
    }
    #[doc = "The composed `inputs:frameendaction`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameendaction(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:frameendaction",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:frameendaction` at `time`.
    #[must_use]
    pub fn frameendaction_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:frameendaction",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:frameendaction` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameendaction_default() -> Arc<str> {
        Arc::from("constant")
    }
    #[doc = "The authored `inputs:frameoffset` port, if it exists. "]
    #[must_use]
    pub fn frameoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameoffset")
    }
    #[doc = "The composed `inputs:frameoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameoffset(&self) -> Option<i32> {
        self.read_value("inputs:frameoffset", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:frameoffset` at `time`.
    #[must_use]
    pub fn frameoffset_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:frameoffset",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:frameoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameoffset_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:framerange` port, if it exists. "]
    #[must_use]
    pub fn framerange_input(&self) -> Option<Port<'a>> {
        self.shader.input("framerange")
    }
    #[doc = "The composed `inputs:framerange`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn framerange(&self) -> Option<Arc<str>> {
        self.read_value("inputs:framerange", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:framerange` at `time`.
    #[must_use]
    pub fn framerange_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:framerange",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:framerange` default in the node definition, separate from authored values.
    #[must_use]
    pub fn framerange_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:layer` port, if it exists. "]
    #[must_use]
    pub fn layer_input(&self) -> Option<Port<'a>> {
        self.shader.input("layer")
    }
    #[doc = "The composed `inputs:layer`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn layer(&self) -> Option<Arc<str>> {
        self.read_value("inputs:layer", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:layer` at `time`.
    #[must_use]
    pub fn layer_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:layer", time, layerstack_schemas::value::read_string)
    }
    /// The `inputs:layer` default in the node definition, separate from authored values.
    #[must_use]
    pub fn layer_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:texcoord` port, if it exists. "]
    #[must_use]
    pub fn texcoord_input(&self) -> Option<Port<'a>> {
        self.shader.input("texcoord")
    }
    #[doc = "The composed `inputs:texcoord`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn texcoord(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:texcoord", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:texcoord` at `time`.
    #[must_use]
    pub fn texcoord_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:texcoord",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    #[doc = "The authored `inputs:uaddressmode` port, if it exists. "]
    #[must_use]
    pub fn uaddressmode_input(&self) -> Option<Port<'a>> {
        self.shader.input("uaddressmode")
    }
    #[doc = "The composed `inputs:uaddressmode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uaddressmode(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:uaddressmode",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:uaddressmode` at `time`.
    #[must_use]
    pub fn uaddressmode_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:uaddressmode",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:uaddressmode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uaddressmode_default() -> Arc<str> {
        Arc::from("periodic")
    }
    #[doc = "The authored `inputs:vaddressmode` port, if it exists. "]
    #[must_use]
    pub fn vaddressmode_input(&self) -> Option<Port<'a>> {
        self.shader.input("vaddressmode")
    }
    #[doc = "The composed `inputs:vaddressmode`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn vaddressmode(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:vaddressmode",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:vaddressmode` at `time`.
    #[must_use]
    pub fn vaddressmode_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:vaddressmode",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:vaddressmode` default in the node definition, separate from authored values.
    #[must_use]
    pub fn vaddressmode_default() -> Arc<str> {
        Arc::from("periodic")
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_image_vector3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdImageVector3Edit {
    shader: ShaderEdit,
}
impl Deref for NdImageVector3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdImageVector3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:default` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_default_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "default", ty)
    }
    /// Authors `inputs:default` at default time, creating the typed input if needed.
    pub fn set_default(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:default` sample in stage time through the edit target.
    pub fn set_default_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:file` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_file_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("asset").expect("a registered value type");
        self.shader.create_input(edit, "file", ty)
    }
    /// Authors `inputs:file` at default time, creating the typed input if needed.
    pub fn set_file(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:file` sample in stage time through the edit target.
    pub fn set_file_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:filtertype` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_filtertype_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "filtertype", ty)
    }
    /// Authors `inputs:filtertype` at default time, creating the typed input if needed.
    pub fn set_filtertype(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:filtertype` sample in stage time through the edit target.
    pub fn set_filtertype_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameendaction` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameendaction_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "frameendaction", ty)
    }
    /// Authors `inputs:frameendaction` at default time, creating the typed input if needed.
    pub fn set_frameendaction(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameendaction` sample in stage time through the edit target.
    pub fn set_frameendaction_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameoffset_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "frameoffset", ty)
    }
    /// Authors `inputs:frameoffset` at default time, creating the typed input if needed.
    pub fn set_frameoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameoffset` sample in stage time through the edit target.
    pub fn set_frameoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:framerange` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_framerange_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "framerange", ty)
    }
    /// Authors `inputs:framerange` at default time, creating the typed input if needed.
    pub fn set_framerange(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:framerange` sample in stage time through the edit target.
    pub fn set_framerange_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:layer` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_layer_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "layer", ty)
    }
    /// Authors `inputs:layer` at default time, creating the typed input if needed.
    pub fn set_layer(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_layer_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:layer` sample in stage time through the edit target.
    pub fn set_layer_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_layer_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:texcoord` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_texcoord_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "texcoord", ty)
    }
    /// Authors `inputs:texcoord` at default time, creating the typed input if needed.
    pub fn set_texcoord(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:texcoord` sample in stage time through the edit target.
    pub fn set_texcoord_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uaddressmode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uaddressmode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "uaddressmode", ty)
    }
    /// Authors `inputs:uaddressmode` at default time, creating the typed input if needed.
    pub fn set_uaddressmode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_uaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uaddressmode` sample in stage time through the edit target.
    pub fn set_uaddressmode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_uaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:vaddressmode` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_vaddressmode_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "vaddressmode", ty)
    }
    /// Authors `inputs:vaddressmode` at default time, creating the typed input if needed.
    pub fn set_vaddressmode(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_vaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:vaddressmode` sample in stage time through the edit target.
    pub fn set_vaddressmode_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_vaddressmode_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_invert_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdInvertFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdInvertFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdInvertFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_invert_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdInvertFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdInvertFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "amount",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:amount",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "in",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "in",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:amount` port, if it exists. "]
    #[must_use]
    pub fn amount_input(&self) -> Option<Port<'a>> {
        self.shader.input("amount")
    }
    #[doc = "The composed `inputs:amount`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn amount(&self) -> Option<f32> {
        self.read_value("inputs:amount", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:amount` at `time`.
    #[must_use]
    pub fn amount_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:amount", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:amount` default in the node definition, separate from authored values.
    #[must_use]
    pub fn amount_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:in` port, if it exists. "]
    #[must_use]
    pub fn in_value_input(&self) -> Option<Port<'a>> {
        self.shader.input("in")
    }
    #[doc = "The composed `inputs:in`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in_value(&self) -> Option<f32> {
        self.read_value("inputs:in", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in` at `time`.
    #[must_use]
    pub fn in_value_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in_value_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_invert_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdInvertFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdInvertFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdInvertFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:amount` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_amount_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "amount", ty)
    }
    /// Authors `inputs:amount` at default time, creating the typed input if needed.
    pub fn set_amount(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_amount_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:amount` sample in stage time through the edit target.
    pub fn set_amount_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_amount_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in_value_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in", ty)
    }
    /// Authors `inputs:in` at default time, creating the typed input if needed.
    pub fn set_in_value(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in` sample in stage time through the edit target.
    pub fn set_in_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_mix_color3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdMixColor3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdMixColor3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdMixColor3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_mix_color3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<NdMixColor3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdMixColor3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "bg",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:bg",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "fg",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:fg",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "mix",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:mix",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color3",
                usd_name: "outputs:out",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "bg",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:bg` port, if it exists. "]
    #[must_use]
    pub fn bg_input(&self) -> Option<Port<'a>> {
        self.shader.input("bg")
    }
    #[doc = "The composed `inputs:bg`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn bg(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:bg", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:bg` at `time`.
    #[must_use]
    pub fn bg_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:bg", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:bg` default in the node definition, separate from authored values.
    #[must_use]
    pub fn bg_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:fg` port, if it exists. "]
    #[must_use]
    pub fn fg_input(&self) -> Option<Port<'a>> {
        self.shader.input("fg")
    }
    #[doc = "The composed `inputs:fg`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn fg(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:fg", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:fg` at `time`.
    #[must_use]
    pub fn fg_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:fg", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:fg` default in the node definition, separate from authored values.
    #[must_use]
    pub fn fg_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:mix` port, if it exists. "]
    #[must_use]
    pub fn mix_input(&self) -> Option<Port<'a>> {
        self.shader.input("mix")
    }
    #[doc = "The composed `inputs:mix`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn mix(&self) -> Option<f32> {
        self.read_value("inputs:mix", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:mix` at `time`.
    #[must_use]
    pub fn mix_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:mix", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:mix` default in the node definition, separate from authored values.
    #[must_use]
    pub fn mix_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_mix_color3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdMixColor3Edit {
    shader: ShaderEdit,
}
impl Deref for NdMixColor3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdMixColor3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:bg` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_bg_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "bg", ty)
    }
    /// Authors `inputs:bg` at default time, creating the typed input if needed.
    pub fn set_bg(&self, edit: &mut SchemaEdit<'_>, value: [f32; 3]) -> Result<&Self, PortError> {
        let port = self.create_bg_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:bg` sample in stage time through the edit target.
    pub fn set_bg_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_bg_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:fg` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_fg_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "fg", ty)
    }
    /// Authors `inputs:fg` at default time, creating the typed input if needed.
    pub fn set_fg(&self, edit: &mut SchemaEdit<'_>, value: [f32; 3]) -> Result<&Self, PortError> {
        let port = self.create_fg_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:fg` sample in stage time through the edit target.
    pub fn set_fg_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_fg_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:mix` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_mix_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "mix", ty)
    }
    /// Authors `inputs:mix` at default time, creating the typed input if needed.
    pub fn set_mix(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_mix_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:mix` sample in stage time through the edit target.
    pub fn set_mix_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_mix_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_mix_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdMixFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdMixFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdMixFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_mix_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<NdMixFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdMixFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "bg",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:bg",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "fg",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:fg",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "mix",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:mix",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "bg",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:bg` port, if it exists. "]
    #[must_use]
    pub fn bg_input(&self) -> Option<Port<'a>> {
        self.shader.input("bg")
    }
    #[doc = "The composed `inputs:bg`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn bg(&self) -> Option<f32> {
        self.read_value("inputs:bg", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:bg` at `time`.
    #[must_use]
    pub fn bg_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:bg", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:bg` default in the node definition, separate from authored values.
    #[must_use]
    pub fn bg_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:fg` port, if it exists. "]
    #[must_use]
    pub fn fg_input(&self) -> Option<Port<'a>> {
        self.shader.input("fg")
    }
    #[doc = "The composed `inputs:fg`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn fg(&self) -> Option<f32> {
        self.read_value("inputs:fg", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:fg` at `time`.
    #[must_use]
    pub fn fg_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:fg", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:fg` default in the node definition, separate from authored values.
    #[must_use]
    pub fn fg_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:mix` port, if it exists. "]
    #[must_use]
    pub fn mix_input(&self) -> Option<Port<'a>> {
        self.shader.input("mix")
    }
    #[doc = "The composed `inputs:mix`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn mix(&self) -> Option<f32> {
        self.read_value("inputs:mix", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:mix` at `time`.
    #[must_use]
    pub fn mix_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:mix", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:mix` default in the node definition, separate from authored values.
    #[must_use]
    pub fn mix_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_mix_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdMixFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdMixFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdMixFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:bg` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_bg_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "bg", ty)
    }
    /// Authors `inputs:bg` at default time, creating the typed input if needed.
    pub fn set_bg(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_bg_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:bg` sample in stage time through the edit target.
    pub fn set_bg_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_bg_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:fg` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_fg_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "fg", ty)
    }
    /// Authors `inputs:fg` at default time, creating the typed input if needed.
    pub fn set_fg(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_fg_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:fg` sample in stage time through the edit target.
    pub fn set_fg_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_fg_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:mix` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_mix_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "mix", ty)
    }
    /// Authors `inputs:mix` at default time, creating the typed input if needed.
    pub fn set_mix(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_mix_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:mix` sample in stage time through the edit target.
    pub fn set_mix_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_mix_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_multiply_color3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdMultiplyColor3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdMultiplyColor3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdMultiplyColor3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_multiply_color3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdMultiplyColor3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdMultiplyColor3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in1",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:in1",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "in2",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:in2",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color3",
                usd_name: "outputs:out",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "in1",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:in1` port, if it exists. "]
    #[must_use]
    pub fn in1_input(&self) -> Option<Port<'a>> {
        self.shader.input("in1")
    }
    #[doc = "The composed `inputs:in1`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in1(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in1", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in1` at `time`.
    #[must_use]
    pub fn in1_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in1", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in1` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in1_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:in2` port, if it exists. "]
    #[must_use]
    pub fn in2_input(&self) -> Option<Port<'a>> {
        self.shader.input("in2")
    }
    #[doc = "The composed `inputs:in2`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in2(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in2", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in2` at `time`.
    #[must_use]
    pub fn in2_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in2", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in2` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in2_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_multiply_color3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdMultiplyColor3Edit {
    shader: ShaderEdit,
}
impl Deref for NdMultiplyColor3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdMultiplyColor3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in1` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in1_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "in1", ty)
    }
    /// Authors `inputs:in1` at default time, creating the typed input if needed.
    pub fn set_in1(&self, edit: &mut SchemaEdit<'_>, value: [f32; 3]) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in1` sample in stage time through the edit target.
    pub fn set_in1_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in2` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in2_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "in2", ty)
    }
    /// Authors `inputs:in2` at default time, creating the typed input if needed.
    pub fn set_in2(&self, edit: &mut SchemaEdit<'_>, value: [f32; 3]) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in2` sample in stage time through the edit target.
    pub fn set_in2_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_multiply_color3FA` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdMultiplyColor3Fa<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdMultiplyColor3Fa<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdMultiplyColor3Fa<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_multiply_color3FA";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdMultiplyColor3FaEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdMultiplyColor3FaEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in1",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:in1",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "in2",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in2",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color3",
                usd_name: "outputs:out",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "in1",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:in1` port, if it exists. "]
    #[must_use]
    pub fn in1_input(&self) -> Option<Port<'a>> {
        self.shader.input("in1")
    }
    #[doc = "The composed `inputs:in1`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in1(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in1", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in1` at `time`.
    #[must_use]
    pub fn in1_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in1", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in1` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in1_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:in2` port, if it exists. "]
    #[must_use]
    pub fn in2_input(&self) -> Option<Port<'a>> {
        self.shader.input("in2")
    }
    #[doc = "The composed `inputs:in2`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in2(&self) -> Option<f32> {
        self.read_value("inputs:in2", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in2` at `time`.
    #[must_use]
    pub fn in2_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in2", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in2` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in2_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_multiply_color3FA` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdMultiplyColor3FaEdit {
    shader: ShaderEdit,
}
impl Deref for NdMultiplyColor3FaEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdMultiplyColor3FaEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in1` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in1_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "in1", ty)
    }
    /// Authors `inputs:in1` at default time, creating the typed input if needed.
    pub fn set_in1(&self, edit: &mut SchemaEdit<'_>, value: [f32; 3]) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in1` sample in stage time through the edit target.
    pub fn set_in1_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in2` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in2_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in2", ty)
    }
    /// Authors `inputs:in2` at default time, creating the typed input if needed.
    pub fn set_in2(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in2` sample in stage time through the edit target.
    pub fn set_in2_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_multiply_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdMultiplyFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdMultiplyFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdMultiplyFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_multiply_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdMultiplyFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdMultiplyFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in1",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in1",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "in2",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in2",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "in1",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:in1` port, if it exists. "]
    #[must_use]
    pub fn in1_input(&self) -> Option<Port<'a>> {
        self.shader.input("in1")
    }
    #[doc = "The composed `inputs:in1`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in1(&self) -> Option<f32> {
        self.read_value("inputs:in1", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in1` at `time`.
    #[must_use]
    pub fn in1_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in1", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in1` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in1_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:in2` port, if it exists. "]
    #[must_use]
    pub fn in2_input(&self) -> Option<Port<'a>> {
        self.shader.input("in2")
    }
    #[doc = "The composed `inputs:in2`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in2(&self) -> Option<f32> {
        self.read_value("inputs:in2", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in2` at `time`.
    #[must_use]
    pub fn in2_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in2", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in2` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in2_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_multiply_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdMultiplyFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdMultiplyFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdMultiplyFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in1` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in1_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in1", ty)
    }
    /// Authors `inputs:in1` at default time, creating the typed input if needed.
    pub fn set_in1(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in1` sample in stage time through the edit target.
    pub fn set_in1_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in1_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in2` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in2_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in2", ty)
    }
    /// Authors `inputs:in2` at default time, creating the typed input if needed.
    pub fn set_in2(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in2` sample in stage time through the edit target.
    pub fn set_in2_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in2_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_normalmap_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdNormalmapFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdNormalmapFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdNormalmapFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_normalmap_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdNormalmapFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdNormalmapFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "bitangent",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:bitangent",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Bworld",
                )),
            },
            MaterialXPort {
                name: "in",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:in",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.5, 0.5, 1.0",
                )),
            },
            MaterialXPort {
                name: "normal",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:normal",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Nworld",
                )),
            },
            MaterialXPort {
                name: "scale",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:scale",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "tangent",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:tangent",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Tworld",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "vector3",
                usd_name: "outputs:out",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "normal",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:bitangent` port, if it exists. "]
    #[must_use]
    pub fn bitangent_input(&self) -> Option<Port<'a>> {
        self.shader.input("bitangent")
    }
    #[doc = "The composed `inputs:bitangent`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn bitangent(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:bitangent", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:bitangent` at `time`.
    #[must_use]
    pub fn bitangent_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:bitangent",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:in` port, if it exists. "]
    #[must_use]
    pub fn in_value_input(&self) -> Option<Port<'a>> {
        self.shader.input("in")
    }
    #[doc = "The composed `inputs:in`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in_value(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in` at `time`.
    #[must_use]
    pub fn in_value_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in_value_default() -> [f32; 3] {
        [0.5, 0.5, 1.0]
    }
    #[doc = "The authored `inputs:normal` port, if it exists. "]
    #[must_use]
    pub fn normal_input(&self) -> Option<Port<'a>> {
        self.shader.input("normal")
    }
    #[doc = "The composed `inputs:normal`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn normal(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:normal", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:normal` at `time`.
    #[must_use]
    pub fn normal_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:normal",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:scale` port, if it exists. "]
    #[must_use]
    pub fn scale_input(&self) -> Option<Port<'a>> {
        self.shader.input("scale")
    }
    #[doc = "The composed `inputs:scale`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn scale(&self) -> Option<f32> {
        self.read_value("inputs:scale", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:scale` at `time`.
    #[must_use]
    pub fn scale_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:scale", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:scale` default in the node definition, separate from authored values.
    #[must_use]
    pub fn scale_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:tangent` port, if it exists. "]
    #[must_use]
    pub fn tangent_input(&self) -> Option<Port<'a>> {
        self.shader.input("tangent")
    }
    #[doc = "The composed `inputs:tangent`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn tangent(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:tangent", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:tangent` at `time`.
    #[must_use]
    pub fn tangent_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:tangent",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_normalmap_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdNormalmapFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdNormalmapFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdNormalmapFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:bitangent` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_bitangent_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "bitangent", ty)
    }
    /// Authors `inputs:bitangent` at default time, creating the typed input if needed.
    pub fn set_bitangent(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_bitangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:bitangent` sample in stage time through the edit target.
    pub fn set_bitangent_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_bitangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:in` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in_value_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "in", ty)
    }
    /// Authors `inputs:in` at default time, creating the typed input if needed.
    pub fn set_in_value(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in` sample in stage time through the edit target.
    pub fn set_in_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:normal` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_normal_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "normal", ty)
    }
    /// Authors `inputs:normal` at default time, creating the typed input if needed.
    pub fn set_normal(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:normal` sample in stage time through the edit target.
    pub fn set_normal_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:scale` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_scale_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "scale", ty)
    }
    /// Authors `inputs:scale` at default time, creating the typed input if needed.
    pub fn set_scale(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:scale` sample in stage time through the edit target.
    pub fn set_scale_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:tangent` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_tangent_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "tangent", ty)
    }
    /// Authors `inputs:tangent` at default time, creating the typed input if needed.
    pub fn set_tangent(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:tangent` sample in stage time through the edit target.
    pub fn set_tangent_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_open_pbr_surface_surfaceshader` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdOpenPbrSurfaceSurfaceshader<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdOpenPbrSurfaceSurfaceshader<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdOpenPbrSurfaceSurfaceshader<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_open_pbr_surface_surfaceshader";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdOpenPbrSurfaceSurfaceshaderEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdOpenPbrSurfaceSurfaceshaderEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "base_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:base_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.8, 0.8, 0.8",
                )),
            },
            MaterialXPort {
                name: "base_diffuse_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:base_diffuse_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "base_metalness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:base_metalness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "base_weight",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:base_weight",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "coat_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:coat_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "coat_darkening",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_darkening",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "coat_ior",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_ior",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.6",
                )),
            },
            MaterialXPort {
                name: "coat_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "coat_roughness_anisotropy",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_roughness_anisotropy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "coat_weight",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_weight",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "emission_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:emission_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "emission_luminance",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:emission_luminance",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "fuzz_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:fuzz_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "fuzz_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:fuzz_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.5",
                )),
            },
            MaterialXPort {
                name: "fuzz_weight",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:fuzz_weight",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "geometry_coat_normal",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:geometry_coat_normal",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Nworld",
                )),
            },
            MaterialXPort {
                name: "geometry_coat_tangent",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:geometry_coat_tangent",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Tworld",
                )),
            },
            MaterialXPort {
                name: "geometry_normal",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:geometry_normal",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Nworld",
                )),
            },
            MaterialXPort {
                name: "geometry_opacity",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:geometry_opacity",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "geometry_tangent",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:geometry_tangent",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Tworld",
                )),
            },
            MaterialXPort {
                name: "geometry_thin_walled",
                direction: PortKind::Input,
                type_name: "boolean",
                usd_name: "inputs:geometry_thin_walled",
                usd_type_name: "bool",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "false",
                )),
            },
            MaterialXPort {
                name: "specular_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:specular_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "specular_ior",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular_ior",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.5",
                )),
            },
            MaterialXPort {
                name: "specular_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.3",
                )),
            },
            MaterialXPort {
                name: "specular_roughness_anisotropy",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular_roughness_anisotropy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "specular_weight",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular_weight",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "subsurface_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:subsurface_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.8, 0.8, 0.8",
                )),
            },
            MaterialXPort {
                name: "subsurface_radius",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:subsurface_radius",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "subsurface_radius_scale",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:subsurface_radius_scale",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 0.5, 0.25",
                )),
            },
            MaterialXPort {
                name: "subsurface_scatter_anisotropy",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:subsurface_scatter_anisotropy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "subsurface_weight",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:subsurface_weight",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "thin_film_ior",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:thin_film_ior",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.4",
                )),
            },
            MaterialXPort {
                name: "thin_film_thickness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:thin_film_thickness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.5",
                )),
            },
            MaterialXPort {
                name: "thin_film_weight",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:thin_film_weight",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "transmission_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:transmission_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "transmission_depth",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_depth",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "transmission_dispersion_abbe_number",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_dispersion_abbe_number",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "20.0",
                )),
            },
            MaterialXPort {
                name: "transmission_dispersion_scale",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_dispersion_scale",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "transmission_scatter",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:transmission_scatter",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0, 0, 0",
                )),
            },
            MaterialXPort {
                name: "transmission_scatter_anisotropy",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_scatter_anisotropy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "transmission_weight",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_weight",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "surfaceshader",
                usd_name: "outputs:surface",
                usd_type_name: "token",
                default: None,
            },
        ],
    };
    #[doc = "The authored `inputs:base_color` port, if it exists. Color of the reflection from the diffuse and metallic base."]
    #[must_use]
    pub fn base_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("base_color")
    }
    #[doc = "The composed `inputs:base_color`, without node defaults or connection evaluation. Color of the reflection from the diffuse and metallic base."]
    #[must_use]
    pub fn base_color(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:base_color", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:base_color` at `time`.
    #[must_use]
    pub fn base_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:base_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:base_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn base_color_default() -> [f32; 3] {
        [0.8, 0.8, 0.8]
    }
    #[doc = "The authored `inputs:base_diffuse_roughness` port, if it exists. Roughness of the diffuse reflection. Higher values cause the surface to appear flatter."]
    #[must_use]
    pub fn base_diffuse_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("base_diffuse_roughness")
    }
    #[doc = "The composed `inputs:base_diffuse_roughness`, without node defaults or connection evaluation. Roughness of the diffuse reflection. Higher values cause the surface to appear flatter."]
    #[must_use]
    pub fn base_diffuse_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:base_diffuse_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:base_diffuse_roughness` at `time`.
    #[must_use]
    pub fn base_diffuse_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:base_diffuse_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:base_diffuse_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn base_diffuse_roughness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:base_metalness` port, if it exists. Specifies how metallic the base material appears (dials the base from pure dielectric to pure metal)."]
    #[must_use]
    pub fn base_metalness_input(&self) -> Option<Port<'a>> {
        self.shader.input("base_metalness")
    }
    #[doc = "The composed `inputs:base_metalness`, without node defaults or connection evaluation. Specifies how metallic the base material appears (dials the base from pure dielectric to pure metal)."]
    #[must_use]
    pub fn base_metalness(&self) -> Option<f32> {
        self.read_value(
            "inputs:base_metalness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:base_metalness` at `time`.
    #[must_use]
    pub fn base_metalness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:base_metalness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:base_metalness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn base_metalness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:base_weight` port, if it exists. Multiplier on the intensity of the reflection from the diffuse and metallic base."]
    #[must_use]
    pub fn base_weight_input(&self) -> Option<Port<'a>> {
        self.shader.input("base_weight")
    }
    #[doc = "The composed `inputs:base_weight`, without node defaults or connection evaluation. Multiplier on the intensity of the reflection from the diffuse and metallic base."]
    #[must_use]
    pub fn base_weight(&self) -> Option<f32> {
        self.read_value("inputs:base_weight", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:base_weight` at `time`.
    #[must_use]
    pub fn base_weight_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:base_weight",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:base_weight` default in the node definition, separate from authored values.
    #[must_use]
    pub fn base_weight_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:coat_color` port, if it exists. The color of the clear-coat layer's transparency, due to absorption in the coat."]
    #[must_use]
    pub fn coat_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_color")
    }
    #[doc = "The composed `inputs:coat_color`, without node defaults or connection evaluation. The color of the clear-coat layer's transparency, due to absorption in the coat."]
    #[must_use]
    pub fn coat_color(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:coat_color", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:coat_color` at `time`.
    #[must_use]
    pub fn coat_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:coat_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:coat_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:coat_darkening` port, if it exists. Modulates the physical coat darkening effect."]
    #[must_use]
    pub fn coat_darkening_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_darkening")
    }
    #[doc = "The composed `inputs:coat_darkening`, without node defaults or connection evaluation. Modulates the physical coat darkening effect."]
    #[must_use]
    pub fn coat_darkening(&self) -> Option<f32> {
        self.read_value(
            "inputs:coat_darkening",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:coat_darkening` at `time`.
    #[must_use]
    pub fn coat_darkening_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_darkening",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_darkening` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_darkening_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:coat_ior` port, if it exists. The index of refraction of the clear-coat layer."]
    #[must_use]
    pub fn coat_ior_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_ior")
    }
    #[doc = "The composed `inputs:coat_ior`, without node defaults or connection evaluation. The index of refraction of the clear-coat layer."]
    #[must_use]
    pub fn coat_ior(&self) -> Option<f32> {
        self.read_value("inputs:coat_ior", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:coat_ior` at `time`.
    #[must_use]
    pub fn coat_ior_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_ior",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_ior` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_ior_default() -> f32 {
        1.6_f32
    }
    #[doc = "The authored `inputs:coat_roughness` port, if it exists. The roughness of the clear-coat reflections. The lower the value, the sharper the reflection."]
    #[must_use]
    pub fn coat_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_roughness")
    }
    #[doc = "The composed `inputs:coat_roughness`, without node defaults or connection evaluation. The roughness of the clear-coat reflections. The lower the value, the sharper the reflection."]
    #[must_use]
    pub fn coat_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:coat_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:coat_roughness` at `time`.
    #[must_use]
    pub fn coat_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_roughness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:coat_roughness_anisotropy` port, if it exists. The directional bias of the roughness of the clear-coat layer, resulting in increasingly stretched highlights along the coat tangent direction."]
    #[must_use]
    pub fn coat_roughness_anisotropy_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_roughness_anisotropy")
    }
    #[doc = "The composed `inputs:coat_roughness_anisotropy`, without node defaults or connection evaluation. The directional bias of the roughness of the clear-coat layer, resulting in increasingly stretched highlights along the coat tangent direction."]
    #[must_use]
    pub fn coat_roughness_anisotropy(&self) -> Option<f32> {
        self.read_value(
            "inputs:coat_roughness_anisotropy",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:coat_roughness_anisotropy` at `time`.
    #[must_use]
    pub fn coat_roughness_anisotropy_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_roughness_anisotropy",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_roughness_anisotropy` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_roughness_anisotropy_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:coat_weight` port, if it exists. The presence weight of a reflective clear-coat layer on top of the material. Use for materials such as car paint or an oily layer."]
    #[must_use]
    pub fn coat_weight_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_weight")
    }
    #[doc = "The composed `inputs:coat_weight`, without node defaults or connection evaluation. The presence weight of a reflective clear-coat layer on top of the material. Use for materials such as car paint or an oily layer."]
    #[must_use]
    pub fn coat_weight(&self) -> Option<f32> {
        self.read_value("inputs:coat_weight", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:coat_weight` at `time`.
    #[must_use]
    pub fn coat_weight_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_weight",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_weight` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_weight_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:emission_color` port, if it exists. The color of the emitted light."]
    #[must_use]
    pub fn emission_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("emission_color")
    }
    #[doc = "The composed `inputs:emission_color`, without node defaults or connection evaluation. The color of the emitted light."]
    #[must_use]
    pub fn emission_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:emission_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:emission_color` at `time`.
    #[must_use]
    pub fn emission_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:emission_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:emission_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn emission_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:emission_luminance` port, if it exists. The amount of emitted light, as a luminance in nits."]
    #[must_use]
    pub fn emission_luminance_input(&self) -> Option<Port<'a>> {
        self.shader.input("emission_luminance")
    }
    #[doc = "The composed `inputs:emission_luminance`, without node defaults or connection evaluation. The amount of emitted light, as a luminance in nits."]
    #[must_use]
    pub fn emission_luminance(&self) -> Option<f32> {
        self.read_value(
            "inputs:emission_luminance",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:emission_luminance` at `time`.
    #[must_use]
    pub fn emission_luminance_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:emission_luminance",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:emission_luminance` default in the node definition, separate from authored values.
    #[must_use]
    pub fn emission_luminance_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:fuzz_color` port, if it exists. The color of the fuzz layer."]
    #[must_use]
    pub fn fuzz_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("fuzz_color")
    }
    #[doc = "The composed `inputs:fuzz_color`, without node defaults or connection evaluation. The color of the fuzz layer."]
    #[must_use]
    pub fn fuzz_color(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:fuzz_color", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:fuzz_color` at `time`.
    #[must_use]
    pub fn fuzz_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:fuzz_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:fuzz_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn fuzz_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:fuzz_roughness` port, if it exists. The roughness of the fuzz layer."]
    #[must_use]
    pub fn fuzz_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("fuzz_roughness")
    }
    #[doc = "The composed `inputs:fuzz_roughness`, without node defaults or connection evaluation. The roughness of the fuzz layer."]
    #[must_use]
    pub fn fuzz_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:fuzz_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:fuzz_roughness` at `time`.
    #[must_use]
    pub fn fuzz_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:fuzz_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:fuzz_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn fuzz_roughness_default() -> f32 {
        0.5_f32
    }
    #[doc = "The authored `inputs:fuzz_weight` port, if it exists. The presence weight of a fuzz layer that can be used to approximate microfibers, for fabrics such as velvet and satin as well as dust grains."]
    #[must_use]
    pub fn fuzz_weight_input(&self) -> Option<Port<'a>> {
        self.shader.input("fuzz_weight")
    }
    #[doc = "The composed `inputs:fuzz_weight`, without node defaults or connection evaluation. The presence weight of a fuzz layer that can be used to approximate microfibers, for fabrics such as velvet and satin as well as dust grains."]
    #[must_use]
    pub fn fuzz_weight(&self) -> Option<f32> {
        self.read_value("inputs:fuzz_weight", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:fuzz_weight` at `time`.
    #[must_use]
    pub fn fuzz_weight_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:fuzz_weight",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:fuzz_weight` default in the node definition, separate from authored values.
    #[must_use]
    pub fn fuzz_weight_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:geometry_coat_normal` port, if it exists. Input normal for coat layer"]
    #[must_use]
    pub fn geometry_coat_normal_input(&self) -> Option<Port<'a>> {
        self.shader.input("geometry_coat_normal")
    }
    #[doc = "The composed `inputs:geometry_coat_normal`, without node defaults or connection evaluation. Input normal for coat layer"]
    #[must_use]
    pub fn geometry_coat_normal(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:geometry_coat_normal",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:geometry_coat_normal` at `time`.
    #[must_use]
    pub fn geometry_coat_normal_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:geometry_coat_normal",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:geometry_coat_tangent` port, if it exists. Input geometric tangent for coat layer"]
    #[must_use]
    pub fn geometry_coat_tangent_input(&self) -> Option<Port<'a>> {
        self.shader.input("geometry_coat_tangent")
    }
    #[doc = "The composed `inputs:geometry_coat_tangent`, without node defaults or connection evaluation. Input geometric tangent for coat layer"]
    #[must_use]
    pub fn geometry_coat_tangent(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:geometry_coat_tangent",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:geometry_coat_tangent` at `time`.
    #[must_use]
    pub fn geometry_coat_tangent_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:geometry_coat_tangent",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:geometry_normal` port, if it exists. Input geometric normal"]
    #[must_use]
    pub fn geometry_normal_input(&self) -> Option<Port<'a>> {
        self.shader.input("geometry_normal")
    }
    #[doc = "The composed `inputs:geometry_normal`, without node defaults or connection evaluation. Input geometric normal"]
    #[must_use]
    pub fn geometry_normal(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:geometry_normal",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:geometry_normal` at `time`.
    #[must_use]
    pub fn geometry_normal_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:geometry_normal",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:geometry_opacity` port, if it exists. The opacity of the entire material."]
    #[must_use]
    pub fn geometry_opacity_input(&self) -> Option<Port<'a>> {
        self.shader.input("geometry_opacity")
    }
    #[doc = "The composed `inputs:geometry_opacity`, without node defaults or connection evaluation. The opacity of the entire material."]
    #[must_use]
    pub fn geometry_opacity(&self) -> Option<f32> {
        self.read_value(
            "inputs:geometry_opacity",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:geometry_opacity` at `time`.
    #[must_use]
    pub fn geometry_opacity_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:geometry_opacity",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:geometry_opacity` default in the node definition, separate from authored values.
    #[must_use]
    pub fn geometry_opacity_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:geometry_tangent` port, if it exists. Input geometric tangent"]
    #[must_use]
    pub fn geometry_tangent_input(&self) -> Option<Port<'a>> {
        self.shader.input("geometry_tangent")
    }
    #[doc = "The composed `inputs:geometry_tangent`, without node defaults or connection evaluation. Input geometric tangent"]
    #[must_use]
    pub fn geometry_tangent(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:geometry_tangent",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:geometry_tangent` at `time`.
    #[must_use]
    pub fn geometry_tangent_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:geometry_tangent",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:geometry_thin_walled` port, if it exists. If true the surface is double-sided and represents an infinitesimally thin shell. Suitable for extremely geometrically thin objects such as leaves or paper."]
    #[must_use]
    pub fn geometry_thin_walled_input(&self) -> Option<Port<'a>> {
        self.shader.input("geometry_thin_walled")
    }
    #[doc = "The composed `inputs:geometry_thin_walled`, without node defaults or connection evaluation. If true the surface is double-sided and represents an infinitesimally thin shell. Suitable for extremely geometrically thin objects such as leaves or paper."]
    #[must_use]
    pub fn geometry_thin_walled(&self) -> Option<bool> {
        self.read_value(
            "inputs:geometry_thin_walled",
            layerstack_schemas::value::read_bool,
        )
    }
    /// The composed `inputs:geometry_thin_walled` at `time`.
    #[must_use]
    pub fn geometry_thin_walled_at(&self, time: layerstack::Time) -> Option<bool> {
        self.read_value_at(
            "inputs:geometry_thin_walled",
            time,
            layerstack_schemas::value::read_bool,
        )
    }
    /// The `inputs:geometry_thin_walled` default in the node definition, separate from authored values.
    #[must_use]
    pub fn geometry_thin_walled_default() -> bool {
        false
    }
    #[doc = "The authored `inputs:specular_color` port, if it exists. Color of the specular reflection (controls the physical edge-tint for metals, and a non-physical overall tint for dielectrics)."]
    #[must_use]
    pub fn specular_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_color")
    }
    #[doc = "The composed `inputs:specular_color`, without node defaults or connection evaluation. Color of the specular reflection (controls the physical edge-tint for metals, and a non-physical overall tint for dielectrics)."]
    #[must_use]
    pub fn specular_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:specular_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:specular_color` at `time`.
    #[must_use]
    pub fn specular_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:specular_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:specular_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:specular_ior` port, if it exists. Index of refraction of the dielectric base."]
    #[must_use]
    pub fn specular_ior_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_ior")
    }
    #[doc = "The composed `inputs:specular_ior`, without node defaults or connection evaluation. Index of refraction of the dielectric base."]
    #[must_use]
    pub fn specular_ior(&self) -> Option<f32> {
        self.read_value("inputs:specular_ior", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:specular_ior` at `time`.
    #[must_use]
    pub fn specular_ior_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular_ior",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular_ior` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_ior_default() -> f32 {
        1.5_f32
    }
    #[doc = "The authored `inputs:specular_roughness` port, if it exists. The roughness of the specular reflection. Lower numbers produce sharper reflections, higher numbers produce blurrier reflections."]
    #[must_use]
    pub fn specular_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_roughness")
    }
    #[doc = "The composed `inputs:specular_roughness`, without node defaults or connection evaluation. The roughness of the specular reflection. Lower numbers produce sharper reflections, higher numbers produce blurrier reflections."]
    #[must_use]
    pub fn specular_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:specular_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:specular_roughness` at `time`.
    #[must_use]
    pub fn specular_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_roughness_default() -> f32 {
        0.3_f32
    }
    #[doc = "The authored `inputs:specular_roughness_anisotropy` port, if it exists. The directional bias of the roughness of the metal/dielectric base, resulting in increasingly stretched highlights along the tangent direction."]
    #[must_use]
    pub fn specular_roughness_anisotropy_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_roughness_anisotropy")
    }
    #[doc = "The composed `inputs:specular_roughness_anisotropy`, without node defaults or connection evaluation. The directional bias of the roughness of the metal/dielectric base, resulting in increasingly stretched highlights along the tangent direction."]
    #[must_use]
    pub fn specular_roughness_anisotropy(&self) -> Option<f32> {
        self.read_value(
            "inputs:specular_roughness_anisotropy",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:specular_roughness_anisotropy` at `time`.
    #[must_use]
    pub fn specular_roughness_anisotropy_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular_roughness_anisotropy",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular_roughness_anisotropy` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_roughness_anisotropy_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:specular_weight` port, if it exists. Multiplies the specular reflectivity."]
    #[must_use]
    pub fn specular_weight_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_weight")
    }
    #[doc = "The composed `inputs:specular_weight`, without node defaults or connection evaluation. Multiplies the specular reflectivity."]
    #[must_use]
    pub fn specular_weight(&self) -> Option<f32> {
        self.read_value(
            "inputs:specular_weight",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:specular_weight` at `time`.
    #[must_use]
    pub fn specular_weight_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular_weight",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular_weight` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_weight_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:subsurface_color` port, if it exists. The observed reflection color of the subsurface scattering medium."]
    #[must_use]
    pub fn subsurface_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_color")
    }
    #[doc = "The composed `inputs:subsurface_color`, without node defaults or connection evaluation. The observed reflection color of the subsurface scattering medium."]
    #[must_use]
    pub fn subsurface_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:subsurface_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:subsurface_color` at `time`.
    #[must_use]
    pub fn subsurface_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:subsurface_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:subsurface_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_color_default() -> [f32; 3] {
        [0.8, 0.8, 0.8]
    }
    #[doc = "The authored `inputs:subsurface_radius` port, if it exists. Length scale of the subsurface scattering mean free path."]
    #[must_use]
    pub fn subsurface_radius_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_radius")
    }
    #[doc = "The composed `inputs:subsurface_radius`, without node defaults or connection evaluation. Length scale of the subsurface scattering mean free path."]
    #[must_use]
    pub fn subsurface_radius(&self) -> Option<f32> {
        self.read_value(
            "inputs:subsurface_radius",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:subsurface_radius` at `time`.
    #[must_use]
    pub fn subsurface_radius_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:subsurface_radius",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:subsurface_radius` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_radius_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:subsurface_radius_scale` port, if it exists. RGB multiplier to subsurface_radius, giving the per-channel scattering mean-free-paths."]
    #[must_use]
    pub fn subsurface_radius_scale_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_radius_scale")
    }
    #[doc = "The composed `inputs:subsurface_radius_scale`, without node defaults or connection evaluation. RGB multiplier to subsurface_radius, giving the per-channel scattering mean-free-paths."]
    #[must_use]
    pub fn subsurface_radius_scale(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:subsurface_radius_scale",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:subsurface_radius_scale` at `time`.
    #[must_use]
    pub fn subsurface_radius_scale_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:subsurface_radius_scale",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:subsurface_radius_scale` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_radius_scale_default() -> [f32; 3] {
        [1.0, 0.5, 0.25]
    }
    #[doc = "The authored `inputs:subsurface_scatter_anisotropy` port, if it exists. Controls the phase-function of subsurface scattering, where zero scatters light evenly, positive values scatter forwards, and negative values scatter backwards."]
    #[must_use]
    pub fn subsurface_scatter_anisotropy_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_scatter_anisotropy")
    }
    #[doc = "The composed `inputs:subsurface_scatter_anisotropy`, without node defaults or connection evaluation. Controls the phase-function of subsurface scattering, where zero scatters light evenly, positive values scatter forwards, and negative values scatter backwards."]
    #[must_use]
    pub fn subsurface_scatter_anisotropy(&self) -> Option<f32> {
        self.read_value(
            "inputs:subsurface_scatter_anisotropy",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:subsurface_scatter_anisotropy` at `time`.
    #[must_use]
    pub fn subsurface_scatter_anisotropy_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:subsurface_scatter_anisotropy",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:subsurface_scatter_anisotropy` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_scatter_anisotropy_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:subsurface_weight` port, if it exists. Mixture weight which dials the opaque dielectric base between diffuse reflection and subsurface scattering. A value of 1.0 indicates full subsurface scattering and a value 0 for diffuse reflection only."]
    #[must_use]
    pub fn subsurface_weight_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_weight")
    }
    #[doc = "The composed `inputs:subsurface_weight`, without node defaults or connection evaluation. Mixture weight which dials the opaque dielectric base between diffuse reflection and subsurface scattering. A value of 1.0 indicates full subsurface scattering and a value 0 for diffuse reflection only."]
    #[must_use]
    pub fn subsurface_weight(&self) -> Option<f32> {
        self.read_value(
            "inputs:subsurface_weight",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:subsurface_weight` at `time`.
    #[must_use]
    pub fn subsurface_weight_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:subsurface_weight",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:subsurface_weight` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_weight_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:thin_film_ior` port, if it exists. The index of refraction of the thin-film."]
    #[must_use]
    pub fn thin_film_ior_input(&self) -> Option<Port<'a>> {
        self.shader.input("thin_film_ior")
    }
    #[doc = "The composed `inputs:thin_film_ior`, without node defaults or connection evaluation. The index of refraction of the thin-film."]
    #[must_use]
    pub fn thin_film_ior(&self) -> Option<f32> {
        self.read_value(
            "inputs:thin_film_ior",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:thin_film_ior` at `time`.
    #[must_use]
    pub fn thin_film_ior_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:thin_film_ior",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:thin_film_ior` default in the node definition, separate from authored values.
    #[must_use]
    pub fn thin_film_ior_default() -> f32 {
        1.4_f32
    }
    #[doc = "The authored `inputs:thin_film_thickness` port, if it exists. The thickness of the thin-film layer on the base (in micrometers)."]
    #[must_use]
    pub fn thin_film_thickness_input(&self) -> Option<Port<'a>> {
        self.shader.input("thin_film_thickness")
    }
    #[doc = "The composed `inputs:thin_film_thickness`, without node defaults or connection evaluation. The thickness of the thin-film layer on the base (in micrometers)."]
    #[must_use]
    pub fn thin_film_thickness(&self) -> Option<f32> {
        self.read_value(
            "inputs:thin_film_thickness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:thin_film_thickness` at `time`.
    #[must_use]
    pub fn thin_film_thickness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:thin_film_thickness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:thin_film_thickness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn thin_film_thickness_default() -> f32 {
        0.5_f32
    }
    #[doc = "The authored `inputs:thin_film_weight` port, if it exists. Coverage weight of the thin-film. Use for materials such as multi-tone car paint or soap bubbles."]
    #[must_use]
    pub fn thin_film_weight_input(&self) -> Option<Port<'a>> {
        self.shader.input("thin_film_weight")
    }
    #[doc = "The composed `inputs:thin_film_weight`, without node defaults or connection evaluation. Coverage weight of the thin-film. Use for materials such as multi-tone car paint or soap bubbles."]
    #[must_use]
    pub fn thin_film_weight(&self) -> Option<f32> {
        self.read_value(
            "inputs:thin_film_weight",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:thin_film_weight` at `time`.
    #[must_use]
    pub fn thin_film_weight_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:thin_film_weight",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:thin_film_weight` default in the node definition, separate from authored values.
    #[must_use]
    pub fn thin_film_weight_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission_color` port, if it exists. Controls color of the transparent base due to Beer's law volumetric absorption under the surface (reverts to a non-physical tint when transmission_depth is zero)."]
    #[must_use]
    pub fn transmission_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_color")
    }
    #[doc = "The composed `inputs:transmission_color`, without node defaults or connection evaluation. Controls color of the transparent base due to Beer's law volumetric absorption under the surface (reverts to a non-physical tint when transmission_depth is zero)."]
    #[must_use]
    pub fn transmission_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:transmission_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:transmission_color` at `time`.
    #[must_use]
    pub fn transmission_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:transmission_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:transmission_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:transmission_depth` port, if it exists. Specifies the distance light travels inside the transparent base before it becomes exactly the transmission_color according to Beer's law."]
    #[must_use]
    pub fn transmission_depth_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_depth")
    }
    #[doc = "The composed `inputs:transmission_depth`, without node defaults or connection evaluation. Specifies the distance light travels inside the transparent base before it becomes exactly the transmission_color according to Beer's law."]
    #[must_use]
    pub fn transmission_depth(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_depth",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_depth` at `time`.
    #[must_use]
    pub fn transmission_depth_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_depth",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_depth` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_depth_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission_dispersion_abbe_number` port, if it exists. Physical Abbe number of the dielectric medium, describing how much the dielectric index of refraction varies across wavelengths."]
    #[must_use]
    pub fn transmission_dispersion_abbe_number_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_dispersion_abbe_number")
    }
    #[doc = "The composed `inputs:transmission_dispersion_abbe_number`, without node defaults or connection evaluation. Physical Abbe number of the dielectric medium, describing how much the dielectric index of refraction varies across wavelengths."]
    #[must_use]
    pub fn transmission_dispersion_abbe_number(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_dispersion_abbe_number",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_dispersion_abbe_number` at `time`.
    #[must_use]
    pub fn transmission_dispersion_abbe_number_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_dispersion_abbe_number",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_dispersion_abbe_number` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_dispersion_abbe_number_default() -> f32 {
        20.0_f32
    }
    #[doc = "The authored `inputs:transmission_dispersion_scale` port, if it exists. Linearly scales the amount of dispersion."]
    #[must_use]
    pub fn transmission_dispersion_scale_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_dispersion_scale")
    }
    #[doc = "The composed `inputs:transmission_dispersion_scale`, without node defaults or connection evaluation. Linearly scales the amount of dispersion."]
    #[must_use]
    pub fn transmission_dispersion_scale(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_dispersion_scale",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_dispersion_scale` at `time`.
    #[must_use]
    pub fn transmission_dispersion_scale_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_dispersion_scale",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_dispersion_scale` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_dispersion_scale_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission_scatter` port, if it exists. Controls the color of light volumetrically scattered inside the transparent base. Suitable for materials with visually significant scattering such as honey, fruit juice, murky water, opalescent glass, or milky glass."]
    #[must_use]
    pub fn transmission_scatter_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_scatter")
    }
    #[doc = "The composed `inputs:transmission_scatter`, without node defaults or connection evaluation. Controls the color of light volumetrically scattered inside the transparent base. Suitable for materials with visually significant scattering such as honey, fruit juice, murky water, opalescent glass, or milky glass."]
    #[must_use]
    pub fn transmission_scatter(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:transmission_scatter",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:transmission_scatter` at `time`.
    #[must_use]
    pub fn transmission_scatter_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:transmission_scatter",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:transmission_scatter` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_scatter_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:transmission_scatter_anisotropy` port, if it exists. The amount of directional bias, or anisotropy, of the volumetric scattering in the transparent base."]
    #[must_use]
    pub fn transmission_scatter_anisotropy_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_scatter_anisotropy")
    }
    #[doc = "The composed `inputs:transmission_scatter_anisotropy`, without node defaults or connection evaluation. The amount of directional bias, or anisotropy, of the volumetric scattering in the transparent base."]
    #[must_use]
    pub fn transmission_scatter_anisotropy(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_scatter_anisotropy",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_scatter_anisotropy` at `time`.
    #[must_use]
    pub fn transmission_scatter_anisotropy_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_scatter_anisotropy",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_scatter_anisotropy` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_scatter_anisotropy_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission_weight` port, if it exists. Mixture weight between the transparent and opaque dielectric base. The greater the value the more transparent the material."]
    #[must_use]
    pub fn transmission_weight_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_weight")
    }
    #[doc = "The composed `inputs:transmission_weight`, without node defaults or connection evaluation. Mixture weight between the transparent and opaque dielectric base. The greater the value the more transparent the material."]
    #[must_use]
    pub fn transmission_weight(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_weight",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_weight` at `time`.
    #[must_use]
    pub fn transmission_weight_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_weight",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_weight` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_weight_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:surface` port, if it exists. "]
    #[must_use]
    pub fn surface_output(&self) -> Option<Port<'a>> {
        self.shader.output("surface")
    }
}

/// Authors `ND_open_pbr_surface_surfaceshader` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdOpenPbrSurfaceSurfaceshaderEdit {
    shader: ShaderEdit,
}
impl Deref for NdOpenPbrSurfaceSurfaceshaderEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdOpenPbrSurfaceSurfaceshaderEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:base_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_base_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "base_color", ty)
    }
    /// Authors `inputs:base_color` at default time, creating the typed input if needed.
    pub fn set_base_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_base_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:base_color` sample in stage time through the edit target.
    pub fn set_base_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_base_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:base_diffuse_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_base_diffuse_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "base_diffuse_roughness", ty)
    }
    /// Authors `inputs:base_diffuse_roughness` at default time, creating the typed input if needed.
    pub fn set_base_diffuse_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_base_diffuse_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:base_diffuse_roughness` sample in stage time through the edit target.
    pub fn set_base_diffuse_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_base_diffuse_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:base_metalness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_base_metalness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "base_metalness", ty)
    }
    /// Authors `inputs:base_metalness` at default time, creating the typed input if needed.
    pub fn set_base_metalness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_base_metalness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:base_metalness` sample in stage time through the edit target.
    pub fn set_base_metalness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_base_metalness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:base_weight` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_base_weight_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "base_weight", ty)
    }
    /// Authors `inputs:base_weight` at default time, creating the typed input if needed.
    pub fn set_base_weight(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_base_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:base_weight` sample in stage time through the edit target.
    pub fn set_base_weight_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_base_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "coat_color", ty)
    }
    /// Authors `inputs:coat_color` at default time, creating the typed input if needed.
    pub fn set_coat_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_color` sample in stage time through the edit target.
    pub fn set_coat_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_darkening` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_darkening_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_darkening", ty)
    }
    /// Authors `inputs:coat_darkening` at default time, creating the typed input if needed.
    pub fn set_coat_darkening(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_darkening_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_darkening` sample in stage time through the edit target.
    pub fn set_coat_darkening_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_darkening_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_ior` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_ior_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_ior", ty)
    }
    /// Authors `inputs:coat_ior` at default time, creating the typed input if needed.
    pub fn set_coat_ior(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_coat_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_ior` sample in stage time through the edit target.
    pub fn set_coat_ior_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_roughness", ty)
    }
    /// Authors `inputs:coat_roughness` at default time, creating the typed input if needed.
    pub fn set_coat_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_roughness` sample in stage time through the edit target.
    pub fn set_coat_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_roughness_anisotropy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_roughness_anisotropy_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "coat_roughness_anisotropy", ty)
    }
    /// Authors `inputs:coat_roughness_anisotropy` at default time, creating the typed input if needed.
    pub fn set_coat_roughness_anisotropy(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_roughness_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_roughness_anisotropy` sample in stage time through the edit target.
    pub fn set_coat_roughness_anisotropy_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_roughness_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_weight` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_weight_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_weight", ty)
    }
    /// Authors `inputs:coat_weight` at default time, creating the typed input if needed.
    pub fn set_coat_weight(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_weight` sample in stage time through the edit target.
    pub fn set_coat_weight_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:emission_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_emission_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "emission_color", ty)
    }
    /// Authors `inputs:emission_color` at default time, creating the typed input if needed.
    pub fn set_emission_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_emission_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:emission_color` sample in stage time through the edit target.
    pub fn set_emission_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_emission_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:emission_luminance` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_emission_luminance_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "emission_luminance", ty)
    }
    /// Authors `inputs:emission_luminance` at default time, creating the typed input if needed.
    pub fn set_emission_luminance(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_emission_luminance_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:emission_luminance` sample in stage time through the edit target.
    pub fn set_emission_luminance_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_emission_luminance_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:fuzz_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_fuzz_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "fuzz_color", ty)
    }
    /// Authors `inputs:fuzz_color` at default time, creating the typed input if needed.
    pub fn set_fuzz_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_fuzz_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:fuzz_color` sample in stage time through the edit target.
    pub fn set_fuzz_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_fuzz_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:fuzz_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_fuzz_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "fuzz_roughness", ty)
    }
    /// Authors `inputs:fuzz_roughness` at default time, creating the typed input if needed.
    pub fn set_fuzz_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_fuzz_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:fuzz_roughness` sample in stage time through the edit target.
    pub fn set_fuzz_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_fuzz_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:fuzz_weight` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_fuzz_weight_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "fuzz_weight", ty)
    }
    /// Authors `inputs:fuzz_weight` at default time, creating the typed input if needed.
    pub fn set_fuzz_weight(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_fuzz_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:fuzz_weight` sample in stage time through the edit target.
    pub fn set_fuzz_weight_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_fuzz_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:geometry_coat_normal` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_geometry_coat_normal_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "geometry_coat_normal", ty)
    }
    /// Authors `inputs:geometry_coat_normal` at default time, creating the typed input if needed.
    pub fn set_geometry_coat_normal(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_coat_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:geometry_coat_normal` sample in stage time through the edit target.
    pub fn set_geometry_coat_normal_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_coat_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:geometry_coat_tangent` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_geometry_coat_tangent_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "geometry_coat_tangent", ty)
    }
    /// Authors `inputs:geometry_coat_tangent` at default time, creating the typed input if needed.
    pub fn set_geometry_coat_tangent(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_coat_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:geometry_coat_tangent` sample in stage time through the edit target.
    pub fn set_geometry_coat_tangent_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_coat_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:geometry_normal` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_geometry_normal_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "geometry_normal", ty)
    }
    /// Authors `inputs:geometry_normal` at default time, creating the typed input if needed.
    pub fn set_geometry_normal(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:geometry_normal` sample in stage time through the edit target.
    pub fn set_geometry_normal_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:geometry_opacity` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_geometry_opacity_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "geometry_opacity", ty)
    }
    /// Authors `inputs:geometry_opacity` at default time, creating the typed input if needed.
    pub fn set_geometry_opacity(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_opacity_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:geometry_opacity` sample in stage time through the edit target.
    pub fn set_geometry_opacity_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_opacity_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:geometry_tangent` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_geometry_tangent_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "geometry_tangent", ty)
    }
    /// Authors `inputs:geometry_tangent` at default time, creating the typed input if needed.
    pub fn set_geometry_tangent(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:geometry_tangent` sample in stage time through the edit target.
    pub fn set_geometry_tangent_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:geometry_thin_walled` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_geometry_thin_walled_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("bool").expect("a registered value type");
        self.shader.create_input(edit, "geometry_thin_walled", ty)
    }
    /// Authors `inputs:geometry_thin_walled` at default time, creating the typed input if needed.
    pub fn set_geometry_thin_walled(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: bool,
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_thin_walled_input(edit)?;
        let value = (layerstack_schemas::value::write_bool)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:geometry_thin_walled` sample in stage time through the edit target.
    pub fn set_geometry_thin_walled_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: bool,
    ) -> Result<&Self, PortError> {
        let port = self.create_geometry_thin_walled_input(edit)?;
        let value = (layerstack_schemas::value::write_bool)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "specular_color", ty)
    }
    /// Authors `inputs:specular_color` at default time, creating the typed input if needed.
    pub fn set_specular_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_color` sample in stage time through the edit target.
    pub fn set_specular_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_ior` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_ior_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular_ior", ty)
    }
    /// Authors `inputs:specular_ior` at default time, creating the typed input if needed.
    pub fn set_specular_ior(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_ior` sample in stage time through the edit target.
    pub fn set_specular_ior_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular_roughness", ty)
    }
    /// Authors `inputs:specular_roughness` at default time, creating the typed input if needed.
    pub fn set_specular_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_roughness` sample in stage time through the edit target.
    pub fn set_specular_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_roughness_anisotropy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_roughness_anisotropy_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "specular_roughness_anisotropy", ty)
    }
    /// Authors `inputs:specular_roughness_anisotropy` at default time, creating the typed input if needed.
    pub fn set_specular_roughness_anisotropy(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_roughness_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_roughness_anisotropy` sample in stage time through the edit target.
    pub fn set_specular_roughness_anisotropy_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_roughness_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_weight` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_weight_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular_weight", ty)
    }
    /// Authors `inputs:specular_weight` at default time, creating the typed input if needed.
    pub fn set_specular_weight(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_weight` sample in stage time through the edit target.
    pub fn set_specular_weight_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "subsurface_color", ty)
    }
    /// Authors `inputs:subsurface_color` at default time, creating the typed input if needed.
    pub fn set_subsurface_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_color` sample in stage time through the edit target.
    pub fn set_subsurface_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_radius` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_radius_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "subsurface_radius", ty)
    }
    /// Authors `inputs:subsurface_radius` at default time, creating the typed input if needed.
    pub fn set_subsurface_radius(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_radius_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_radius` sample in stage time through the edit target.
    pub fn set_subsurface_radius_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_radius_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_radius_scale` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_radius_scale_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader
            .create_input(edit, "subsurface_radius_scale", ty)
    }
    /// Authors `inputs:subsurface_radius_scale` at default time, creating the typed input if needed.
    pub fn set_subsurface_radius_scale(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_radius_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_radius_scale` sample in stage time through the edit target.
    pub fn set_subsurface_radius_scale_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_radius_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_scatter_anisotropy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_scatter_anisotropy_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "subsurface_scatter_anisotropy", ty)
    }
    /// Authors `inputs:subsurface_scatter_anisotropy` at default time, creating the typed input if needed.
    pub fn set_subsurface_scatter_anisotropy(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_scatter_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_scatter_anisotropy` sample in stage time through the edit target.
    pub fn set_subsurface_scatter_anisotropy_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_scatter_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_weight` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_weight_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "subsurface_weight", ty)
    }
    /// Authors `inputs:subsurface_weight` at default time, creating the typed input if needed.
    pub fn set_subsurface_weight(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_weight` sample in stage time through the edit target.
    pub fn set_subsurface_weight_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:thin_film_ior` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_thin_film_ior_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "thin_film_ior", ty)
    }
    /// Authors `inputs:thin_film_ior` at default time, creating the typed input if needed.
    pub fn set_thin_film_ior(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:thin_film_ior` sample in stage time through the edit target.
    pub fn set_thin_film_ior_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:thin_film_thickness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_thin_film_thickness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "thin_film_thickness", ty)
    }
    /// Authors `inputs:thin_film_thickness` at default time, creating the typed input if needed.
    pub fn set_thin_film_thickness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_thickness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:thin_film_thickness` sample in stage time through the edit target.
    pub fn set_thin_film_thickness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_thickness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:thin_film_weight` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_thin_film_weight_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "thin_film_weight", ty)
    }
    /// Authors `inputs:thin_film_weight` at default time, creating the typed input if needed.
    pub fn set_thin_film_weight(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:thin_film_weight` sample in stage time through the edit target.
    pub fn set_thin_film_weight_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "transmission_color", ty)
    }
    /// Authors `inputs:transmission_color` at default time, creating the typed input if needed.
    pub fn set_transmission_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_color` sample in stage time through the edit target.
    pub fn set_transmission_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_depth` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_depth_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "transmission_depth", ty)
    }
    /// Authors `inputs:transmission_depth` at default time, creating the typed input if needed.
    pub fn set_transmission_depth(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_depth_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_depth` sample in stage time through the edit target.
    pub fn set_transmission_depth_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_depth_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_dispersion_abbe_number` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_dispersion_abbe_number_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "transmission_dispersion_abbe_number", ty)
    }
    /// Authors `inputs:transmission_dispersion_abbe_number` at default time, creating the typed input if needed.
    pub fn set_transmission_dispersion_abbe_number(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_dispersion_abbe_number_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_dispersion_abbe_number` sample in stage time through the edit target.
    pub fn set_transmission_dispersion_abbe_number_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_dispersion_abbe_number_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_dispersion_scale` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_dispersion_scale_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "transmission_dispersion_scale", ty)
    }
    /// Authors `inputs:transmission_dispersion_scale` at default time, creating the typed input if needed.
    pub fn set_transmission_dispersion_scale(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_dispersion_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_dispersion_scale` sample in stage time through the edit target.
    pub fn set_transmission_dispersion_scale_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_dispersion_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_scatter` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_scatter_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "transmission_scatter", ty)
    }
    /// Authors `inputs:transmission_scatter` at default time, creating the typed input if needed.
    pub fn set_transmission_scatter(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_scatter_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_scatter` sample in stage time through the edit target.
    pub fn set_transmission_scatter_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_scatter_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_scatter_anisotropy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_scatter_anisotropy_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "transmission_scatter_anisotropy", ty)
    }
    /// Authors `inputs:transmission_scatter_anisotropy` at default time, creating the typed input if needed.
    pub fn set_transmission_scatter_anisotropy(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_scatter_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_scatter_anisotropy` sample in stage time through the edit target.
    pub fn set_transmission_scatter_anisotropy_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_scatter_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_weight` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_weight_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "transmission_weight", ty)
    }
    /// Authors `inputs:transmission_weight` at default time, creating the typed input if needed.
    pub fn set_transmission_weight(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_weight` sample in stage time through the edit target.
    pub fn set_transmission_weight_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_weight_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:surface` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_surface_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("token").expect("a registered value type");
        self.shader.create_output(edit, "surface", ty)
    }
}

/// The `ND_place2d_vector2` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdPlace2dVector2<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdPlace2dVector2<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdPlace2dVector2<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_place2d_vector2";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdPlace2dVector2Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdPlace2dVector2Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "offset",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:offset",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0,0.0",
                )),
            },
            MaterialXPort {
                name: "operationorder",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:operationorder",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "pivot",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:pivot",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0,0.0",
                )),
            },
            MaterialXPort {
                name: "rotate",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:rotate",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "scale",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:scale",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0,1.0",
                )),
            },
            MaterialXPort {
                name: "texcoord",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:texcoord",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "vector2",
                usd_name: "outputs:out",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "texcoord",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:offset` port, if it exists. "]
    #[must_use]
    pub fn offset_input(&self) -> Option<Port<'a>> {
        self.shader.input("offset")
    }
    #[doc = "The composed `inputs:offset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn offset(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:offset", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:offset` at `time`.
    #[must_use]
    pub fn offset_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:offset",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:offset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn offset_default() -> [f32; 2] {
        [0.0, 0.0]
    }
    #[doc = "The authored `inputs:operationorder` port, if it exists. "]
    #[must_use]
    pub fn operationorder_input(&self) -> Option<Port<'a>> {
        self.shader.input("operationorder")
    }
    #[doc = "The composed `inputs:operationorder`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn operationorder(&self) -> Option<i32> {
        self.read_value("inputs:operationorder", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:operationorder` at `time`.
    #[must_use]
    pub fn operationorder_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:operationorder",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:operationorder` default in the node definition, separate from authored values.
    #[must_use]
    pub fn operationorder_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:pivot` port, if it exists. "]
    #[must_use]
    pub fn pivot_input(&self) -> Option<Port<'a>> {
        self.shader.input("pivot")
    }
    #[doc = "The composed `inputs:pivot`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn pivot(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:pivot", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:pivot` at `time`.
    #[must_use]
    pub fn pivot_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at("inputs:pivot", time, layerstack_schemas::value::read_float2)
    }
    /// The `inputs:pivot` default in the node definition, separate from authored values.
    #[must_use]
    pub fn pivot_default() -> [f32; 2] {
        [0.0, 0.0]
    }
    #[doc = "The authored `inputs:rotate` port, if it exists. "]
    #[must_use]
    pub fn rotate_input(&self) -> Option<Port<'a>> {
        self.shader.input("rotate")
    }
    #[doc = "The composed `inputs:rotate`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn rotate(&self) -> Option<f32> {
        self.read_value("inputs:rotate", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:rotate` at `time`.
    #[must_use]
    pub fn rotate_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:rotate", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:rotate` default in the node definition, separate from authored values.
    #[must_use]
    pub fn rotate_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:scale` port, if it exists. "]
    #[must_use]
    pub fn scale_input(&self) -> Option<Port<'a>> {
        self.shader.input("scale")
    }
    #[doc = "The composed `inputs:scale`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn scale(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:scale", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:scale` at `time`.
    #[must_use]
    pub fn scale_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at("inputs:scale", time, layerstack_schemas::value::read_float2)
    }
    /// The `inputs:scale` default in the node definition, separate from authored values.
    #[must_use]
    pub fn scale_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `inputs:texcoord` port, if it exists. "]
    #[must_use]
    pub fn texcoord_input(&self) -> Option<Port<'a>> {
        self.shader.input("texcoord")
    }
    #[doc = "The composed `inputs:texcoord`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn texcoord(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:texcoord", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:texcoord` at `time`.
    #[must_use]
    pub fn texcoord_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:texcoord",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:texcoord` default in the node definition, separate from authored values.
    #[must_use]
    pub fn texcoord_default() -> [f32; 2] {
        [0.0, 0.0]
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_place2d_vector2` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdPlace2dVector2Edit {
    shader: ShaderEdit,
}
impl Deref for NdPlace2dVector2Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdPlace2dVector2Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:offset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_offset_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "offset", ty)
    }
    /// Authors `inputs:offset` at default time, creating the typed input if needed.
    pub fn set_offset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_offset_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:offset` sample in stage time through the edit target.
    pub fn set_offset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_offset_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:operationorder` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_operationorder_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "operationorder", ty)
    }
    /// Authors `inputs:operationorder` at default time, creating the typed input if needed.
    pub fn set_operationorder(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_operationorder_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:operationorder` sample in stage time through the edit target.
    pub fn set_operationorder_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_operationorder_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:pivot` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_pivot_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "pivot", ty)
    }
    /// Authors `inputs:pivot` at default time, creating the typed input if needed.
    pub fn set_pivot(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_pivot_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:pivot` sample in stage time through the edit target.
    pub fn set_pivot_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_pivot_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:rotate` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_rotate_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "rotate", ty)
    }
    /// Authors `inputs:rotate` at default time, creating the typed input if needed.
    pub fn set_rotate(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_rotate_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:rotate` sample in stage time through the edit target.
    pub fn set_rotate_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_rotate_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:scale` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_scale_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "scale", ty)
    }
    /// Authors `inputs:scale` at default time, creating the typed input if needed.
    pub fn set_scale(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:scale` sample in stage time through the edit target.
    pub fn set_scale_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:texcoord` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_texcoord_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "texcoord", ty)
    }
    /// Authors `inputs:texcoord` at default time, creating the typed input if needed.
    pub fn set_texcoord(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:texcoord` sample in stage time through the edit target.
    pub fn set_texcoord_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_remap_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdRemapFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdRemapFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdRemapFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_remap_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<NdRemapFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdRemapFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:in",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "inhigh",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:inhigh",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "inlow",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:inlow",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "outhigh",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:outhigh",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "outlow",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:outlow",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Input {
                    name: "in",
                    fallback: None,
                }),
            },
        ],
    };
    #[doc = "The authored `inputs:in` port, if it exists. "]
    #[must_use]
    pub fn in_value_input(&self) -> Option<Port<'a>> {
        self.shader.input("in")
    }
    #[doc = "The composed `inputs:in`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in_value(&self) -> Option<f32> {
        self.read_value("inputs:in", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:in` at `time`.
    #[must_use]
    pub fn in_value_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:in", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:in` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in_value_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:inhigh` port, if it exists. "]
    #[must_use]
    pub fn inhigh_input(&self) -> Option<Port<'a>> {
        self.shader.input("inhigh")
    }
    #[doc = "The composed `inputs:inhigh`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn inhigh(&self) -> Option<f32> {
        self.read_value("inputs:inhigh", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:inhigh` at `time`.
    #[must_use]
    pub fn inhigh_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:inhigh", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:inhigh` default in the node definition, separate from authored values.
    #[must_use]
    pub fn inhigh_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:inlow` port, if it exists. "]
    #[must_use]
    pub fn inlow_input(&self) -> Option<Port<'a>> {
        self.shader.input("inlow")
    }
    #[doc = "The composed `inputs:inlow`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn inlow(&self) -> Option<f32> {
        self.read_value("inputs:inlow", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:inlow` at `time`.
    #[must_use]
    pub fn inlow_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:inlow", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:inlow` default in the node definition, separate from authored values.
    #[must_use]
    pub fn inlow_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:outhigh` port, if it exists. "]
    #[must_use]
    pub fn outhigh_input(&self) -> Option<Port<'a>> {
        self.shader.input("outhigh")
    }
    #[doc = "The composed `inputs:outhigh`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn outhigh(&self) -> Option<f32> {
        self.read_value("inputs:outhigh", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:outhigh` at `time`.
    #[must_use]
    pub fn outhigh_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:outhigh",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:outhigh` default in the node definition, separate from authored values.
    #[must_use]
    pub fn outhigh_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:outlow` port, if it exists. "]
    #[must_use]
    pub fn outlow_input(&self) -> Option<Port<'a>> {
        self.shader.input("outlow")
    }
    #[doc = "The composed `inputs:outlow`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn outlow(&self) -> Option<f32> {
        self.read_value("inputs:outlow", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:outlow` at `time`.
    #[must_use]
    pub fn outlow_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:outlow", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:outlow` default in the node definition, separate from authored values.
    #[must_use]
    pub fn outlow_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_remap_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdRemapFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdRemapFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdRemapFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in_value_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "in", ty)
    }
    /// Authors `inputs:in` at default time, creating the typed input if needed.
    pub fn set_in_value(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in` sample in stage time through the edit target.
    pub fn set_in_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:inhigh` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_inhigh_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "inhigh", ty)
    }
    /// Authors `inputs:inhigh` at default time, creating the typed input if needed.
    pub fn set_inhigh(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_inhigh_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:inhigh` sample in stage time through the edit target.
    pub fn set_inhigh_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_inhigh_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:inlow` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_inlow_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "inlow", ty)
    }
    /// Authors `inputs:inlow` at default time, creating the typed input if needed.
    pub fn set_inlow(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_inlow_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:inlow` sample in stage time through the edit target.
    pub fn set_inlow_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_inlow_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:outhigh` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_outhigh_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "outhigh", ty)
    }
    /// Authors `inputs:outhigh` at default time, creating the typed input if needed.
    pub fn set_outhigh(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_outhigh_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:outhigh` sample in stage time through the edit target.
    pub fn set_outhigh_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_outhigh_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:outlow` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_outlow_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "outlow", ty)
    }
    /// Authors `inputs:outlow` at default time, creating the typed input if needed.
    pub fn set_outlow(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_outlow_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:outlow` sample in stage time through the edit target.
    pub fn set_outlow_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_outlow_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_separate3_vector3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdSeparate3Vector3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdSeparate3Vector3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdSeparate3Vector3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_separate3_vector3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdSeparate3Vector3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdSeparate3Vector3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "in",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:in",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "outx",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:outx",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "outy",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:outy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "outz",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:outz",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:in` port, if it exists. "]
    #[must_use]
    pub fn in_value_input(&self) -> Option<Port<'a>> {
        self.shader.input("in")
    }
    #[doc = "The composed `inputs:in`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn in_value(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:in", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:in` at `time`.
    #[must_use]
    pub fn in_value_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at("inputs:in", time, layerstack_schemas::value::read_float3)
    }
    /// The `inputs:in` default in the node definition, separate from authored values.
    #[must_use]
    pub fn in_value_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `outputs:outx` port, if it exists. "]
    #[must_use]
    pub fn outx_output(&self) -> Option<Port<'a>> {
        self.shader.output("outx")
    }
    #[doc = "The authored `outputs:outy` port, if it exists. "]
    #[must_use]
    pub fn outy_output(&self) -> Option<Port<'a>> {
        self.shader.output("outy")
    }
    #[doc = "The authored `outputs:outz` port, if it exists. "]
    #[must_use]
    pub fn outz_output(&self) -> Option<Port<'a>> {
        self.shader.output("outz")
    }
}

/// Authors `ND_separate3_vector3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdSeparate3Vector3Edit {
    shader: ShaderEdit,
}
impl Deref for NdSeparate3Vector3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdSeparate3Vector3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:in` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_in_value_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "in", ty)
    }
    /// Authors `inputs:in` at default time, creating the typed input if needed.
    pub fn set_in_value(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:in` sample in stage time through the edit target.
    pub fn set_in_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_in_value_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:outx` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_outx_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "outx", ty)
    }
    /// Creates `outputs:outy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_outy_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "outy", ty)
    }
    /// Creates `outputs:outz` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_outz_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "outz", ty)
    }
}

/// The `ND_standard_surface_surfaceshader` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdStandardSurfaceSurfaceshader<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdStandardSurfaceSurfaceshader<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdStandardSurfaceSurfaceshader<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_standard_surface_surfaceshader";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdStandardSurfaceSurfaceshaderEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdStandardSurfaceSurfaceshaderEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "base",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:base",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0",
                )),
            },
            MaterialXPort {
                name: "base_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:base_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.8, 0.8, 0.8",
                )),
            },
            MaterialXPort {
                name: "coat",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "coat_IOR",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_IOR",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.5",
                )),
            },
            MaterialXPort {
                name: "coat_affect_color",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_affect_color",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "coat_affect_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_affect_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "coat_anisotropy",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_anisotropy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "coat_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:coat_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "coat_normal",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:coat_normal",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Nworld",
                )),
            },
            MaterialXPort {
                name: "coat_rotation",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_rotation",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "coat_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:coat_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.1",
                )),
            },
            MaterialXPort {
                name: "diffuse_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:diffuse_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "emission",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:emission",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "emission_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:emission_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "metalness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:metalness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "normal",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:normal",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Nworld",
                )),
            },
            MaterialXPort {
                name: "opacity",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:opacity",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "sheen",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:sheen",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "sheen_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:sheen_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "sheen_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:sheen_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.3",
                )),
            },
            MaterialXPort {
                name: "specular",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "specular_IOR",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular_IOR",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.5",
                )),
            },
            MaterialXPort {
                name: "specular_anisotropy",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular_anisotropy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "specular_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:specular_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "specular_rotation",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular_rotation",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "specular_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:specular_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.2",
                )),
            },
            MaterialXPort {
                name: "subsurface",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:subsurface",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "subsurface_anisotropy",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:subsurface_anisotropy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "subsurface_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:subsurface_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "subsurface_radius",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:subsurface_radius",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "subsurface_scale",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:subsurface_scale",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1",
                )),
            },
            MaterialXPort {
                name: "tangent",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:tangent",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "Tworld",
                )),
            },
            MaterialXPort {
                name: "thin_film_IOR",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:thin_film_IOR",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.5",
                )),
            },
            MaterialXPort {
                name: "thin_film_thickness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:thin_film_thickness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "thin_walled",
                direction: PortKind::Input,
                type_name: "boolean",
                usd_name: "inputs:thin_walled",
                usd_type_name: "bool",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "false",
                )),
            },
            MaterialXPort {
                name: "transmission",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "transmission_color",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:transmission_color",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1, 1, 1",
                )),
            },
            MaterialXPort {
                name: "transmission_depth",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_depth",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "transmission_dispersion",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_dispersion",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "transmission_extra_roughness",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_extra_roughness",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "transmission_scatter",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:transmission_scatter",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0, 0, 0",
                )),
            },
            MaterialXPort {
                name: "transmission_scatter_anisotropy",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:transmission_scatter_anisotropy",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "surfaceshader",
                usd_name: "outputs:surface",
                usd_type_name: "token",
                default: None,
            },
        ],
    };
    #[doc = "The authored `inputs:base` port, if it exists. Multiplier on the intensity of the diffuse reflection."]
    #[must_use]
    pub fn base_input(&self) -> Option<Port<'a>> {
        self.shader.input("base")
    }
    #[doc = "The composed `inputs:base`, without node defaults or connection evaluation. Multiplier on the intensity of the diffuse reflection."]
    #[must_use]
    pub fn base(&self) -> Option<f32> {
        self.read_value("inputs:base", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:base` at `time`.
    #[must_use]
    pub fn base_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:base", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:base` default in the node definition, separate from authored values.
    #[must_use]
    pub fn base_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:base_color` port, if it exists. Color of the diffuse reflection."]
    #[must_use]
    pub fn base_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("base_color")
    }
    #[doc = "The composed `inputs:base_color`, without node defaults or connection evaluation. Color of the diffuse reflection."]
    #[must_use]
    pub fn base_color(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:base_color", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:base_color` at `time`.
    #[must_use]
    pub fn base_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:base_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:base_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn base_color_default() -> [f32; 3] {
        [0.8, 0.8, 0.8]
    }
    #[doc = "The authored `inputs:coat` port, if it exists. The weight of a reflective clear-coat layer on top of the material. Use for materials such as car paint or an oily layer."]
    #[must_use]
    pub fn coat_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat")
    }
    #[doc = "The composed `inputs:coat`, without node defaults or connection evaluation. The weight of a reflective clear-coat layer on top of the material. Use for materials such as car paint or an oily layer."]
    #[must_use]
    pub fn coat(&self) -> Option<f32> {
        self.read_value("inputs:coat", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:coat` at `time`.
    #[must_use]
    pub fn coat_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:coat", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:coat` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:coat_IOR` port, if it exists. The index of refraction of the clear-coat layer."]
    #[must_use]
    pub fn coat_ior_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_IOR")
    }
    #[doc = "The composed `inputs:coat_IOR`, without node defaults or connection evaluation. The index of refraction of the clear-coat layer."]
    #[must_use]
    pub fn coat_ior(&self) -> Option<f32> {
        self.read_value("inputs:coat_IOR", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:coat_IOR` at `time`.
    #[must_use]
    pub fn coat_ior_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_IOR",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_IOR` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_ior_default() -> f32 {
        1.5_f32
    }
    #[doc = "The authored `inputs:coat_affect_color` port, if it exists. Controls the saturation of diffuse reflection and subsurface scattering below the clear-coat."]
    #[must_use]
    pub fn coat_affect_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_affect_color")
    }
    #[doc = "The composed `inputs:coat_affect_color`, without node defaults or connection evaluation. Controls the saturation of diffuse reflection and subsurface scattering below the clear-coat."]
    #[must_use]
    pub fn coat_affect_color(&self) -> Option<f32> {
        self.read_value(
            "inputs:coat_affect_color",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:coat_affect_color` at `time`.
    #[must_use]
    pub fn coat_affect_color_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_affect_color",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_affect_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_affect_color_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:coat_affect_roughness` port, if it exists. Controls the roughness of the specular reflection in the layers below the clear-coat."]
    #[must_use]
    pub fn coat_affect_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_affect_roughness")
    }
    #[doc = "The composed `inputs:coat_affect_roughness`, without node defaults or connection evaluation. Controls the roughness of the specular reflection in the layers below the clear-coat."]
    #[must_use]
    pub fn coat_affect_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:coat_affect_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:coat_affect_roughness` at `time`.
    #[must_use]
    pub fn coat_affect_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_affect_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_affect_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_affect_roughness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:coat_anisotropy` port, if it exists. The amount of directional bias, or anisotropy, of the clear-coat layer."]
    #[must_use]
    pub fn coat_anisotropy_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_anisotropy")
    }
    #[doc = "The composed `inputs:coat_anisotropy`, without node defaults or connection evaluation. The amount of directional bias, or anisotropy, of the clear-coat layer."]
    #[must_use]
    pub fn coat_anisotropy(&self) -> Option<f32> {
        self.read_value(
            "inputs:coat_anisotropy",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:coat_anisotropy` at `time`.
    #[must_use]
    pub fn coat_anisotropy_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_anisotropy",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_anisotropy` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_anisotropy_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:coat_color` port, if it exists. The color of the clear-coat layer's transparency."]
    #[must_use]
    pub fn coat_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_color")
    }
    #[doc = "The composed `inputs:coat_color`, without node defaults or connection evaluation. The color of the clear-coat layer's transparency."]
    #[must_use]
    pub fn coat_color(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:coat_color", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:coat_color` at `time`.
    #[must_use]
    pub fn coat_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:coat_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:coat_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:coat_normal` port, if it exists. Input normal for clear-coat layer"]
    #[must_use]
    pub fn coat_normal_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_normal")
    }
    #[doc = "The composed `inputs:coat_normal`, without node defaults or connection evaluation. Input normal for clear-coat layer"]
    #[must_use]
    pub fn coat_normal(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:coat_normal", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:coat_normal` at `time`.
    #[must_use]
    pub fn coat_normal_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:coat_normal",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:coat_rotation` port, if it exists. The rotation of the anisotropic effect of the clear-coat layer."]
    #[must_use]
    pub fn coat_rotation_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_rotation")
    }
    #[doc = "The composed `inputs:coat_rotation`, without node defaults or connection evaluation. The rotation of the anisotropic effect of the clear-coat layer."]
    #[must_use]
    pub fn coat_rotation(&self) -> Option<f32> {
        self.read_value(
            "inputs:coat_rotation",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:coat_rotation` at `time`.
    #[must_use]
    pub fn coat_rotation_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_rotation",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_rotation` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_rotation_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:coat_roughness` port, if it exists. The roughness of the clear-coat reflections. The lower the value, the sharper the reflection."]
    #[must_use]
    pub fn coat_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("coat_roughness")
    }
    #[doc = "The composed `inputs:coat_roughness`, without node defaults or connection evaluation. The roughness of the clear-coat reflections. The lower the value, the sharper the reflection."]
    #[must_use]
    pub fn coat_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:coat_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:coat_roughness` at `time`.
    #[must_use]
    pub fn coat_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:coat_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:coat_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn coat_roughness_default() -> f32 {
        0.1_f32
    }
    #[doc = "The authored `inputs:diffuse_roughness` port, if it exists. Roughness of the diffuse reflection. Higher values cause the surface to appear flatter and darker."]
    #[must_use]
    pub fn diffuse_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("diffuse_roughness")
    }
    #[doc = "The composed `inputs:diffuse_roughness`, without node defaults or connection evaluation. Roughness of the diffuse reflection. Higher values cause the surface to appear flatter and darker."]
    #[must_use]
    pub fn diffuse_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:diffuse_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:diffuse_roughness` at `time`.
    #[must_use]
    pub fn diffuse_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:diffuse_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:diffuse_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn diffuse_roughness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:emission` port, if it exists. The amount of emitted incandescent light."]
    #[must_use]
    pub fn emission_input(&self) -> Option<Port<'a>> {
        self.shader.input("emission")
    }
    #[doc = "The composed `inputs:emission`, without node defaults or connection evaluation. The amount of emitted incandescent light."]
    #[must_use]
    pub fn emission(&self) -> Option<f32> {
        self.read_value("inputs:emission", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:emission` at `time`.
    #[must_use]
    pub fn emission_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:emission",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:emission` default in the node definition, separate from authored values.
    #[must_use]
    pub fn emission_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:emission_color` port, if it exists. The color of the emitted light."]
    #[must_use]
    pub fn emission_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("emission_color")
    }
    #[doc = "The composed `inputs:emission_color`, without node defaults or connection evaluation. The color of the emitted light."]
    #[must_use]
    pub fn emission_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:emission_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:emission_color` at `time`.
    #[must_use]
    pub fn emission_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:emission_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:emission_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn emission_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:metalness` port, if it exists. Specifies how metallic the material appears. At its maximum, the surface behaves like a metal, using fully specular reflection and complex fresnel."]
    #[must_use]
    pub fn metalness_input(&self) -> Option<Port<'a>> {
        self.shader.input("metalness")
    }
    #[doc = "The composed `inputs:metalness`, without node defaults or connection evaluation. Specifies how metallic the material appears. At its maximum, the surface behaves like a metal, using fully specular reflection and complex fresnel."]
    #[must_use]
    pub fn metalness(&self) -> Option<f32> {
        self.read_value("inputs:metalness", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:metalness` at `time`.
    #[must_use]
    pub fn metalness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:metalness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:metalness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn metalness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:normal` port, if it exists. Input geometric normal"]
    #[must_use]
    pub fn normal_input(&self) -> Option<Port<'a>> {
        self.shader.input("normal")
    }
    #[doc = "The composed `inputs:normal`, without node defaults or connection evaluation. Input geometric normal"]
    #[must_use]
    pub fn normal(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:normal", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:normal` at `time`.
    #[must_use]
    pub fn normal_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:normal",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:opacity` port, if it exists. The opacity of the entire material."]
    #[must_use]
    pub fn opacity_input(&self) -> Option<Port<'a>> {
        self.shader.input("opacity")
    }
    #[doc = "The composed `inputs:opacity`, without node defaults or connection evaluation. The opacity of the entire material."]
    #[must_use]
    pub fn opacity(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:opacity", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:opacity` at `time`.
    #[must_use]
    pub fn opacity_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:opacity",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:opacity` default in the node definition, separate from authored values.
    #[must_use]
    pub fn opacity_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:sheen` port, if it exists. The weight of a sheen layer that can be used to approximate microfibers or fabrics such as velvet and satin."]
    #[must_use]
    pub fn sheen_input(&self) -> Option<Port<'a>> {
        self.shader.input("sheen")
    }
    #[doc = "The composed `inputs:sheen`, without node defaults or connection evaluation. The weight of a sheen layer that can be used to approximate microfibers or fabrics such as velvet and satin."]
    #[must_use]
    pub fn sheen(&self) -> Option<f32> {
        self.read_value("inputs:sheen", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:sheen` at `time`.
    #[must_use]
    pub fn sheen_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at("inputs:sheen", time, layerstack_schemas::value::read_float)
    }
    /// The `inputs:sheen` default in the node definition, separate from authored values.
    #[must_use]
    pub fn sheen_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:sheen_color` port, if it exists. The color of the sheen layer."]
    #[must_use]
    pub fn sheen_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("sheen_color")
    }
    #[doc = "The composed `inputs:sheen_color`, without node defaults or connection evaluation. The color of the sheen layer."]
    #[must_use]
    pub fn sheen_color(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:sheen_color", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:sheen_color` at `time`.
    #[must_use]
    pub fn sheen_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:sheen_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:sheen_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn sheen_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:sheen_roughness` port, if it exists. The roughness of the sheen layer."]
    #[must_use]
    pub fn sheen_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("sheen_roughness")
    }
    #[doc = "The composed `inputs:sheen_roughness`, without node defaults or connection evaluation. The roughness of the sheen layer."]
    #[must_use]
    pub fn sheen_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:sheen_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:sheen_roughness` at `time`.
    #[must_use]
    pub fn sheen_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:sheen_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:sheen_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn sheen_roughness_default() -> f32 {
        0.3_f32
    }
    #[doc = "The authored `inputs:specular` port, if it exists. Multiplier on the intensity of the specular reflection."]
    #[must_use]
    pub fn specular_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular")
    }
    #[doc = "The composed `inputs:specular`, without node defaults or connection evaluation. Multiplier on the intensity of the specular reflection."]
    #[must_use]
    pub fn specular(&self) -> Option<f32> {
        self.read_value("inputs:specular", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:specular` at `time`.
    #[must_use]
    pub fn specular_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:specular_IOR` port, if it exists. Index of refraction for specular reflection."]
    #[must_use]
    pub fn specular_ior_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_IOR")
    }
    #[doc = "The composed `inputs:specular_IOR`, without node defaults or connection evaluation. Index of refraction for specular reflection."]
    #[must_use]
    pub fn specular_ior(&self) -> Option<f32> {
        self.read_value("inputs:specular_IOR", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:specular_IOR` at `time`.
    #[must_use]
    pub fn specular_ior_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular_IOR",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular_IOR` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_ior_default() -> f32 {
        1.5_f32
    }
    #[doc = "The authored `inputs:specular_anisotropy` port, if it exists. The directional bias of reflected and transmitted light resulting in materials appearing rougher or glossier in certain directions."]
    #[must_use]
    pub fn specular_anisotropy_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_anisotropy")
    }
    #[doc = "The composed `inputs:specular_anisotropy`, without node defaults or connection evaluation. The directional bias of reflected and transmitted light resulting in materials appearing rougher or glossier in certain directions."]
    #[must_use]
    pub fn specular_anisotropy(&self) -> Option<f32> {
        self.read_value(
            "inputs:specular_anisotropy",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:specular_anisotropy` at `time`.
    #[must_use]
    pub fn specular_anisotropy_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular_anisotropy",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular_anisotropy` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_anisotropy_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:specular_color` port, if it exists. Color tint on the specular reflection."]
    #[must_use]
    pub fn specular_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_color")
    }
    #[doc = "The composed `inputs:specular_color`, without node defaults or connection evaluation. Color tint on the specular reflection."]
    #[must_use]
    pub fn specular_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:specular_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:specular_color` at `time`.
    #[must_use]
    pub fn specular_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:specular_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:specular_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:specular_rotation` port, if it exists. Rotation of the axis of specular anisotropy around the surface normal."]
    #[must_use]
    pub fn specular_rotation_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_rotation")
    }
    #[doc = "The composed `inputs:specular_rotation`, without node defaults or connection evaluation. Rotation of the axis of specular anisotropy around the surface normal."]
    #[must_use]
    pub fn specular_rotation(&self) -> Option<f32> {
        self.read_value(
            "inputs:specular_rotation",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:specular_rotation` at `time`.
    #[must_use]
    pub fn specular_rotation_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular_rotation",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular_rotation` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_rotation_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:specular_roughness` port, if it exists. The roughness of the specular reflection. Lower numbers produce sharper reflections, higher numbers produce blurrier reflections."]
    #[must_use]
    pub fn specular_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("specular_roughness")
    }
    #[doc = "The composed `inputs:specular_roughness`, without node defaults or connection evaluation. The roughness of the specular reflection. Lower numbers produce sharper reflections, higher numbers produce blurrier reflections."]
    #[must_use]
    pub fn specular_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:specular_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:specular_roughness` at `time`.
    #[must_use]
    pub fn specular_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:specular_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:specular_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn specular_roughness_default() -> f32 {
        0.2_f32
    }
    #[doc = "The authored `inputs:subsurface` port, if it exists. The blend between diffuse reflection and subsurface scattering. A value of 1.0 indicates full subsurface scattering and a value 0 for diffuse reflection only."]
    #[must_use]
    pub fn subsurface_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface")
    }
    #[doc = "The composed `inputs:subsurface`, without node defaults or connection evaluation. The blend between diffuse reflection and subsurface scattering. A value of 1.0 indicates full subsurface scattering and a value 0 for diffuse reflection only."]
    #[must_use]
    pub fn subsurface(&self) -> Option<f32> {
        self.read_value("inputs:subsurface", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:subsurface` at `time`.
    #[must_use]
    pub fn subsurface_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:subsurface",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:subsurface` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:subsurface_anisotropy` port, if it exists. The direction of subsurface scattering. 0 scatters light evenly, positive values scatter forward and negative values scatter backward."]
    #[must_use]
    pub fn subsurface_anisotropy_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_anisotropy")
    }
    #[doc = "The composed `inputs:subsurface_anisotropy`, without node defaults or connection evaluation. The direction of subsurface scattering. 0 scatters light evenly, positive values scatter forward and negative values scatter backward."]
    #[must_use]
    pub fn subsurface_anisotropy(&self) -> Option<f32> {
        self.read_value(
            "inputs:subsurface_anisotropy",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:subsurface_anisotropy` at `time`.
    #[must_use]
    pub fn subsurface_anisotropy_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:subsurface_anisotropy",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:subsurface_anisotropy` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_anisotropy_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:subsurface_color` port, if it exists. The color of the subsurface scattering effect."]
    #[must_use]
    pub fn subsurface_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_color")
    }
    #[doc = "The composed `inputs:subsurface_color`, without node defaults or connection evaluation. The color of the subsurface scattering effect."]
    #[must_use]
    pub fn subsurface_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:subsurface_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:subsurface_color` at `time`.
    #[must_use]
    pub fn subsurface_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:subsurface_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:subsurface_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:subsurface_radius` port, if it exists. The mean free path. The distance which light can travel before being scattered inside the surface."]
    #[must_use]
    pub fn subsurface_radius_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_radius")
    }
    #[doc = "The composed `inputs:subsurface_radius`, without node defaults or connection evaluation. The mean free path. The distance which light can travel before being scattered inside the surface."]
    #[must_use]
    pub fn subsurface_radius(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:subsurface_radius",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:subsurface_radius` at `time`.
    #[must_use]
    pub fn subsurface_radius_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:subsurface_radius",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:subsurface_radius` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_radius_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:subsurface_scale` port, if it exists. Scalar weight for the subsurface radius value."]
    #[must_use]
    pub fn subsurface_scale_input(&self) -> Option<Port<'a>> {
        self.shader.input("subsurface_scale")
    }
    #[doc = "The composed `inputs:subsurface_scale`, without node defaults or connection evaluation. Scalar weight for the subsurface radius value."]
    #[must_use]
    pub fn subsurface_scale(&self) -> Option<f32> {
        self.read_value(
            "inputs:subsurface_scale",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:subsurface_scale` at `time`.
    #[must_use]
    pub fn subsurface_scale_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:subsurface_scale",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:subsurface_scale` default in the node definition, separate from authored values.
    #[must_use]
    pub fn subsurface_scale_default() -> f32 {
        1.0_f32
    }
    #[doc = "The authored `inputs:tangent` port, if it exists. Input geometric tangent"]
    #[must_use]
    pub fn tangent_input(&self) -> Option<Port<'a>> {
        self.shader.input("tangent")
    }
    #[doc = "The composed `inputs:tangent`, without node defaults or connection evaluation. Input geometric tangent"]
    #[must_use]
    pub fn tangent(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:tangent", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:tangent` at `time`.
    #[must_use]
    pub fn tangent_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:tangent",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    #[doc = "The authored `inputs:thin_film_IOR` port, if it exists. The index of refraction of the medium surrounding the material."]
    #[must_use]
    pub fn thin_film_ior_input(&self) -> Option<Port<'a>> {
        self.shader.input("thin_film_IOR")
    }
    #[doc = "The composed `inputs:thin_film_IOR`, without node defaults or connection evaluation. The index of refraction of the medium surrounding the material."]
    #[must_use]
    pub fn thin_film_ior(&self) -> Option<f32> {
        self.read_value(
            "inputs:thin_film_IOR",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:thin_film_IOR` at `time`.
    #[must_use]
    pub fn thin_film_ior_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:thin_film_IOR",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:thin_film_IOR` default in the node definition, separate from authored values.
    #[must_use]
    pub fn thin_film_ior_default() -> f32 {
        1.5_f32
    }
    #[doc = "The authored `inputs:thin_film_thickness` port, if it exists. The thickness of the thin film layer on a surface. Use for materials such as multitone car paint or soap bubbles (in nanometers)."]
    #[must_use]
    pub fn thin_film_thickness_input(&self) -> Option<Port<'a>> {
        self.shader.input("thin_film_thickness")
    }
    #[doc = "The composed `inputs:thin_film_thickness`, without node defaults or connection evaluation. The thickness of the thin film layer on a surface. Use for materials such as multitone car paint or soap bubbles (in nanometers)."]
    #[must_use]
    pub fn thin_film_thickness(&self) -> Option<f32> {
        self.read_value(
            "inputs:thin_film_thickness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:thin_film_thickness` at `time`.
    #[must_use]
    pub fn thin_film_thickness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:thin_film_thickness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:thin_film_thickness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn thin_film_thickness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:thin_walled` port, if it exists. If true the surface is double-sided and represents an infinitely thin shell. Suitable for thin objects such as tree leaves or paper."]
    #[must_use]
    pub fn thin_walled_input(&self) -> Option<Port<'a>> {
        self.shader.input("thin_walled")
    }
    #[doc = "The composed `inputs:thin_walled`, without node defaults or connection evaluation. If true the surface is double-sided and represents an infinitely thin shell. Suitable for thin objects such as tree leaves or paper."]
    #[must_use]
    pub fn thin_walled(&self) -> Option<bool> {
        self.read_value("inputs:thin_walled", layerstack_schemas::value::read_bool)
    }
    /// The composed `inputs:thin_walled` at `time`.
    #[must_use]
    pub fn thin_walled_at(&self, time: layerstack::Time) -> Option<bool> {
        self.read_value_at(
            "inputs:thin_walled",
            time,
            layerstack_schemas::value::read_bool,
        )
    }
    /// The `inputs:thin_walled` default in the node definition, separate from authored values.
    #[must_use]
    pub fn thin_walled_default() -> bool {
        false
    }
    #[doc = "The authored `inputs:transmission` port, if it exists. Transmission of light through the surface for materials such as glass or water. The greater the value the more transparent the material."]
    #[must_use]
    pub fn transmission_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission")
    }
    #[doc = "The composed `inputs:transmission`, without node defaults or connection evaluation. Transmission of light through the surface for materials such as glass or water. The greater the value the more transparent the material."]
    #[must_use]
    pub fn transmission(&self) -> Option<f32> {
        self.read_value("inputs:transmission", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:transmission` at `time`.
    #[must_use]
    pub fn transmission_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission_color` port, if it exists. Color tint on the transmitted light."]
    #[must_use]
    pub fn transmission_color_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_color")
    }
    #[doc = "The composed `inputs:transmission_color`, without node defaults or connection evaluation. Color tint on the transmitted light."]
    #[must_use]
    pub fn transmission_color(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:transmission_color",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:transmission_color` at `time`.
    #[must_use]
    pub fn transmission_color_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:transmission_color",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:transmission_color` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_color_default() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }
    #[doc = "The authored `inputs:transmission_depth` port, if it exists. Specifies the distance light travels inside the material before its becomes exactly the transmission color according to Beer's law."]
    #[must_use]
    pub fn transmission_depth_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_depth")
    }
    #[doc = "The composed `inputs:transmission_depth`, without node defaults or connection evaluation. Specifies the distance light travels inside the material before its becomes exactly the transmission color according to Beer's law."]
    #[must_use]
    pub fn transmission_depth(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_depth",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_depth` at `time`.
    #[must_use]
    pub fn transmission_depth_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_depth",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_depth` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_depth_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission_dispersion` port, if it exists. Dispersion amount, describing how much the index of refraction varies across wavelengths."]
    #[must_use]
    pub fn transmission_dispersion_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_dispersion")
    }
    #[doc = "The composed `inputs:transmission_dispersion`, without node defaults or connection evaluation. Dispersion amount, describing how much the index of refraction varies across wavelengths."]
    #[must_use]
    pub fn transmission_dispersion(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_dispersion",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_dispersion` at `time`.
    #[must_use]
    pub fn transmission_dispersion_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_dispersion",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_dispersion` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_dispersion_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission_extra_roughness` port, if it exists. Additional roughness on top of specular roughness. Positive values blur refractions more than reflections, and negative values blur refractions less."]
    #[must_use]
    pub fn transmission_extra_roughness_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_extra_roughness")
    }
    #[doc = "The composed `inputs:transmission_extra_roughness`, without node defaults or connection evaluation. Additional roughness on top of specular roughness. Positive values blur refractions more than reflections, and negative values blur refractions less."]
    #[must_use]
    pub fn transmission_extra_roughness(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_extra_roughness",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_extra_roughness` at `time`.
    #[must_use]
    pub fn transmission_extra_roughness_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_extra_roughness",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_extra_roughness` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_extra_roughness_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:transmission_scatter` port, if it exists. Scattering coefficient of the interior medium. Suitable for a large body of liquid or one that is fairly thick, such as an ocean, honey, ice, or frosted glass."]
    #[must_use]
    pub fn transmission_scatter_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_scatter")
    }
    #[doc = "The composed `inputs:transmission_scatter`, without node defaults or connection evaluation. Scattering coefficient of the interior medium. Suitable for a large body of liquid or one that is fairly thick, such as an ocean, honey, ice, or frosted glass."]
    #[must_use]
    pub fn transmission_scatter(&self) -> Option<[f32; 3]> {
        self.read_value(
            "inputs:transmission_scatter",
            layerstack_schemas::value::read_float3,
        )
    }
    /// The composed `inputs:transmission_scatter` at `time`.
    #[must_use]
    pub fn transmission_scatter_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:transmission_scatter",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:transmission_scatter` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_scatter_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:transmission_scatter_anisotropy` port, if it exists. The amount of directional bias, or anisotropy, of the scattering."]
    #[must_use]
    pub fn transmission_scatter_anisotropy_input(&self) -> Option<Port<'a>> {
        self.shader.input("transmission_scatter_anisotropy")
    }
    #[doc = "The composed `inputs:transmission_scatter_anisotropy`, without node defaults or connection evaluation. The amount of directional bias, or anisotropy, of the scattering."]
    #[must_use]
    pub fn transmission_scatter_anisotropy(&self) -> Option<f32> {
        self.read_value(
            "inputs:transmission_scatter_anisotropy",
            layerstack_schemas::value::read_float,
        )
    }
    /// The composed `inputs:transmission_scatter_anisotropy` at `time`.
    #[must_use]
    pub fn transmission_scatter_anisotropy_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:transmission_scatter_anisotropy",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:transmission_scatter_anisotropy` default in the node definition, separate from authored values.
    #[must_use]
    pub fn transmission_scatter_anisotropy_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `outputs:surface` port, if it exists. "]
    #[must_use]
    pub fn surface_output(&self) -> Option<Port<'a>> {
        self.shader.output("surface")
    }
}

/// Authors `ND_standard_surface_surfaceshader` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdStandardSurfaceSurfaceshaderEdit {
    shader: ShaderEdit,
}
impl Deref for NdStandardSurfaceSurfaceshaderEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdStandardSurfaceSurfaceshaderEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:base` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_base_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "base", ty)
    }
    /// Authors `inputs:base` at default time, creating the typed input if needed.
    pub fn set_base(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_base_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:base` sample in stage time through the edit target.
    pub fn set_base_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_base_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:base_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_base_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "base_color", ty)
    }
    /// Authors `inputs:base_color` at default time, creating the typed input if needed.
    pub fn set_base_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_base_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:base_color` sample in stage time through the edit target.
    pub fn set_base_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_base_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat", ty)
    }
    /// Authors `inputs:coat` at default time, creating the typed input if needed.
    pub fn set_coat(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_coat_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat` sample in stage time through the edit target.
    pub fn set_coat_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_IOR` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_ior_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_IOR", ty)
    }
    /// Authors `inputs:coat_IOR` at default time, creating the typed input if needed.
    pub fn set_coat_ior(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_coat_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_IOR` sample in stage time through the edit target.
    pub fn set_coat_ior_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_affect_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_affect_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_affect_color", ty)
    }
    /// Authors `inputs:coat_affect_color` at default time, creating the typed input if needed.
    pub fn set_coat_affect_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_affect_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_affect_color` sample in stage time through the edit target.
    pub fn set_coat_affect_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_affect_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_affect_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_affect_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_affect_roughness", ty)
    }
    /// Authors `inputs:coat_affect_roughness` at default time, creating the typed input if needed.
    pub fn set_coat_affect_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_affect_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_affect_roughness` sample in stage time through the edit target.
    pub fn set_coat_affect_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_affect_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_anisotropy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_anisotropy_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_anisotropy", ty)
    }
    /// Authors `inputs:coat_anisotropy` at default time, creating the typed input if needed.
    pub fn set_coat_anisotropy(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_anisotropy` sample in stage time through the edit target.
    pub fn set_coat_anisotropy_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "coat_color", ty)
    }
    /// Authors `inputs:coat_color` at default time, creating the typed input if needed.
    pub fn set_coat_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_color` sample in stage time through the edit target.
    pub fn set_coat_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_normal` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_normal_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "coat_normal", ty)
    }
    /// Authors `inputs:coat_normal` at default time, creating the typed input if needed.
    pub fn set_coat_normal(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_normal` sample in stage time through the edit target.
    pub fn set_coat_normal_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_rotation` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_rotation_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_rotation", ty)
    }
    /// Authors `inputs:coat_rotation` at default time, creating the typed input if needed.
    pub fn set_coat_rotation(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_rotation_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_rotation` sample in stage time through the edit target.
    pub fn set_coat_rotation_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_rotation_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:coat_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_coat_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "coat_roughness", ty)
    }
    /// Authors `inputs:coat_roughness` at default time, creating the typed input if needed.
    pub fn set_coat_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:coat_roughness` sample in stage time through the edit target.
    pub fn set_coat_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_coat_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:diffuse_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_diffuse_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "diffuse_roughness", ty)
    }
    /// Authors `inputs:diffuse_roughness` at default time, creating the typed input if needed.
    pub fn set_diffuse_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_diffuse_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:diffuse_roughness` sample in stage time through the edit target.
    pub fn set_diffuse_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_diffuse_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:emission` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_emission_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "emission", ty)
    }
    /// Authors `inputs:emission` at default time, creating the typed input if needed.
    pub fn set_emission(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_emission_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:emission` sample in stage time through the edit target.
    pub fn set_emission_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_emission_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:emission_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_emission_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "emission_color", ty)
    }
    /// Authors `inputs:emission_color` at default time, creating the typed input if needed.
    pub fn set_emission_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_emission_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:emission_color` sample in stage time through the edit target.
    pub fn set_emission_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_emission_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:metalness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_metalness_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "metalness", ty)
    }
    /// Authors `inputs:metalness` at default time, creating the typed input if needed.
    pub fn set_metalness(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_metalness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:metalness` sample in stage time through the edit target.
    pub fn set_metalness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_metalness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:normal` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_normal_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "normal", ty)
    }
    /// Authors `inputs:normal` at default time, creating the typed input if needed.
    pub fn set_normal(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:normal` sample in stage time through the edit target.
    pub fn set_normal_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_normal_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:opacity` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_opacity_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "opacity", ty)
    }
    /// Authors `inputs:opacity` at default time, creating the typed input if needed.
    pub fn set_opacity(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_opacity_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:opacity` sample in stage time through the edit target.
    pub fn set_opacity_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_opacity_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:sheen` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_sheen_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "sheen", ty)
    }
    /// Authors `inputs:sheen` at default time, creating the typed input if needed.
    pub fn set_sheen(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_sheen_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:sheen` sample in stage time through the edit target.
    pub fn set_sheen_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:sheen_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_sheen_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "sheen_color", ty)
    }
    /// Authors `inputs:sheen_color` at default time, creating the typed input if needed.
    pub fn set_sheen_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:sheen_color` sample in stage time through the edit target.
    pub fn set_sheen_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:sheen_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_sheen_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "sheen_roughness", ty)
    }
    /// Authors `inputs:sheen_roughness` at default time, creating the typed input if needed.
    pub fn set_sheen_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:sheen_roughness` sample in stage time through the edit target.
    pub fn set_sheen_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_sheen_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular", ty)
    }
    /// Authors `inputs:specular` at default time, creating the typed input if needed.
    pub fn set_specular(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_specular_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular` sample in stage time through the edit target.
    pub fn set_specular_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_IOR` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_ior_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular_IOR", ty)
    }
    /// Authors `inputs:specular_IOR` at default time, creating the typed input if needed.
    pub fn set_specular_ior(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_IOR` sample in stage time through the edit target.
    pub fn set_specular_ior_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_anisotropy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_anisotropy_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular_anisotropy", ty)
    }
    /// Authors `inputs:specular_anisotropy` at default time, creating the typed input if needed.
    pub fn set_specular_anisotropy(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_anisotropy` sample in stage time through the edit target.
    pub fn set_specular_anisotropy_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "specular_color", ty)
    }
    /// Authors `inputs:specular_color` at default time, creating the typed input if needed.
    pub fn set_specular_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_color` sample in stage time through the edit target.
    pub fn set_specular_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_rotation` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_rotation_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular_rotation", ty)
    }
    /// Authors `inputs:specular_rotation` at default time, creating the typed input if needed.
    pub fn set_specular_rotation(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_rotation_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_rotation` sample in stage time through the edit target.
    pub fn set_specular_rotation_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_rotation_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:specular_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_specular_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "specular_roughness", ty)
    }
    /// Authors `inputs:specular_roughness` at default time, creating the typed input if needed.
    pub fn set_specular_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:specular_roughness` sample in stage time through the edit target.
    pub fn set_specular_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_specular_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "subsurface", ty)
    }
    /// Authors `inputs:subsurface` at default time, creating the typed input if needed.
    pub fn set_subsurface(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface` sample in stage time through the edit target.
    pub fn set_subsurface_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_anisotropy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_anisotropy_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "subsurface_anisotropy", ty)
    }
    /// Authors `inputs:subsurface_anisotropy` at default time, creating the typed input if needed.
    pub fn set_subsurface_anisotropy(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_anisotropy` sample in stage time through the edit target.
    pub fn set_subsurface_anisotropy_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "subsurface_color", ty)
    }
    /// Authors `inputs:subsurface_color` at default time, creating the typed input if needed.
    pub fn set_subsurface_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_color` sample in stage time through the edit target.
    pub fn set_subsurface_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_radius` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_radius_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "subsurface_radius", ty)
    }
    /// Authors `inputs:subsurface_radius` at default time, creating the typed input if needed.
    pub fn set_subsurface_radius(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_radius_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_radius` sample in stage time through the edit target.
    pub fn set_subsurface_radius_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_radius_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:subsurface_scale` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_subsurface_scale_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "subsurface_scale", ty)
    }
    /// Authors `inputs:subsurface_scale` at default time, creating the typed input if needed.
    pub fn set_subsurface_scale(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:subsurface_scale` sample in stage time through the edit target.
    pub fn set_subsurface_scale_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_subsurface_scale_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:tangent` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_tangent_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "tangent", ty)
    }
    /// Authors `inputs:tangent` at default time, creating the typed input if needed.
    pub fn set_tangent(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:tangent` sample in stage time through the edit target.
    pub fn set_tangent_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_tangent_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:thin_film_IOR` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_thin_film_ior_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "thin_film_IOR", ty)
    }
    /// Authors `inputs:thin_film_IOR` at default time, creating the typed input if needed.
    pub fn set_thin_film_ior(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:thin_film_IOR` sample in stage time through the edit target.
    pub fn set_thin_film_ior_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_ior_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:thin_film_thickness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_thin_film_thickness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "thin_film_thickness", ty)
    }
    /// Authors `inputs:thin_film_thickness` at default time, creating the typed input if needed.
    pub fn set_thin_film_thickness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_thickness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:thin_film_thickness` sample in stage time through the edit target.
    pub fn set_thin_film_thickness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_film_thickness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:thin_walled` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_thin_walled_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("bool").expect("a registered value type");
        self.shader.create_input(edit, "thin_walled", ty)
    }
    /// Authors `inputs:thin_walled` at default time, creating the typed input if needed.
    pub fn set_thin_walled(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: bool,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_walled_input(edit)?;
        let value = (layerstack_schemas::value::write_bool)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:thin_walled` sample in stage time through the edit target.
    pub fn set_thin_walled_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: bool,
    ) -> Result<&Self, PortError> {
        let port = self.create_thin_walled_input(edit)?;
        let value = (layerstack_schemas::value::write_bool)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "transmission", ty)
    }
    /// Authors `inputs:transmission` at default time, creating the typed input if needed.
    pub fn set_transmission(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission` sample in stage time through the edit target.
    pub fn set_transmission_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_color` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_color_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "transmission_color", ty)
    }
    /// Authors `inputs:transmission_color` at default time, creating the typed input if needed.
    pub fn set_transmission_color(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_color` sample in stage time through the edit target.
    pub fn set_transmission_color_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_color_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_depth` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_depth_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "transmission_depth", ty)
    }
    /// Authors `inputs:transmission_depth` at default time, creating the typed input if needed.
    pub fn set_transmission_depth(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_depth_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_depth` sample in stage time through the edit target.
    pub fn set_transmission_depth_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_depth_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_dispersion` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_dispersion_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "transmission_dispersion", ty)
    }
    /// Authors `inputs:transmission_dispersion` at default time, creating the typed input if needed.
    pub fn set_transmission_dispersion(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_dispersion_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_dispersion` sample in stage time through the edit target.
    pub fn set_transmission_dispersion_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_dispersion_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_extra_roughness` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_extra_roughness_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "transmission_extra_roughness", ty)
    }
    /// Authors `inputs:transmission_extra_roughness` at default time, creating the typed input if needed.
    pub fn set_transmission_extra_roughness(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_extra_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_extra_roughness` sample in stage time through the edit target.
    pub fn set_transmission_extra_roughness_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_extra_roughness_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_scatter` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_scatter_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "transmission_scatter", ty)
    }
    /// Authors `inputs:transmission_scatter` at default time, creating the typed input if needed.
    pub fn set_transmission_scatter(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_scatter_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_scatter` sample in stage time through the edit target.
    pub fn set_transmission_scatter_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_scatter_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:transmission_scatter_anisotropy` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_transmission_scatter_anisotropy_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader
            .create_input(edit, "transmission_scatter_anisotropy", ty)
    }
    /// Authors `inputs:transmission_scatter_anisotropy` at default time, creating the typed input if needed.
    pub fn set_transmission_scatter_anisotropy(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_scatter_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:transmission_scatter_anisotropy` sample in stage time through the edit target.
    pub fn set_transmission_scatter_anisotropy_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_transmission_scatter_anisotropy_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:surface` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_surface_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("token").expect("a registered value type");
        self.shader.create_output(edit, "surface", ty)
    }
}

/// The `ND_texcoord_vector2` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdTexcoordVector2<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdTexcoordVector2<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdTexcoordVector2<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_texcoord_vector2";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdTexcoordVector2Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdTexcoordVector2Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "index",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:index",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "vector2",
                usd_name: "outputs:out",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:index` port, if it exists. "]
    #[must_use]
    pub fn index_input(&self) -> Option<Port<'a>> {
        self.shader.input("index")
    }
    #[doc = "The composed `inputs:index`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn index(&self) -> Option<i32> {
        self.read_value("inputs:index", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:index` at `time`.
    #[must_use]
    pub fn index_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at("inputs:index", time, layerstack_schemas::value::read_int)
    }
    /// The `inputs:index` default in the node definition, separate from authored values.
    #[must_use]
    pub fn index_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_texcoord_vector2` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdTexcoordVector2Edit {
    shader: ShaderEdit,
}
impl Deref for NdTexcoordVector2Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdTexcoordVector2Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:index` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_index_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "index", ty)
    }
    /// Authors `inputs:index` at default time, creating the typed input if needed.
    pub fn set_index(&self, edit: &mut SchemaEdit<'_>, value: i32) -> Result<&Self, PortError> {
        let port = self.create_index_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:index` sample in stage time through the edit target.
    pub fn set_index_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_index_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_tiledimage_color3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdTiledimageColor3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdTiledimageColor3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdTiledimageColor3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_tiledimage_color3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdTiledimageColor3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdTiledimageColor3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "default",
                direction: PortKind::Input,
                type_name: "color3",
                usd_name: "inputs:default",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "file",
                direction: PortKind::Input,
                type_name: "filename",
                usd_name: "inputs:file",
                usd_type_name: "asset",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "filtertype",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:filtertype",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "linear",
                )),
            },
            MaterialXPort {
                name: "frameendaction",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:frameendaction",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "constant",
                )),
            },
            MaterialXPort {
                name: "frameoffset",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:frameoffset",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "framerange",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:framerange",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "realworldimagesize",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:realworldimagesize",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "realworldtilesize",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:realworldtilesize",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "texcoord",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:texcoord",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "UV0",
                )),
            },
            MaterialXPort {
                name: "uvoffset",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:uvoffset",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "uvtiling",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:uvtiling",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "color3",
                usd_name: "outputs:out",
                usd_type_name: "color3f",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:default` port, if it exists. "]
    #[must_use]
    pub fn default_input(&self) -> Option<Port<'a>> {
        self.shader.input("default")
    }
    #[doc = "The composed `inputs:default`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn default(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:default", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:default` at `time`.
    #[must_use]
    pub fn default_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:default",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:default` default in the node definition, separate from authored values.
    #[must_use]
    pub fn default_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:file` port, if it exists. "]
    #[must_use]
    pub fn file_input(&self) -> Option<Port<'a>> {
        self.shader.input("file")
    }
    #[doc = "The composed `inputs:file`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn file(&self) -> Option<Arc<str>> {
        self.read_value("inputs:file", layerstack_schemas::value::read_asset)
    }
    /// The composed `inputs:file` at `time`.
    #[must_use]
    pub fn file_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:file", time, layerstack_schemas::value::read_asset)
    }
    /// The `inputs:file` default in the node definition, separate from authored values.
    #[must_use]
    pub fn file_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:filtertype` port, if it exists. "]
    #[must_use]
    pub fn filtertype_input(&self) -> Option<Port<'a>> {
        self.shader.input("filtertype")
    }
    #[doc = "The composed `inputs:filtertype`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn filtertype(&self) -> Option<Arc<str>> {
        self.read_value("inputs:filtertype", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:filtertype` at `time`.
    #[must_use]
    pub fn filtertype_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:filtertype",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:filtertype` default in the node definition, separate from authored values.
    #[must_use]
    pub fn filtertype_default() -> Arc<str> {
        Arc::from("linear")
    }
    #[doc = "The authored `inputs:frameendaction` port, if it exists. "]
    #[must_use]
    pub fn frameendaction_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameendaction")
    }
    #[doc = "The composed `inputs:frameendaction`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameendaction(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:frameendaction",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:frameendaction` at `time`.
    #[must_use]
    pub fn frameendaction_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:frameendaction",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:frameendaction` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameendaction_default() -> Arc<str> {
        Arc::from("constant")
    }
    #[doc = "The authored `inputs:frameoffset` port, if it exists. "]
    #[must_use]
    pub fn frameoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameoffset")
    }
    #[doc = "The composed `inputs:frameoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameoffset(&self) -> Option<i32> {
        self.read_value("inputs:frameoffset", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:frameoffset` at `time`.
    #[must_use]
    pub fn frameoffset_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:frameoffset",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:frameoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameoffset_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:framerange` port, if it exists. "]
    #[must_use]
    pub fn framerange_input(&self) -> Option<Port<'a>> {
        self.shader.input("framerange")
    }
    #[doc = "The composed `inputs:framerange`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn framerange(&self) -> Option<Arc<str>> {
        self.read_value("inputs:framerange", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:framerange` at `time`.
    #[must_use]
    pub fn framerange_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:framerange",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:framerange` default in the node definition, separate from authored values.
    #[must_use]
    pub fn framerange_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:realworldimagesize` port, if it exists. "]
    #[must_use]
    pub fn realworldimagesize_input(&self) -> Option<Port<'a>> {
        self.shader.input("realworldimagesize")
    }
    #[doc = "The composed `inputs:realworldimagesize`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn realworldimagesize(&self) -> Option<[f32; 2]> {
        self.read_value(
            "inputs:realworldimagesize",
            layerstack_schemas::value::read_float2,
        )
    }
    /// The composed `inputs:realworldimagesize` at `time`.
    #[must_use]
    pub fn realworldimagesize_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:realworldimagesize",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:realworldimagesize` default in the node definition, separate from authored values.
    #[must_use]
    pub fn realworldimagesize_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `inputs:realworldtilesize` port, if it exists. "]
    #[must_use]
    pub fn realworldtilesize_input(&self) -> Option<Port<'a>> {
        self.shader.input("realworldtilesize")
    }
    #[doc = "The composed `inputs:realworldtilesize`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn realworldtilesize(&self) -> Option<[f32; 2]> {
        self.read_value(
            "inputs:realworldtilesize",
            layerstack_schemas::value::read_float2,
        )
    }
    /// The composed `inputs:realworldtilesize` at `time`.
    #[must_use]
    pub fn realworldtilesize_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:realworldtilesize",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:realworldtilesize` default in the node definition, separate from authored values.
    #[must_use]
    pub fn realworldtilesize_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `inputs:texcoord` port, if it exists. "]
    #[must_use]
    pub fn texcoord_input(&self) -> Option<Port<'a>> {
        self.shader.input("texcoord")
    }
    #[doc = "The composed `inputs:texcoord`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn texcoord(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:texcoord", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:texcoord` at `time`.
    #[must_use]
    pub fn texcoord_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:texcoord",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    #[doc = "The authored `inputs:uvoffset` port, if it exists. "]
    #[must_use]
    pub fn uvoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("uvoffset")
    }
    #[doc = "The composed `inputs:uvoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uvoffset(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:uvoffset", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:uvoffset` at `time`.
    #[must_use]
    pub fn uvoffset_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:uvoffset",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:uvoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uvoffset_default() -> [f32; 2] {
        [0.0, 0.0]
    }
    #[doc = "The authored `inputs:uvtiling` port, if it exists. "]
    #[must_use]
    pub fn uvtiling_input(&self) -> Option<Port<'a>> {
        self.shader.input("uvtiling")
    }
    #[doc = "The composed `inputs:uvtiling`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uvtiling(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:uvtiling", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:uvtiling` at `time`.
    #[must_use]
    pub fn uvtiling_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:uvtiling",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:uvtiling` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uvtiling_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_tiledimage_color3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdTiledimageColor3Edit {
    shader: ShaderEdit,
}
impl Deref for NdTiledimageColor3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdTiledimageColor3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:default` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_default_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_input(edit, "default", ty)
    }
    /// Authors `inputs:default` at default time, creating the typed input if needed.
    pub fn set_default(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:default` sample in stage time through the edit target.
    pub fn set_default_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:file` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_file_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("asset").expect("a registered value type");
        self.shader.create_input(edit, "file", ty)
    }
    /// Authors `inputs:file` at default time, creating the typed input if needed.
    pub fn set_file(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:file` sample in stage time through the edit target.
    pub fn set_file_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:filtertype` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_filtertype_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "filtertype", ty)
    }
    /// Authors `inputs:filtertype` at default time, creating the typed input if needed.
    pub fn set_filtertype(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:filtertype` sample in stage time through the edit target.
    pub fn set_filtertype_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameendaction` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameendaction_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "frameendaction", ty)
    }
    /// Authors `inputs:frameendaction` at default time, creating the typed input if needed.
    pub fn set_frameendaction(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameendaction` sample in stage time through the edit target.
    pub fn set_frameendaction_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameoffset_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "frameoffset", ty)
    }
    /// Authors `inputs:frameoffset` at default time, creating the typed input if needed.
    pub fn set_frameoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameoffset` sample in stage time through the edit target.
    pub fn set_frameoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:framerange` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_framerange_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "framerange", ty)
    }
    /// Authors `inputs:framerange` at default time, creating the typed input if needed.
    pub fn set_framerange(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:framerange` sample in stage time through the edit target.
    pub fn set_framerange_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:realworldimagesize` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_realworldimagesize_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "realworldimagesize", ty)
    }
    /// Authors `inputs:realworldimagesize` at default time, creating the typed input if needed.
    pub fn set_realworldimagesize(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldimagesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:realworldimagesize` sample in stage time through the edit target.
    pub fn set_realworldimagesize_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldimagesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:realworldtilesize` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_realworldtilesize_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "realworldtilesize", ty)
    }
    /// Authors `inputs:realworldtilesize` at default time, creating the typed input if needed.
    pub fn set_realworldtilesize(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldtilesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:realworldtilesize` sample in stage time through the edit target.
    pub fn set_realworldtilesize_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldtilesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:texcoord` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_texcoord_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "texcoord", ty)
    }
    /// Authors `inputs:texcoord` at default time, creating the typed input if needed.
    pub fn set_texcoord(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:texcoord` sample in stage time through the edit target.
    pub fn set_texcoord_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uvoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uvoffset_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "uvoffset", ty)
    }
    /// Authors `inputs:uvoffset` at default time, creating the typed input if needed.
    pub fn set_uvoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uvoffset` sample in stage time through the edit target.
    pub fn set_uvoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uvtiling` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uvtiling_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "uvtiling", ty)
    }
    /// Authors `inputs:uvtiling` at default time, creating the typed input if needed.
    pub fn set_uvtiling(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvtiling_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uvtiling` sample in stage time through the edit target.
    pub fn set_uvtiling_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvtiling_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("color3f").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_tiledimage_float` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdTiledimageFloat<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdTiledimageFloat<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdTiledimageFloat<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_tiledimage_float";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdTiledimageFloatEdit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdTiledimageFloatEdit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "default",
                direction: PortKind::Input,
                type_name: "float",
                usd_name: "inputs:default",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
            MaterialXPort {
                name: "file",
                direction: PortKind::Input,
                type_name: "filename",
                usd_name: "inputs:file",
                usd_type_name: "asset",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "filtertype",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:filtertype",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "linear",
                )),
            },
            MaterialXPort {
                name: "frameendaction",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:frameendaction",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "constant",
                )),
            },
            MaterialXPort {
                name: "frameoffset",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:frameoffset",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "framerange",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:framerange",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "realworldimagesize",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:realworldimagesize",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "realworldtilesize",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:realworldtilesize",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "texcoord",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:texcoord",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "UV0",
                )),
            },
            MaterialXPort {
                name: "uvoffset",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:uvoffset",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "uvtiling",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:uvtiling",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "float",
                usd_name: "outputs:out",
                usd_type_name: "float",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:default` port, if it exists. "]
    #[must_use]
    pub fn default_input(&self) -> Option<Port<'a>> {
        self.shader.input("default")
    }
    #[doc = "The composed `inputs:default`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn default(&self) -> Option<f32> {
        self.read_value("inputs:default", layerstack_schemas::value::read_float)
    }
    /// The composed `inputs:default` at `time`.
    #[must_use]
    pub fn default_at(&self, time: layerstack::Time) -> Option<f32> {
        self.read_value_at(
            "inputs:default",
            time,
            layerstack_schemas::value::read_float,
        )
    }
    /// The `inputs:default` default in the node definition, separate from authored values.
    #[must_use]
    pub fn default_default() -> f32 {
        0.0_f32
    }
    #[doc = "The authored `inputs:file` port, if it exists. "]
    #[must_use]
    pub fn file_input(&self) -> Option<Port<'a>> {
        self.shader.input("file")
    }
    #[doc = "The composed `inputs:file`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn file(&self) -> Option<Arc<str>> {
        self.read_value("inputs:file", layerstack_schemas::value::read_asset)
    }
    /// The composed `inputs:file` at `time`.
    #[must_use]
    pub fn file_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:file", time, layerstack_schemas::value::read_asset)
    }
    /// The `inputs:file` default in the node definition, separate from authored values.
    #[must_use]
    pub fn file_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:filtertype` port, if it exists. "]
    #[must_use]
    pub fn filtertype_input(&self) -> Option<Port<'a>> {
        self.shader.input("filtertype")
    }
    #[doc = "The composed `inputs:filtertype`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn filtertype(&self) -> Option<Arc<str>> {
        self.read_value("inputs:filtertype", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:filtertype` at `time`.
    #[must_use]
    pub fn filtertype_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:filtertype",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:filtertype` default in the node definition, separate from authored values.
    #[must_use]
    pub fn filtertype_default() -> Arc<str> {
        Arc::from("linear")
    }
    #[doc = "The authored `inputs:frameendaction` port, if it exists. "]
    #[must_use]
    pub fn frameendaction_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameendaction")
    }
    #[doc = "The composed `inputs:frameendaction`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameendaction(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:frameendaction",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:frameendaction` at `time`.
    #[must_use]
    pub fn frameendaction_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:frameendaction",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:frameendaction` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameendaction_default() -> Arc<str> {
        Arc::from("constant")
    }
    #[doc = "The authored `inputs:frameoffset` port, if it exists. "]
    #[must_use]
    pub fn frameoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameoffset")
    }
    #[doc = "The composed `inputs:frameoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameoffset(&self) -> Option<i32> {
        self.read_value("inputs:frameoffset", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:frameoffset` at `time`.
    #[must_use]
    pub fn frameoffset_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:frameoffset",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:frameoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameoffset_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:framerange` port, if it exists. "]
    #[must_use]
    pub fn framerange_input(&self) -> Option<Port<'a>> {
        self.shader.input("framerange")
    }
    #[doc = "The composed `inputs:framerange`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn framerange(&self) -> Option<Arc<str>> {
        self.read_value("inputs:framerange", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:framerange` at `time`.
    #[must_use]
    pub fn framerange_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:framerange",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:framerange` default in the node definition, separate from authored values.
    #[must_use]
    pub fn framerange_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:realworldimagesize` port, if it exists. "]
    #[must_use]
    pub fn realworldimagesize_input(&self) -> Option<Port<'a>> {
        self.shader.input("realworldimagesize")
    }
    #[doc = "The composed `inputs:realworldimagesize`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn realworldimagesize(&self) -> Option<[f32; 2]> {
        self.read_value(
            "inputs:realworldimagesize",
            layerstack_schemas::value::read_float2,
        )
    }
    /// The composed `inputs:realworldimagesize` at `time`.
    #[must_use]
    pub fn realworldimagesize_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:realworldimagesize",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:realworldimagesize` default in the node definition, separate from authored values.
    #[must_use]
    pub fn realworldimagesize_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `inputs:realworldtilesize` port, if it exists. "]
    #[must_use]
    pub fn realworldtilesize_input(&self) -> Option<Port<'a>> {
        self.shader.input("realworldtilesize")
    }
    #[doc = "The composed `inputs:realworldtilesize`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn realworldtilesize(&self) -> Option<[f32; 2]> {
        self.read_value(
            "inputs:realworldtilesize",
            layerstack_schemas::value::read_float2,
        )
    }
    /// The composed `inputs:realworldtilesize` at `time`.
    #[must_use]
    pub fn realworldtilesize_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:realworldtilesize",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:realworldtilesize` default in the node definition, separate from authored values.
    #[must_use]
    pub fn realworldtilesize_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `inputs:texcoord` port, if it exists. "]
    #[must_use]
    pub fn texcoord_input(&self) -> Option<Port<'a>> {
        self.shader.input("texcoord")
    }
    #[doc = "The composed `inputs:texcoord`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn texcoord(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:texcoord", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:texcoord` at `time`.
    #[must_use]
    pub fn texcoord_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:texcoord",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    #[doc = "The authored `inputs:uvoffset` port, if it exists. "]
    #[must_use]
    pub fn uvoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("uvoffset")
    }
    #[doc = "The composed `inputs:uvoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uvoffset(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:uvoffset", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:uvoffset` at `time`.
    #[must_use]
    pub fn uvoffset_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:uvoffset",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:uvoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uvoffset_default() -> [f32; 2] {
        [0.0, 0.0]
    }
    #[doc = "The authored `inputs:uvtiling` port, if it exists. "]
    #[must_use]
    pub fn uvtiling_input(&self) -> Option<Port<'a>> {
        self.shader.input("uvtiling")
    }
    #[doc = "The composed `inputs:uvtiling`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uvtiling(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:uvtiling", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:uvtiling` at `time`.
    #[must_use]
    pub fn uvtiling_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:uvtiling",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:uvtiling` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uvtiling_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_tiledimage_float` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdTiledimageFloatEdit {
    shader: ShaderEdit,
}
impl Deref for NdTiledimageFloatEdit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdTiledimageFloatEdit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:default` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_default_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_input(edit, "default", ty)
    }
    /// Authors `inputs:default` at default time, creating the typed input if needed.
    pub fn set_default(&self, edit: &mut SchemaEdit<'_>, value: f32) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:default` sample in stage time through the edit target.
    pub fn set_default_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: f32,
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:file` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_file_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("asset").expect("a registered value type");
        self.shader.create_input(edit, "file", ty)
    }
    /// Authors `inputs:file` at default time, creating the typed input if needed.
    pub fn set_file(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:file` sample in stage time through the edit target.
    pub fn set_file_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:filtertype` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_filtertype_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "filtertype", ty)
    }
    /// Authors `inputs:filtertype` at default time, creating the typed input if needed.
    pub fn set_filtertype(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:filtertype` sample in stage time through the edit target.
    pub fn set_filtertype_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameendaction` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameendaction_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "frameendaction", ty)
    }
    /// Authors `inputs:frameendaction` at default time, creating the typed input if needed.
    pub fn set_frameendaction(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameendaction` sample in stage time through the edit target.
    pub fn set_frameendaction_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameoffset_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "frameoffset", ty)
    }
    /// Authors `inputs:frameoffset` at default time, creating the typed input if needed.
    pub fn set_frameoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameoffset` sample in stage time through the edit target.
    pub fn set_frameoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:framerange` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_framerange_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "framerange", ty)
    }
    /// Authors `inputs:framerange` at default time, creating the typed input if needed.
    pub fn set_framerange(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:framerange` sample in stage time through the edit target.
    pub fn set_framerange_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:realworldimagesize` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_realworldimagesize_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "realworldimagesize", ty)
    }
    /// Authors `inputs:realworldimagesize` at default time, creating the typed input if needed.
    pub fn set_realworldimagesize(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldimagesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:realworldimagesize` sample in stage time through the edit target.
    pub fn set_realworldimagesize_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldimagesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:realworldtilesize` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_realworldtilesize_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "realworldtilesize", ty)
    }
    /// Authors `inputs:realworldtilesize` at default time, creating the typed input if needed.
    pub fn set_realworldtilesize(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldtilesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:realworldtilesize` sample in stage time through the edit target.
    pub fn set_realworldtilesize_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldtilesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:texcoord` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_texcoord_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "texcoord", ty)
    }
    /// Authors `inputs:texcoord` at default time, creating the typed input if needed.
    pub fn set_texcoord(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:texcoord` sample in stage time through the edit target.
    pub fn set_texcoord_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uvoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uvoffset_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "uvoffset", ty)
    }
    /// Authors `inputs:uvoffset` at default time, creating the typed input if needed.
    pub fn set_uvoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uvoffset` sample in stage time through the edit target.
    pub fn set_uvoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uvtiling` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uvtiling_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "uvtiling", ty)
    }
    /// Authors `inputs:uvtiling` at default time, creating the typed input if needed.
    pub fn set_uvtiling(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvtiling_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uvtiling` sample in stage time through the edit target.
    pub fn set_uvtiling_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvtiling_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The `ND_tiledimage_vector3` shader node.
#[derive(Clone, Copy, Debug)]
pub struct NdTiledimageVector3<'a> {
    shader: Shader<'a>,
}
impl<'a> Deref for NdTiledimageVector3<'a> {
    type Target = Shader<'a>;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl<'a> NdTiledimageVector3<'a> {
    /// The shader's definition identifier.
    pub const ID: &'static str = "ND_tiledimage_vector3";
    /// Reads an identifier-based Shader whose composed `info:id` matches.
    #[must_use]
    pub fn get(scene: &Scene<'a>, path: impl layerstack::PrimPathArg) -> Option<Self> {
        let shader = Shader::get(scene, path)?;
        let definition = shader.node_def_api();
        (definition.id() == Some(Self::ID)
            && definition.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then_some(Self { shader })
    }
    /// Defines a Shader with this node's `info:id`, as [`Shader::define`] does; inputs remain unauthored.
    ///
    /// # Errors
    ///
    /// [`CannotEdit`] for a path beneath an instance or in a prototype; nothing is authored.
    pub fn define(
        edit: &mut SchemaEdit<'_>,
        path: PathId,
    ) -> Result<NdTiledimageVector3Edit, CannotEdit> {
        let shader = Shader::define(edit, path)?;
        shader.node_def_api().set_id(edit, Self::ID);
        shader
            .node_def_api()
            .set_implementation_source(edit, NodeDefApiImplementationSource::Id);
        Ok(NdTiledimageVector3Edit { shader })
    }
    /// The MaterialX NodeDef interface, independent of authored scene state.
    pub const DEFINITION: MaterialXNode = MaterialXNode {
        id: Self::ID,
        ports: &[
            MaterialXPort {
                name: "default",
                direction: PortKind::Input,
                type_name: "vector3",
                usd_name: "inputs:default",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "file",
                direction: PortKind::Input,
                type_name: "filename",
                usd_name: "inputs:file",
                usd_type_name: "asset",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "filtertype",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:filtertype",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "linear",
                )),
            },
            MaterialXPort {
                name: "frameendaction",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:frameendaction",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "constant",
                )),
            },
            MaterialXPort {
                name: "frameoffset",
                direction: PortKind::Input,
                type_name: "integer",
                usd_name: "inputs:frameoffset",
                usd_type_name: "int",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0",
                )),
            },
            MaterialXPort {
                name: "framerange",
                direction: PortKind::Input,
                type_name: "string",
                usd_name: "inputs:framerange",
                usd_type_name: "string",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal("")),
            },
            MaterialXPort {
                name: "realworldimagesize",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:realworldimagesize",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "realworldtilesize",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:realworldtilesize",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "texcoord",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:texcoord",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::GeomProp(
                    "UV0",
                )),
            },
            MaterialXPort {
                name: "uvoffset",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:uvoffset",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0",
                )),
            },
            MaterialXPort {
                name: "uvtiling",
                direction: PortKind::Input,
                type_name: "vector2",
                usd_name: "inputs:uvtiling",
                usd_type_name: "float2",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "1.0, 1.0",
                )),
            },
            MaterialXPort {
                name: "out",
                direction: PortKind::Output,
                type_name: "vector3",
                usd_name: "outputs:out",
                usd_type_name: "float3",
                default: Some(layerstack_schemas::materialx::MaterialXDefault::Literal(
                    "0.0, 0.0, 0.0",
                )),
            },
        ],
    };
    #[doc = "The authored `inputs:default` port, if it exists. "]
    #[must_use]
    pub fn default_input(&self) -> Option<Port<'a>> {
        self.shader.input("default")
    }
    #[doc = "The composed `inputs:default`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn default(&self) -> Option<[f32; 3]> {
        self.read_value("inputs:default", layerstack_schemas::value::read_float3)
    }
    /// The composed `inputs:default` at `time`.
    #[must_use]
    pub fn default_at(&self, time: layerstack::Time) -> Option<[f32; 3]> {
        self.read_value_at(
            "inputs:default",
            time,
            layerstack_schemas::value::read_float3,
        )
    }
    /// The `inputs:default` default in the node definition, separate from authored values.
    #[must_use]
    pub fn default_default() -> [f32; 3] {
        [0.0, 0.0, 0.0]
    }
    #[doc = "The authored `inputs:file` port, if it exists. "]
    #[must_use]
    pub fn file_input(&self) -> Option<Port<'a>> {
        self.shader.input("file")
    }
    #[doc = "The composed `inputs:file`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn file(&self) -> Option<Arc<str>> {
        self.read_value("inputs:file", layerstack_schemas::value::read_asset)
    }
    /// The composed `inputs:file` at `time`.
    #[must_use]
    pub fn file_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at("inputs:file", time, layerstack_schemas::value::read_asset)
    }
    /// The `inputs:file` default in the node definition, separate from authored values.
    #[must_use]
    pub fn file_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:filtertype` port, if it exists. "]
    #[must_use]
    pub fn filtertype_input(&self) -> Option<Port<'a>> {
        self.shader.input("filtertype")
    }
    #[doc = "The composed `inputs:filtertype`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn filtertype(&self) -> Option<Arc<str>> {
        self.read_value("inputs:filtertype", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:filtertype` at `time`.
    #[must_use]
    pub fn filtertype_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:filtertype",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:filtertype` default in the node definition, separate from authored values.
    #[must_use]
    pub fn filtertype_default() -> Arc<str> {
        Arc::from("linear")
    }
    #[doc = "The authored `inputs:frameendaction` port, if it exists. "]
    #[must_use]
    pub fn frameendaction_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameendaction")
    }
    #[doc = "The composed `inputs:frameendaction`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameendaction(&self) -> Option<Arc<str>> {
        self.read_value(
            "inputs:frameendaction",
            layerstack_schemas::value::read_string,
        )
    }
    /// The composed `inputs:frameendaction` at `time`.
    #[must_use]
    pub fn frameendaction_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:frameendaction",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:frameendaction` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameendaction_default() -> Arc<str> {
        Arc::from("constant")
    }
    #[doc = "The authored `inputs:frameoffset` port, if it exists. "]
    #[must_use]
    pub fn frameoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("frameoffset")
    }
    #[doc = "The composed `inputs:frameoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn frameoffset(&self) -> Option<i32> {
        self.read_value("inputs:frameoffset", layerstack_schemas::value::read_int)
    }
    /// The composed `inputs:frameoffset` at `time`.
    #[must_use]
    pub fn frameoffset_at(&self, time: layerstack::Time) -> Option<i32> {
        self.read_value_at(
            "inputs:frameoffset",
            time,
            layerstack_schemas::value::read_int,
        )
    }
    /// The `inputs:frameoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn frameoffset_default() -> i32 {
        0_i32
    }
    #[doc = "The authored `inputs:framerange` port, if it exists. "]
    #[must_use]
    pub fn framerange_input(&self) -> Option<Port<'a>> {
        self.shader.input("framerange")
    }
    #[doc = "The composed `inputs:framerange`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn framerange(&self) -> Option<Arc<str>> {
        self.read_value("inputs:framerange", layerstack_schemas::value::read_string)
    }
    /// The composed `inputs:framerange` at `time`.
    #[must_use]
    pub fn framerange_at(&self, time: layerstack::Time) -> Option<Arc<str>> {
        self.read_value_at(
            "inputs:framerange",
            time,
            layerstack_schemas::value::read_string,
        )
    }
    /// The `inputs:framerange` default in the node definition, separate from authored values.
    #[must_use]
    pub fn framerange_default() -> Arc<str> {
        Arc::from("")
    }
    #[doc = "The authored `inputs:realworldimagesize` port, if it exists. "]
    #[must_use]
    pub fn realworldimagesize_input(&self) -> Option<Port<'a>> {
        self.shader.input("realworldimagesize")
    }
    #[doc = "The composed `inputs:realworldimagesize`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn realworldimagesize(&self) -> Option<[f32; 2]> {
        self.read_value(
            "inputs:realworldimagesize",
            layerstack_schemas::value::read_float2,
        )
    }
    /// The composed `inputs:realworldimagesize` at `time`.
    #[must_use]
    pub fn realworldimagesize_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:realworldimagesize",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:realworldimagesize` default in the node definition, separate from authored values.
    #[must_use]
    pub fn realworldimagesize_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `inputs:realworldtilesize` port, if it exists. "]
    #[must_use]
    pub fn realworldtilesize_input(&self) -> Option<Port<'a>> {
        self.shader.input("realworldtilesize")
    }
    #[doc = "The composed `inputs:realworldtilesize`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn realworldtilesize(&self) -> Option<[f32; 2]> {
        self.read_value(
            "inputs:realworldtilesize",
            layerstack_schemas::value::read_float2,
        )
    }
    /// The composed `inputs:realworldtilesize` at `time`.
    #[must_use]
    pub fn realworldtilesize_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:realworldtilesize",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:realworldtilesize` default in the node definition, separate from authored values.
    #[must_use]
    pub fn realworldtilesize_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `inputs:texcoord` port, if it exists. "]
    #[must_use]
    pub fn texcoord_input(&self) -> Option<Port<'a>> {
        self.shader.input("texcoord")
    }
    #[doc = "The composed `inputs:texcoord`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn texcoord(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:texcoord", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:texcoord` at `time`.
    #[must_use]
    pub fn texcoord_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:texcoord",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    #[doc = "The authored `inputs:uvoffset` port, if it exists. "]
    #[must_use]
    pub fn uvoffset_input(&self) -> Option<Port<'a>> {
        self.shader.input("uvoffset")
    }
    #[doc = "The composed `inputs:uvoffset`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uvoffset(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:uvoffset", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:uvoffset` at `time`.
    #[must_use]
    pub fn uvoffset_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:uvoffset",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:uvoffset` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uvoffset_default() -> [f32; 2] {
        [0.0, 0.0]
    }
    #[doc = "The authored `inputs:uvtiling` port, if it exists. "]
    #[must_use]
    pub fn uvtiling_input(&self) -> Option<Port<'a>> {
        self.shader.input("uvtiling")
    }
    #[doc = "The composed `inputs:uvtiling`, without node defaults or connection evaluation. "]
    #[must_use]
    pub fn uvtiling(&self) -> Option<[f32; 2]> {
        self.read_value("inputs:uvtiling", layerstack_schemas::value::read_float2)
    }
    /// The composed `inputs:uvtiling` at `time`.
    #[must_use]
    pub fn uvtiling_at(&self, time: layerstack::Time) -> Option<[f32; 2]> {
        self.read_value_at(
            "inputs:uvtiling",
            time,
            layerstack_schemas::value::read_float2,
        )
    }
    /// The `inputs:uvtiling` default in the node definition, separate from authored values.
    #[must_use]
    pub fn uvtiling_default() -> [f32; 2] {
        [1.0, 1.0]
    }
    #[doc = "The authored `outputs:out` port, if it exists. "]
    #[must_use]
    pub fn out_output(&self) -> Option<Port<'a>> {
        self.shader.output("out")
    }
}

/// Authors `ND_tiledimage_vector3` inputs and typed ports through an explicit edit target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NdTiledimageVector3Edit {
    shader: ShaderEdit,
}
impl Deref for NdTiledimageVector3Edit {
    type Target = ShaderEdit;
    fn deref(&self) -> &Self::Target {
        &self.shader
    }
}
impl NdTiledimageVector3Edit {
    /// A handle authoring this node's inputs on the shader `shader` authors, whatever its `info:id`.
    #[must_use]
    pub fn from_shader(shader: ShaderEdit) -> Self {
        Self { shader }
    }
    /// Creates `inputs:default` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_default_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_input(edit, "default", ty)
    }
    /// Authors `inputs:default` at default time, creating the typed input if needed.
    pub fn set_default(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:default` sample in stage time through the edit target.
    pub fn set_default_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 3],
    ) -> Result<&Self, PortError> {
        let port = self.create_default_input(edit)?;
        let value = (layerstack_schemas::value::write_float3)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:file` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_file_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("asset").expect("a registered value type");
        self.shader.create_input(edit, "file", ty)
    }
    /// Authors `inputs:file` at default time, creating the typed input if needed.
    pub fn set_file(&self, edit: &mut SchemaEdit<'_>, value: &str) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:file` sample in stage time through the edit target.
    pub fn set_file_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_file_input(edit)?;
        let value = (layerstack_schemas::value::write_asset)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:filtertype` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_filtertype_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "filtertype", ty)
    }
    /// Authors `inputs:filtertype` at default time, creating the typed input if needed.
    pub fn set_filtertype(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:filtertype` sample in stage time through the edit target.
    pub fn set_filtertype_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_filtertype_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameendaction` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameendaction_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "frameendaction", ty)
    }
    /// Authors `inputs:frameendaction` at default time, creating the typed input if needed.
    pub fn set_frameendaction(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameendaction` sample in stage time through the edit target.
    pub fn set_frameendaction_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameendaction_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:frameoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_frameoffset_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("int").expect("a registered value type");
        self.shader.create_input(edit, "frameoffset", ty)
    }
    /// Authors `inputs:frameoffset` at default time, creating the typed input if needed.
    pub fn set_frameoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:frameoffset` sample in stage time through the edit target.
    pub fn set_frameoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: i32,
    ) -> Result<&Self, PortError> {
        let port = self.create_frameoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_int)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:framerange` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_framerange_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("string").expect("a registered value type");
        self.shader.create_input(edit, "framerange", ty)
    }
    /// Authors `inputs:framerange` at default time, creating the typed input if needed.
    pub fn set_framerange(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:framerange` sample in stage time through the edit target.
    pub fn set_framerange_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &str,
    ) -> Result<&Self, PortError> {
        let port = self.create_framerange_input(edit)?;
        let value = (layerstack_schemas::value::write_string)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:realworldimagesize` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_realworldimagesize_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "realworldimagesize", ty)
    }
    /// Authors `inputs:realworldimagesize` at default time, creating the typed input if needed.
    pub fn set_realworldimagesize(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldimagesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:realworldimagesize` sample in stage time through the edit target.
    pub fn set_realworldimagesize_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldimagesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:realworldtilesize` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_realworldtilesize_input(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "realworldtilesize", ty)
    }
    /// Authors `inputs:realworldtilesize` at default time, creating the typed input if needed.
    pub fn set_realworldtilesize(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldtilesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:realworldtilesize` sample in stage time through the edit target.
    pub fn set_realworldtilesize_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_realworldtilesize_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:texcoord` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_texcoord_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "texcoord", ty)
    }
    /// Authors `inputs:texcoord` at default time, creating the typed input if needed.
    pub fn set_texcoord(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:texcoord` sample in stage time through the edit target.
    pub fn set_texcoord_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_texcoord_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uvoffset` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uvoffset_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "uvoffset", ty)
    }
    /// Authors `inputs:uvoffset` at default time, creating the typed input if needed.
    pub fn set_uvoffset(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uvoffset` sample in stage time through the edit target.
    pub fn set_uvoffset_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvoffset_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `inputs:uvtiling` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_uvtiling_input(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float2").expect("a registered value type");
        self.shader.create_input(edit, "uvtiling", ty)
    }
    /// Authors `inputs:uvtiling` at default time, creating the typed input if needed.
    pub fn set_uvtiling(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvtiling_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set(edit, value)?;
        Ok(self)
    }
    /// Authors a `inputs:uvtiling` sample in stage time through the edit target.
    pub fn set_uvtiling_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: [f32; 2],
    ) -> Result<&Self, PortError> {
        let port = self.create_uvtiling_input(edit)?;
        let value = (layerstack_schemas::value::write_float2)(value, edit.tokens());
        port.set_at(edit, time, value)?;
        Ok(self)
    }
    /// Creates `outputs:out` with its node-defined USD type for values or connections.
    /// Rejects existing ports of a different type before appending edits.
    pub fn create_out_output(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {
        let ty = PropertyType::parse("float3").expect("a registered value type");
        self.shader.create_output(edit, "out", ty)
    }
}

/// The selected MaterialX node definitions, in identifier order.
pub const NODE_DEFINITIONS: &[MaterialXNode] = &[
    NdAddColor3::DEFINITION,
    NdAddFloat::DEFINITION,
    NdBumpVector3::DEFINITION,
    NdClampFloat::DEFINITION,
    NdColorcorrectColor3::DEFINITION,
    NdCombine3Color3::DEFINITION,
    NdExtractColor3::DEFINITION,
    NdExtractVector3::DEFINITION,
    NdGltfPbrSurfaceshader::DEFINITION,
    NdImageColor3::DEFINITION,
    NdImageColor4::DEFINITION,
    NdImageFloat::DEFINITION,
    NdImageVector3::DEFINITION,
    NdInvertFloat::DEFINITION,
    NdMixColor3::DEFINITION,
    NdMixFloat::DEFINITION,
    NdMultiplyColor3::DEFINITION,
    NdMultiplyColor3Fa::DEFINITION,
    NdMultiplyFloat::DEFINITION,
    NdNormalmapFloat::DEFINITION,
    NdOpenPbrSurfaceSurfaceshader::DEFINITION,
    NdPlace2dVector2::DEFINITION,
    NdRemapFloat::DEFINITION,
    NdSeparate3Vector3::DEFINITION,
    NdStandardSurfaceSurfaceshader::DEFINITION,
    NdTexcoordVector2::DEFINITION,
    NdTiledimageColor3::DEFINITION,
    NdTiledimageFloat::DEFINITION,
    NdTiledimageVector3::DEFINITION,
];
