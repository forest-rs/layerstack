// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Section parsers for the six USDC sections.
//!
//! Each section is located via the TOC and parsed into in-memory structures
//! that the assembler can consume.
//!
//! Spec: AOUSD Core §16.3.5–§16.3.8.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::compression::{lz4_decompress, read_compressed_ints};
use crate::error::UsdcError;
use crate::toc::{SectionEntry, Toc};
use crate::value_type::SpecForm;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A field definition: a token index and a raw 8-byte value representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldDef {
    /// Index into the tokens table for the field name.
    pub token_index: u32,
    /// Raw 8-byte value representation (decoded later by `value_rep`).
    pub value_rep: [u8; 8],
}

/// A spec definition: a path index, fieldset index, and spec form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpecDef {
    /// Index into the paths table.
    pub path_index: u32,
    /// Start index into the fieldsets flat array.
    pub fieldset_index: u32,
    /// The spec form (Prim, Attribute, etc.).
    pub form: SpecForm,
}

/// All parsed section data from a USDC file.
///
/// Holds the decoded contents of all six sections ready for scene assembly.
#[derive(Clone, Debug)]
pub struct CrateSections {
    /// Decoded token strings from the TOKENS section.
    pub tokens: Vec<String>,
    /// String indices (into `tokens`) from the STRINGS section.
    pub strings: Vec<u32>,
    /// Field definitions from the FIELDS section.
    pub fields: Vec<FieldDef>,
    /// Flat fieldset array from the FIELDSETS section.
    ///
    /// Groups are delimited by negative values.
    pub fieldsets: Vec<i32>,
    /// Reconstructed SDF path strings from the PATHS section.
    pub paths: Vec<String>,
    /// Spec definitions from the SPECS section.
    pub specs: Vec<SpecDef>,
}

// ---------------------------------------------------------------------------
// Top-level parse entry point
// ---------------------------------------------------------------------------

/// Parses all six sections from the given file data and TOC.
///
/// Sections that are absent from the TOC produce empty vectors.
pub fn parse_sections(data: &[u8], toc: &Toc) -> Result<CrateSections, UsdcError> {
    let tokens = if let Some(entry) = toc.tokens {
        parse_tokens(data, &entry)?
    } else {
        vec![]
    };

    let strings = if let Some(entry) = toc.strings {
        parse_strings(data, &entry)?
    } else {
        vec![]
    };

    let fields = if let Some(entry) = toc.fields {
        parse_fields(data, &entry)?
    } else {
        vec![]
    };

    let fieldsets = if let Some(entry) = toc.fieldsets {
        parse_fieldsets(data, &entry)?
    } else {
        vec![]
    };

    let paths = if let Some(entry) = toc.paths {
        parse_paths(data, &entry, &tokens)?
    } else {
        vec![]
    };

    let specs = if let Some(entry) = toc.specs {
        parse_specs(data, &entry)?
    } else {
        vec![]
    };

    Ok(CrateSections {
        tokens,
        strings,
        fields,
        fieldsets,
        paths,
        specs,
    })
}

// ---------------------------------------------------------------------------
// TOKENS section
// ---------------------------------------------------------------------------

/// Parses the TOKENS section.
///
/// Format: `num_tokens: u64`, `uncompressed_size: u64`,
/// `compressed_size: u64`, then `compressed_size` bytes of LZ4 data.
/// Decompressed data is a null-delimited, null-terminated list of UTF-8
/// strings.
///
/// Spec: AOUSD Core §16.3.5.
fn parse_tokens(data: &[u8], entry: &SectionEntry) -> Result<Vec<String>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 24 {
        return Err(UsdcError::UnexpectedEof {
            section: "TOKENS",
            offset: entry.offset,
            expected: 24,
        });
    }

    let num_tokens = read_u64(section, 0);
    let uncompressed_size = read_u64(section, 8);
    let compressed_size = read_u64(section, 16);

    #[allow(
        clippy::cast_possible_truncation,
        reason = "section data sizes are well under 4 GiB"
    )]
    let csz = compressed_size as usize;
    if section.len() < 24 + csz {
        return Err(UsdcError::UnexpectedEof {
            section: "TOKENS compressed data",
            offset: entry.offset + 24,
            expected: compressed_size,
        });
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "section data sizes are well under 4 GiB"
    )]
    let usz = uncompressed_size as usize;
    let decompressed = lz4_decompress(&section[24..24 + csz], usz)?;

    // Split on null bytes; the final null produces a trailing empty string.
    let text = core::str::from_utf8(&decompressed).map_err(|_| UsdcError::Inconsistent {
        message: "TOKENS section contains invalid UTF-8",
    })?;

    let mut tokens: Vec<String> = text.split('\0').map(String::from).collect();
    // Remove trailing empty string from the final null terminator.
    if tokens.last().is_some_and(|s| s.is_empty()) {
        tokens.pop();
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "num_tokens bounded by file size"
    )]
    let expected = num_tokens as usize;
    if tokens.len() != expected {
        return Err(UsdcError::Inconsistent {
            message: "TOKENS count mismatch",
        });
    }

    Ok(tokens)
}

// ---------------------------------------------------------------------------
// STRINGS section
// ---------------------------------------------------------------------------

/// Parses the STRINGS section.
///
/// Format: `num_strings: u64`, then `num_strings × u32` token indices.
///
/// Spec: AOUSD Core §16.3.5.
fn parse_strings(data: &[u8], entry: &SectionEntry) -> Result<Vec<u32>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "STRINGS",
            offset: entry.offset,
            expected: 8,
        });
    }

    let num_strings = read_u64(section, 0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "string count bounded by file size"
    )]
    let count = num_strings as usize;
    let needed = 8 + count * 4;
    if section.len() < needed {
        return Err(UsdcError::UnexpectedEof {
            section: "STRINGS indices",
            offset: entry.offset + 8,
            expected: (count * 4) as u64,
        });
    }

    let mut indices = Vec::with_capacity(count);
    for i in 0..count {
        let off = 8 + i * 4;
        let idx = u32::from_le_bytes(section[off..off + 4].try_into().unwrap());
        indices.push(idx);
    }

    Ok(indices)
}

// ---------------------------------------------------------------------------
// FIELDS section
// ---------------------------------------------------------------------------

/// Parses the FIELDS section.
///
/// Format: `num_fields: u64`, compressed integer array of token indices
/// (`u32`), `reps_size: u64`, then `reps_size` bytes of LZ4-compressed
/// value reps (8 bytes each).
///
/// Spec: AOUSD Core §16.3.6.
fn parse_fields(data: &[u8], entry: &SectionEntry) -> Result<Vec<FieldDef>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "FIELDS",
            offset: entry.offset,
            expected: 8,
        });
    }

    let num_fields = read_u64(section, 0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "field count bounded by file size"
    )]
    let count = num_fields as usize;

    // Read compressed token indices.
    let (indices_i64, indices_consumed) = read_compressed_ints(&section[8..], count, 4)?;

    // Read reps_size and LZ4-compressed value reps.
    let reps_start = 8 + indices_consumed;
    if section.len() < reps_start + 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "FIELDS reps_size",
            offset: entry.offset + reps_start as u64,
            expected: 8,
        });
    }

    let reps_size = read_u64(section, reps_start);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "reps data sizes are well under 4 GiB"
    )]
    let rsz = reps_size as usize;
    let reps_data_start = reps_start + 8;
    if section.len() < reps_data_start + rsz {
        return Err(UsdcError::UnexpectedEof {
            section: "FIELDS reps data",
            offset: entry.offset + reps_data_start as u64,
            expected: reps_size,
        });
    }

    let uncompressed_reps_size = count * 8;
    let decompressed_reps = lz4_decompress(
        &section[reps_data_start..reps_data_start + rsz],
        uncompressed_reps_size,
    )?;

    let mut fields = Vec::with_capacity(count);
    for (i, &idx_val) in indices_i64.iter().enumerate() {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "token indices are u32-range values"
        )]
        let token_index = idx_val as u32;
        let rep_off = i * 8;
        let mut value_rep = [0_u8; 8];
        value_rep.copy_from_slice(&decompressed_reps[rep_off..rep_off + 8]);
        fields.push(FieldDef {
            token_index,
            value_rep,
        });
    }

    Ok(fields)
}

// ---------------------------------------------------------------------------
// FIELDSETS section
// ---------------------------------------------------------------------------

/// Parses the FIELDSETS section.
///
/// Format: `num_fieldsets: u64`, compressed integer array of `i32` values.
/// Negative values act as group delimiters.
///
/// Spec: AOUSD Core §16.3.7.
fn parse_fieldsets(data: &[u8], entry: &SectionEntry) -> Result<Vec<i32>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "FIELDSETS",
            offset: entry.offset,
            expected: 8,
        });
    }

    let num = read_u64(section, 0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "fieldset count bounded by file size"
    )]
    let count = num as usize;

    let (values, _consumed) = read_compressed_ints(&section[8..], count, 4)?;

    #[allow(
        clippy::cast_possible_truncation,
        reason = "fieldset values are i32-range"
    )]
    let fieldsets: Vec<i32> = values.iter().map(|&v| v as i32).collect();
    Ok(fieldsets)
}

// ---------------------------------------------------------------------------
// PATHS section
// ---------------------------------------------------------------------------

/// Parses the PATHS section and reconstructs SDF path strings.
///
/// Format: `num_paths: u64`, `num_encoded: u64`, then three compressed
/// integer arrays: `path_indices`, `element_token_indices`, `jumps`.
///
/// The path reconstruction algorithm builds SDF paths recursively using
/// a token table and the three index arrays.
///
/// Spec: AOUSD Core §16.3.7.
fn parse_paths(
    data: &[u8],
    entry: &SectionEntry,
    tokens: &[String],
) -> Result<Vec<String>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 16 {
        return Err(UsdcError::UnexpectedEof {
            section: "PATHS",
            offset: entry.offset,
            expected: 16,
        });
    }

    let num_paths = read_u64(section, 0);
    let num_encoded = read_u64(section, 8);

    #[allow(
        clippy::cast_possible_truncation,
        reason = "path counts bounded by file size"
    )]
    let n_paths = num_paths as usize;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "path counts bounded by file size"
    )]
    let n_encoded = num_encoded as usize;

    let mut cursor = &section[16..];
    let (path_indices, c1) = read_compressed_ints(cursor, n_encoded, 4)?;
    cursor = &cursor[c1..];
    let (element_token_indices, c2) = read_compressed_ints(cursor, n_encoded, 4)?;
    cursor = &cursor[c2..];
    let (jumps, _c3) = read_compressed_ints(cursor, n_encoded, 4)?;

    // Reconstruct paths.
    let mut paths = vec![String::new(); n_paths];
    build_paths(
        &path_indices,
        &element_token_indices,
        &jumps,
        tokens,
        &mut paths,
    )?;

    Ok(paths)
}

/// Iterative path reconstruction using an explicit stack.
///
/// Mirrors the Python reference `build_decompressed_paths` but avoids
/// recursion by using a work stack of `(start_index, parent_path)` frames.
///
/// The Python algorithm is a `do-while` loop that continues as long as
/// `has_child or has_sibling`. When there are siblings but no children
/// (`jump >= 0` and `jump != -1`, specifically `jump == 0`), the loop
/// still advances `start_index += 1` to process the next entry as a
/// sibling under the same parent.
fn build_paths(
    path_indices: &[i64],
    element_token_indices: &[i64],
    jumps: &[i64],
    tokens: &[String],
    paths: &mut [String],
) -> Result<(), UsdcError> {
    if path_indices.is_empty() {
        return Ok(());
    }

    // Stack frames: (start_index, parent_path, is_first_iteration)
    let mut stack: Vec<(usize, String, bool)> = vec![(0, String::new(), true)];

    while let Some((mut idx, mut parent, mut first)) = stack.pop() {
        // do-while: always execute at least once per stack frame,
        // then continue while has_child || has_sibling.
        loop {
            if idx >= path_indices.len() {
                break;
            }

            #[allow(
                clippy::cast_possible_truncation,
                reason = "path_indices are u32-range"
            )]
            let target = path_indices[idx] as usize;

            if first && parent.is_empty() {
                // Root node.
                parent = String::from("/");
                if target < paths.len() {
                    paths[target] = parent.clone();
                }
            } else {
                let token_idx_raw = element_token_indices[idx];
                let is_property = token_idx_raw < 0;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "token indices are u32-range"
                )]
                let token_idx = token_idx_raw.unsigned_abs() as usize;

                if token_idx >= tokens.len() {
                    return Err(UsdcError::PathReconstruction);
                }

                let element = &tokens[token_idx];
                let sep = if is_property { "." } else { "/" };
                let path = if parent == "/" {
                    alloc::format!("{sep}{element}")
                } else {
                    alloc::format!("{parent}{sep}{element}")
                };

                if target < paths.len() {
                    paths[target] = path;
                }
            }
            first = false;

            let jump = jumps[idx];
            let has_child = jump > 0 || jump == -1;
            let has_sibling = jump >= 0;

            if has_child {
                if has_sibling {
                    // Push sibling for later processing with the same parent.
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "jump values are small offsets"
                    )]
                    let sibling_idx = idx + jump as usize;
                    stack.push((sibling_idx, parent.clone(), false));
                }

                // Descend into child: advance to next index with current path
                // as new parent.
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "path_indices are u32-range"
                )]
                let target = path_indices[idx] as usize;
                if target < paths.len() {
                    parent = paths[target].clone();
                }
                idx += 1;
            } else if has_sibling {
                // No children but has sibling: advance to the next entry
                // which is processed as a sibling under the same parent.
                idx += 1;
            } else {
                // Leaf with no siblings — done with this branch.
                break;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// SPECS section
// ---------------------------------------------------------------------------

/// Parses the SPECS section.
///
/// Format: `num_specs: u64`, then three compressed integer arrays:
/// `path_indices`, `fieldset_indices`, `forms`.
///
/// Spec: AOUSD Core §16.3.8.
fn parse_specs(data: &[u8], entry: &SectionEntry) -> Result<Vec<SpecDef>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "SPECS",
            offset: entry.offset,
            expected: 8,
        });
    }

    let num_specs = read_u64(section, 0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "spec count bounded by file size"
    )]
    let count = num_specs as usize;

    let mut cursor = &section[8..];
    let (path_indices, c1) = read_compressed_ints(cursor, count, 4)?;
    cursor = &cursor[c1..];
    let (fieldset_indices, c2) = read_compressed_ints(cursor, count, 4)?;
    cursor = &cursor[c2..];
    let (forms, _c3) = read_compressed_ints(cursor, count, 4)?;

    let mut specs = Vec::with_capacity(count);
    for i in 0..count {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "spec indices are u32-range"
        )]
        let path_index = path_indices[i] as u32;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "spec indices are u32-range"
        )]
        let fieldset_index = fieldset_indices[i] as u32;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "spec forms are small enum values"
        )]
        let form = SpecForm::try_from(forms[i] as u32)?;
        specs.push(SpecDef {
            path_index,
            fieldset_index,
            form,
        });
    }

    Ok(specs)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns the slice of `data` corresponding to the given section entry.
fn section_slice<'a>(data: &'a [u8], entry: &SectionEntry) -> Result<&'a [u8], UsdcError> {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "section offsets validated by TOC parser"
    )]
    let start = entry.offset as usize;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "section sizes validated by TOC parser"
    )]
    let size = entry.size as usize;
    let end = start + size;
    if end > data.len() {
        return Err(UsdcError::SectionOutOfBounds {
            name: String::from("section"),
            offset: entry.offset,
            size: entry.size,
        });
    }
    Ok(&data[start..end])
}

/// Reads a `u64` little-endian from `data` at `offset`.
fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn build_paths_single_root() {
        // One path: the pseudo-root "/"
        let tokens: Vec<String> = vec!["Root".to_string()];
        let path_indices = vec![0_i64];
        let element_token_indices = vec![0_i64];
        let jumps = vec![0_i64]; // leaf

        let mut paths = vec![String::new(); 1];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
        )
        .unwrap();

        assert_eq!(paths[0], "/");
    }

    #[test]
    fn build_paths_root_with_child() {
        // Two paths: "/" at index 0, "/Cube" at index 1
        // Encoded entries:
        //   [0]: path_index=0, token=0("Cube"), jump=-1 (has child, no sibling)
        //   [1]: path_index=1, token=0("Cube"), jump=0 (leaf)
        let tokens = vec!["Cube".to_string()];
        let path_indices = vec![0_i64, 1];
        let element_token_indices = vec![0_i64, 0];
        let jumps = vec![-1_i64, 0];

        let mut paths = vec![String::new(); 2];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
        )
        .unwrap();

        assert_eq!(paths[0], "/");
        assert_eq!(paths[1], "/Cube");
    }

    #[test]
    fn build_paths_property() {
        // Three paths: "/" at 0, "/Sphere" at 1, "/Sphere.radius" at 2
        let tokens = vec!["Sphere".to_string(), "radius".to_string()];
        let path_indices = vec![0_i64, 1, 2];
        // Entry 0: root
        // Entry 1: prim Sphere (positive token index)
        // Entry 2: property radius (negative token index = -1)
        let element_token_indices = vec![0_i64, 0, -1];
        // Entry 0: has child, no sibling → jump=-1
        // Entry 1: has child, no sibling → jump=-1
        // Entry 2: leaf → jump=0
        let jumps = vec![-1_i64, -1, 0];

        let mut paths = vec![String::new(); 3];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
        )
        .unwrap();

        assert_eq!(paths[0], "/");
        assert_eq!(paths[1], "/Sphere");
        assert_eq!(paths[2], "/Sphere.radius");
    }

    #[test]
    fn build_paths_multiple_property_siblings() {
        // Typical gen_bool.usdc layout:
        //   /  (path 0)
        //   /root  (path 1)
        //   /root.array  (path 2)
        //   /root.array:unset  (path 3)
        //   /root.single  (path 4)
        //   /root.unset  (path 5)
        //
        // Encoded entries:
        //   [0]: idx=0, token=0(""),      jump=-1  → root "/" (has child, no sibling)
        //   [1]: idx=1, token=1("root"),   jump=-1  → /root (has child, no sibling)
        //   [2]: idx=2, token=-2("array"), jump=0   → /root.array (no child, has sibling → advance)
        //   [3]: idx=3, token=-3("array:unset"), jump=0 → /root.array:unset (sibling → advance)
        //   [4]: idx=4, token=-4("single"),jump=0   → /root.single (sibling → advance)
        //   [5]: idx=5, token=-5("unset"), jump=-2  → /root.unset (no child, no sibling → stop)
        let tokens = vec![
            "".to_string(),
            "root".to_string(),
            "array".to_string(),
            "array:unset".to_string(),
            "single".to_string(),
            "unset".to_string(),
        ];
        let path_indices = vec![0, 1, 2, 3, 4, 5_i64];
        let element_token_indices = vec![0, 1, -2, -3, -4, -5_i64];
        let jumps = vec![-1, -1, 0, 0, 0, -2_i64];

        let mut paths = vec![String::new(); 6];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
        )
        .unwrap();

        assert_eq!(paths[0], "/");
        assert_eq!(paths[1], "/root");
        assert_eq!(paths[2], "/root.array");
        assert_eq!(paths[3], "/root.array:unset");
        assert_eq!(paths[4], "/root.single");
        assert_eq!(paths[5], "/root.unset");
    }
}
