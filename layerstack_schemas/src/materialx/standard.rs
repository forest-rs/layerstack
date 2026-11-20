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
