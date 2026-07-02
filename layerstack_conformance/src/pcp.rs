// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

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

    /// Time offset entries for this prim (§12.3.2.1).
    #[serde(rename = "Time Offsets")]
    pub time_offsets: Option<Vec<PcpTimeOffset>>,
}

/// A time offset entry from the supplemental composition `pcp.json` files.
///
/// Each entry represents an arc boundary (root, reference, payload) or sublayer
/// with its accumulated offset and scale.
#[derive(Debug, Deserialize)]
pub struct PcpTimeOffset {
    pub layer: String,
    pub prim: Option<String>,
    #[serde(rename = "type")]
    pub arc_type: String,
    pub offset: String,
    pub scale: String,
    #[serde(default)]
    pub children: Vec<PcpTimeOffsetChild>,
}

/// A sublayer child within a [`PcpTimeOffset`] entry.
#[derive(Debug, Deserialize)]
pub struct PcpTimeOffsetChild {
    pub layer: String,
    #[serde(rename = "type")]
    pub arc_type: String,
    pub offset: String,
    pub scale: String,
}

pub fn load_pcp_json(path: &std::path::Path) -> Pcp {
    let text = std::fs::read_to_string(path).expect("read pcp.json");
    serde_json::from_str::<Pcp>(&text).expect("parse pcp.json")
}
