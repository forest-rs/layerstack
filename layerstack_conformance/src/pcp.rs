//! Minimal loader for the supplemental `pcp.json` expectation files.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Pcp {
    #[serde(rename = "Entry")]
    pub entry: String,

    #[serde(rename = "Composing")]
    pub composing: BTreeMap<String, PcpPrim>,

    #[serde(rename = "Layer Stack")]
    pub layer_stack: Vec<String>,

    #[serde(rename = "Errors")]
    pub errors: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct PcpPrim {
    #[serde(rename = "Prim Stack")]
    pub prim_stack: Option<BTreeMap<String, String>>,

    #[serde(rename = "Child names")]
    pub child_names: Option<Vec<String>>,

    #[serde(rename = "Property names")]
    pub property_names: Option<Vec<String>>,

    #[serde(rename = "Property stacks")]
    pub property_stacks: Option<BTreeMap<String, BTreeMap<String, String>>>,

    #[serde(rename = "Relationship targets")]
    pub relationship_targets: Option<BTreeMap<String, Vec<String>>>,

    #[serde(rename = "Attribute connections")]
    pub attribute_connections: Option<BTreeMap<String, Vec<String>>>,
}

pub fn load_pcp_json(path: &std::path::Path) -> Pcp {
    let text = std::fs::read_to_string(path).expect("read pcp.json");
    serde_json::from_str::<Pcp>(&text).expect("parse pcp.json")
}
