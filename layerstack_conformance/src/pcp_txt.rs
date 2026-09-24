// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lossless loader for the supplemental `pcp.txt` composition baselines.
//!
//! Each `pcp.txt` is the captured output of OpenUSD's
//! `pxr/usd/pcp/testenv/testPcpCompositionResults.py --usd <entry>` (see
//! `core-spec-supplemental-release_dec2025/composition/tests/assets/pcpConverter.py`),
//! with the script's stderr appended after a dashed separator. The prim
//! stacks it prints are `PcpPrimIndex::GetPrimStack()` and the property stacks
//! are `PcpPropertyIndex::GetPropertyStack()`, both strongest-first.
//!
//! The sibling `pcp.json` files are lossy: `converter.py` keys every stack by
//! layer name, so several specs from one layer overwrite each other, and
//! [`crate::pcp`] stores them in a `BTreeMap`, which loses order. This module
//! keeps every stack as an ordered `Vec`, including repeated occurrences of
//! the same `(layer, path)` site (diamonds, duplicate sublayers, sublayer
//! cycles).
//!
//! Parsing is strict: an unrecognized section or malformed stack line is an
//! error rather than silently dropped content.

use std::path::Path;

/// One `(layer, path)` entry of a prim or property stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcpSite {
    /// Layer label, relative to the entry layer's directory (e.g. `ref.usd`).
    pub layer: String,
    /// Spec path in that layer, including variant selections
    /// (e.g. `/Model{vset=a}Child.attr`).
    pub path: String,
}

/// An ordered list of target paths for one property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcpTargets {
    /// Composed property path (e.g. `/Prim.rel`).
    pub property: String,
    /// Target paths in composed order.
    pub targets: Vec<String>,
}

/// The ordered property stack for one property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcpPropertyStack {
    /// Composed property path (e.g. `/Prim.attr`).
    pub property: String,
    /// Property specs, strongest first, repeats kept.
    pub stack: Vec<PcpSite>,
}

/// Results printed for one composed prim.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcpTxtPrim {
    /// Composed prim path.
    pub path: String,
    /// Prim specs, strongest first, repeats kept.
    pub prim_stack: Vec<PcpSite>,
    /// Raw `Time Offsets` lines (indentation trimmed).
    pub time_offsets: Vec<String>,
    /// Applied variant selections as `(set, selection)`, sorted by set name.
    pub variant_selections: Vec<(String, String)>,
    /// Composed child names in order.
    pub child_names: Vec<String>,
    /// Prohibited child names (sorted by the generator).
    pub prohibited_child_names: Vec<String>,
    /// Composed property names in order.
    pub property_names: Vec<String>,
    /// Property stacks, sorted by property path by the generator.
    pub property_stacks: Vec<PcpPropertyStack>,
    /// Composed relationship targets.
    pub relationship_targets: Vec<PcpTargets>,
    /// Composed attribute connections.
    pub attribute_connections: Vec<PcpTargets>,
    /// Deleted target paths.
    pub deleted_target_paths: Vec<PcpTargets>,
}

/// A parsed `pcp.txt`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcpTxt {
    /// Entry layer file name (e.g. `root.usd`).
    pub entry: String,
    /// Root layer stack, strongest first, repeats kept.
    pub layer_stack: Vec<String>,
    /// Composed prims in the generator's depth-first traversal order.
    pub prims: Vec<PcpTxtPrim>,
    /// Diagnostic blocks from the generator's stderr, verbatim.
    pub diagnostics: Vec<String>,
}

impl PcpTxt {
    /// Returns the results for `path`, if the generator composed it.
    #[must_use]
    pub fn prim(&self, path: &str) -> Option<&PcpTxtPrim> {
        self.prims.iter().find(|prim| prim.path == path)
    }
}

/// Reads and parses a `pcp.txt` file, panicking with the file path on error.
#[must_use]
pub fn load_pcp_txt(path: &Path) -> PcpTxt {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    parse_pcp_txt(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn is_separator(line: &str) -> bool {
    line.len() >= 10 && line.bytes().all(|b| b == b'-')
}

fn is_section_header(line: &str) -> bool {
    line.ends_with(':')
        && line.starts_with(|c: char| c.is_ascii_uppercase())
        && line[..line.len() - 1]
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == ' ')
}

/// Parses the text of a `pcp.txt` file.
///
/// # Errors
///
/// Returns a message naming the offending line when the text does not follow
/// the `testPcpCompositionResults.py` layout.
pub fn parse_pcp_txt(text: &str) -> Result<PcpTxt, String> {
    let mut out = PcpTxt::default();
    let lines: Vec<&str> = text.lines().collect();

    // Split into blocks at dashed separators, remembering 1-based line numbers.
    let mut blocks: Vec<Vec<(usize, &str)>> = vec![Vec::new()];
    for (i, line) in lines.iter().enumerate() {
        let line = line.trim_end();
        if is_separator(line) {
            blocks.push(Vec::new());
        } else {
            blocks.last_mut().expect("block").push((i + 1, line));
        }
    }

    let mut blocks = blocks.into_iter();
    let header = blocks.next().unwrap_or_default();
    let (_, loading) = header
        .iter()
        .copied()
        .find(|(_, l)| !l.is_empty())
        .ok_or("missing `Loading @...@` header")?;
    let entry = loading
        .strip_prefix("Loading @")
        .and_then(|rest| rest.strip_suffix('@'))
        .ok_or_else(|| format!("unexpected header line {loading:?}"))?;
    out.entry = entry.rsplit('/').next().unwrap_or(entry).to_string();

    for block in blocks {
        let mut content = block.iter().copied().skip_while(|(_, l)| l.is_empty());
        let Some((line_no, first)) = content.next() else {
            continue;
        };
        if first == "Layer Stack:" {
            for (_, line) in content {
                let layer = line.trim();
                if !layer.is_empty() {
                    out.layer_stack.push(layer.to_string());
                }
            }
        } else if let Some(rest) = first.strip_prefix("Results for composing <") {
            let path = rest
                .strip_suffix('>')
                .ok_or_else(|| format!("line {line_no}: malformed {first:?}"))?;
            out.prims.push(parse_prim(path, content)?);
        } else {
            // Everything else is stderr: `Errors while composing <...>`
            // blocks, layer stack errors, and the trailing `ERROR:` line.
            let mut text = String::from(first);
            for (_, line) in content {
                text.push('\n');
                text.push_str(line);
            }
            out.diagnostics.push(text.trim_end().to_string());
        }
    }
    Ok(out)
}

fn parse_site(line_no: usize, line: &str) -> Result<PcpSite, String> {
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some(layer), Some(path), None) if path.starts_with('/') => Ok(PcpSite {
            layer: layer.to_string(),
            path: path.to_string(),
        }),
        _ => Err(format!("line {line_no}: malformed stack entry {line:?}")),
    }
}

/// Parses a Python list repr of identifiers, e.g. `['a', 'b']`.
fn parse_name_list(line_no: usize, line: &str) -> Result<Vec<String>, String> {
    let inner = line
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| format!("line {line_no}: malformed name list {line:?}"))?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    inner
        .split(',')
        .map(|item| {
            let item = item.trim();
            item.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .map(str::to_string)
                .ok_or_else(|| format!("line {line_no}: malformed name {item:?}"))
        })
        .collect()
}

fn parse_prim<'a>(
    path: &str,
    lines: impl Iterator<Item = (usize, &'a str)>,
) -> Result<PcpTxtPrim, String> {
    let mut prim = PcpTxtPrim {
        path: path.to_string(),
        ..PcpTxtPrim::default()
    };
    let mut section: Option<&str> = None;
    for (line_no, line) in lines {
        if line.is_empty() {
            continue;
        }
        if is_section_header(line) {
            section = Some(&line[..line.len() - 1]);
            continue;
        }
        let keyed_header = line.starts_with('/') && line.ends_with(':');
        match section {
            Some("Prim Stack") => prim.prim_stack.push(parse_site(line_no, line)?),
            Some("Time Offsets") => prim.time_offsets.push(line.trim().to_string()),
            Some("Variant Selections") => {
                let pair = line
                    .trim()
                    .strip_prefix('{')
                    .and_then(|rest| rest.strip_suffix('}'))
                    .and_then(|inner| inner.split_once(" = "))
                    .ok_or_else(|| format!("line {line_no}: malformed selection {line:?}"))?;
                prim.variant_selections
                    .push((pair.0.to_string(), pair.1.to_string()));
            }
            Some("Child names") => prim.child_names = parse_name_list(line_no, line)?,
            Some("Prohibited child names") => {
                prim.prohibited_child_names = parse_name_list(line_no, line)?;
            }
            Some("Property names") => prim.property_names = parse_name_list(line_no, line)?,
            Some("Property stacks") if keyed_header => {
                prim.property_stacks.push(PcpPropertyStack {
                    property: line[..line.len() - 1].to_string(),
                    stack: Vec::new(),
                });
            }
            Some("Property stacks") => prim
                .property_stacks
                .last_mut()
                .ok_or_else(|| format!("line {line_no}: stack entry before property"))?
                .stack
                .push(parse_site(line_no, line)?),
            Some(
                name @ ("Relationship targets" | "Attribute connections" | "Deleted target paths"),
            ) => {
                let list = match name {
                    "Relationship targets" => &mut prim.relationship_targets,
                    "Attribute connections" => &mut prim.attribute_connections,
                    _ => &mut prim.deleted_target_paths,
                };
                if keyed_header {
                    list.push(PcpTargets {
                        property: line[..line.len() - 1].to_string(),
                        targets: Vec::new(),
                    });
                } else {
                    list.last_mut()
                        .ok_or_else(|| format!("line {line_no}: target before property"))?
                        .targets
                        .push(line.trim().to_string());
                }
            }
            Some(other) => {
                return Err(format!("line {line_no}: unknown section {other:?}"));
            }
            None => return Err(format!("line {line_no}: content outside a section")),
        }
    }
    Ok(prim)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Loading @composition/tests/assets/Sample_root/usda/root.usd@

------------------------------------------------------------------------
Layer Stack:
     root.usd
     sub.usd
     sub.usd

------------------------------------------------------------------------
Results for composing </Root>

Prim Stack:
    root.usd             /Root
    A.usd                /A
    C.usd                /C
    B.usd                /B
    C.usd                /C
    model.usd            /Model{vset=a}

Variant Selections:
    {vset = a}

Child names:
     ['Child', 'Other']

Prohibited child names:
     ['Gone']

Property names:
     ['attr', 'rel']

Property stacks:
/Root.attr:
    C.usd                /C.attr
    C.usd                /C.attr
/Root.rel:
    root.usd             /Root.rel

Relationship targets:
/Root.rel:
    /Root/Child
    /Root/Other

------------------------------------------------------------------------
Results for composing </Root/Child>

Prim Stack:
    B.usd                /B/Child

Time Offsets:
    root.usd             /Root/Child     root       (offset=0.00, scale=1.00)
        sub.usd                          sublayer   (offset=20.00, scale=1.00)


------------------------------------------------------------------------
Errors while composing </Root/Child>

Something went wrong.

ERROR: Unexpected error(s) encountered during test!
";

    fn site(layer: &str, path: &str) -> PcpSite {
        PcpSite {
            layer: layer.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn keeps_order_and_repeats() {
        let parsed = parse_pcp_txt(SAMPLE).expect("parse");
        assert_eq!(parsed.entry, "root.usd");
        assert_eq!(parsed.layer_stack, ["root.usd", "sub.usd", "sub.usd"]);
        assert_eq!(parsed.prims.len(), 2);

        let root = parsed.prim("/Root").expect("/Root");
        assert_eq!(
            root.prim_stack,
            [
                site("root.usd", "/Root"),
                site("A.usd", "/A"),
                site("C.usd", "/C"),
                site("B.usd", "/B"),
                site("C.usd", "/C"),
                site("model.usd", "/Model{vset=a}"),
            ]
        );
        assert_eq!(
            root.variant_selections,
            [("vset".to_string(), "a".to_string())]
        );
        assert_eq!(root.child_names, ["Child", "Other"]);
        assert_eq!(root.prohibited_child_names, ["Gone"]);
        assert_eq!(root.property_names, ["attr", "rel"]);
        assert_eq!(root.property_stacks.len(), 2);
        assert_eq!(root.property_stacks[0].property, "/Root.attr");
        assert_eq!(
            root.property_stacks[0].stack,
            [site("C.usd", "/C.attr"), site("C.usd", "/C.attr")]
        );
        assert_eq!(
            root.relationship_targets,
            [PcpTargets {
                property: "/Root.rel".to_string(),
                targets: vec!["/Root/Child".to_string(), "/Root/Other".to_string()],
            }]
        );
    }

    #[test]
    fn stderr_blocks_become_diagnostics() {
        let parsed = parse_pcp_txt(SAMPLE).expect("parse");
        let child = parsed.prim("/Root/Child").expect("/Root/Child");
        assert_eq!(child.prim_stack, [site("B.usd", "/B/Child")]);
        assert_eq!(child.time_offsets.len(), 2);
        assert_eq!(
            parsed.diagnostics,
            [
                "Errors while composing </Root/Child>\n\nSomething went wrong.\n\nERROR: Unexpected error(s) encountered during test!"
            ]
        );
    }

    #[test]
    fn rejects_unknown_sections_and_malformed_entries() {
        let unknown =
            "Loading @a/root.usd@\n----------\nResults for composing </A>\n\nMystery:\n    x\n";
        assert!(parse_pcp_txt(unknown).unwrap_err().contains("Mystery"));

        let malformed = "Loading @a/root.usd@\n----------\nResults for composing </A>\n\nPrim Stack:\n    root.usd\n";
        assert!(parse_pcp_txt(malformed).unwrap_err().contains("malformed"));
    }
}
