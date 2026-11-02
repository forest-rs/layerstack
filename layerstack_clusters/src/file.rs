// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A binary encoding of a [`ClusterSet`] and the vertices it indexes, for
//! caching sets built ahead of time.
//!
//! Little-endian throughout, every value 4 bytes wide, so each column of a
//! file read into memory is aligned for a typed array view at its offset:
//!
//! | Section | Contents |
//! | --- | --- |
//! | header | magic `LSCLUSTR`, then 16 `u32`: [`FILE_VERSION`], flags (bit 0 normals, bit 1 texture coordinates), vertices, indices, clusters, groups, nodes, levels, report levels, then 7 reserved zeros |
//! | vertices | positions `f32` ×3 per vertex; normals `f32` ×3 (flag 0); texture coordinates `f32` ×2 (flag 1) |
//! | indices | [`ClusterSet::indices`], `u32` |
//! | clusters | [`ClusterTable`] columns in field order: `index_start`, `triangle_count`, `vertex_count`, `group`, `refined`, `subset` (`u32` each), then `bounds` (`f32` ×4) |
//! | groups | [`GroupTable`]: `depth` (`u32`), `bounds` (`f32` ×4), `error` (`f32`, infinite for a root or stuck group), `cluster_start`, `cluster_count` (`u32`) |
//! | nodes | [`NodeTable`]: `bounds` (`f32` ×4), `error` (`f32`), `group`, `child_start`, `child_count` (`u32`) |
//! | report | [`ClusterReport`]: `vertices`, `triangles` (low then high `u32`), `subsets`, `subset_border_vertices`, `seam_vertices`; then per level `groups`, `clusters`, `triangles` (low, high), `stuck_groups`, `sloppy_groups` |
//!
//! Columns are whole arrays one after another (structure of arrays), so a
//! reader uploads a column as it is.

use core::fmt;

use crate::{ClusterReport, ClusterSet, ClusterTable, GroupTable, LevelReport, NodeTable};

/// The first eight bytes of a cluster file.
pub const FILE_MAGIC: [u8; 8] = *b"LSCLUSTR";

/// The version of the encoding [`ClusterFile::encode`] writes and
/// [`ClusterFile::decode`] reads. A reader refuses any other.
pub const FILE_VERSION: u32 = 1;

/// The header's size in bytes: the magic and 16 `u32`.
pub const HEADER_BYTES: usize = 8 + 16 * 4;

const NORMALS: u32 = 1;
const UVS: u32 = 2;

/// A [`ClusterSet`] with the vertices its indices refer to: what a cluster
/// file holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClusterFile {
    /// The set.
    pub set: ClusterSet,
    /// Each vertex's position: [`ClusterSet::vertex_count`] of them.
    pub positions: Vec<[f32; 3]>,
    /// Each vertex's normal, when kept.
    pub normals: Option<Vec<[f32; 3]>>,
    /// Each vertex's texture coordinate, when kept.
    pub uvs: Option<Vec<[f32; 2]>>,
}

/// Why [`ClusterFile::decode`] refused its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes do not start with [`FILE_MAGIC`].
    Magic,
    /// The file is of another [`FILE_VERSION`].
    Version {
        /// The version it has.
        found: u32,
    },
    /// The bytes end before the sections the header announces.
    Truncated {
        /// The bytes the header announces.
        expected: usize,
        /// The bytes there are.
        len: usize,
    },
    /// A count or index in the file is inconsistent with the rest.
    Inconsistent(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Magic => f.write_str("not a cluster file"),
            Self::Version { found } => {
                write!(f, "cluster file version {found}, expected {FILE_VERSION}")
            }
            Self::Truncated { expected, len } => {
                write!(f, "cluster file of {len} bytes, expected {expected}")
            }
            Self::Inconsistent(what) => write!(f, "inconsistent cluster file: {what}"),
        }
    }
}

impl core::error::Error for DecodeError {}

impl ClusterFile {
    /// The file's bytes, laid out as the [module](self) describes.
    ///
    /// # Panics
    ///
    /// When `positions`, `normals` or `uvs` is not one per vertex of the
    /// set.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let set = &self.set;
        let vertices = set.vertex_count as usize;
        assert_eq!(self.positions.len(), vertices, "a position per vertex");
        if let Some(normals) = &self.normals {
            assert_eq!(normals.len(), vertices, "a normal per vertex");
        }
        if let Some(uvs) = &self.uvs {
            assert_eq!(uvs.len(), vertices, "a texture coordinate per vertex");
        }
        let flags = if self.normals.is_some() { NORMALS } else { 0 }
            | if self.uvs.is_some() { UVS } else { 0 };
        let mut out = Writer(Vec::with_capacity(self.encoded_len()));
        out.0.extend_from_slice(&FILE_MAGIC);
        let report = &set.report;
        for value in [
            FILE_VERSION,
            flags,
            set.vertex_count,
            len32(set.indices.len()),
            len32(set.clusters.len()),
            len32(set.groups.len()),
            len32(set.nodes.len()),
            set.levels,
            len32(report.levels.len()),
        ] {
            out.u32(value);
        }
        for _ in 0..7 {
            out.u32(0);
        }
        out.vecs(&self.positions);
        if let Some(normals) = &self.normals {
            out.vecs(normals);
        }
        if let Some(uvs) = &self.uvs {
            out.vecs(uvs);
        }
        out.u32s(&set.indices);
        let c = &set.clusters;
        for column in [
            &c.index_start,
            &c.triangle_count,
            &c.vertex_count,
            &c.group,
            &c.refined,
            &c.subset,
        ] {
            out.u32s(column);
        }
        out.vecs(&c.bounds);
        let g = &set.groups;
        out.u32s(&g.depth);
        out.vecs(&g.bounds);
        out.f32s(&g.error);
        out.u32s(&g.cluster_start);
        out.u32s(&g.cluster_count);
        let n = &set.nodes;
        out.vecs(&n.bounds);
        out.f32s(&n.error);
        out.u32s(&n.group);
        out.u32s(&n.child_start);
        out.u32s(&n.child_count);
        out.u32(report.vertices);
        out.u64(report.triangles);
        out.u32(report.subsets);
        out.u32(report.subset_border_vertices);
        out.u32(report.seam_vertices);
        for level in &report.levels {
            out.u32(level.groups);
            out.u32(level.clusters);
            out.u64(level.triangles);
            out.u32(level.stuck_groups);
            out.u32(level.sloppy_groups);
        }
        debug_assert_eq!(
            out.0.len(),
            self.encoded_len(),
            "the length encoded_len promises"
        );
        out.0
    }

    /// The bytes [`Self::encode`] writes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        let set = &self.set;
        let vertices = set.vertex_count as usize;
        let floats_per_vertex =
            3 + if self.normals.is_some() { 3 } else { 0 } + if self.uvs.is_some() { 2 } else { 0 };
        let words = vertices * floats_per_vertex
            + set.indices.len()
            + set.clusters.len() * (6 + 4)
            + set.groups.len() * (1 + 4 + 1 + 2)
            + set.nodes.len() * (4 + 1 + 3)
            + 6
            + set.report.levels.len() * 6;
        HEADER_BYTES + 4 * words
    }

    /// Reads a file [`Self::encode`] wrote.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when the bytes are not a cluster file of
    /// [`FILE_VERSION`], end early, or hold counts or indices that do not
    /// agree.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_BYTES || bytes[..8] != FILE_MAGIC {
            return Err(DecodeError::Magic);
        }
        let mut header = Reader { bytes, at: 8 };
        let version = header.u32();
        if version != FILE_VERSION {
            return Err(DecodeError::Version { found: version });
        }
        let flags = header.u32();
        let [
            vertices,
            indices,
            clusters,
            groups,
            nodes,
            levels,
            report_levels,
        ] = [(); 7].map(|()| header.u32() as usize);
        let mut file = Self {
            set: ClusterSet {
                vertex_count: len32(vertices),
                levels: len32(levels),
                ..ClusterSet::default()
            },
            ..Self::default()
        };
        file.normals = (flags & NORMALS != 0).then(Vec::new);
        file.uvs = (flags & UVS != 0).then(Vec::new);
        // Every count is checked against the length before anything is
        // allocated for it.
        let floats_per_vertex =
            3 + if file.normals.is_some() { 3 } else { 0 } + if file.uvs.is_some() { 2 } else { 0 };
        let words = [
            vertices.checked_mul(floats_per_vertex),
            Some(indices),
            clusters.checked_mul(10),
            groups.checked_mul(8),
            nodes.checked_mul(8),
            Some(6),
            report_levels.checked_mul(6),
        ]
        .into_iter()
        .try_fold(0_usize, |sum, words| sum.checked_add(words?));
        let expected = words
            .and_then(|w| w.checked_mul(4))
            .and_then(|b| b.checked_add(HEADER_BYTES))
            .ok_or(DecodeError::Inconsistent("counts overflow"))?;
        if bytes.len() != expected {
            return Err(DecodeError::Truncated {
                expected,
                len: bytes.len(),
            });
        }
        let mut r = Reader {
            bytes,
            at: HEADER_BYTES,
        };
        file.positions = r.vecs(vertices);
        if let Some(normals) = &mut file.normals {
            *normals = r.vecs(vertices);
        }
        if let Some(uvs) = &mut file.uvs {
            *uvs = r.vecs(vertices);
        }
        let set = &mut file.set;
        set.indices = r.u32s(indices);
        set.clusters = ClusterTable {
            index_start: r.u32s(clusters),
            triangle_count: r.u32s(clusters),
            vertex_count: r.u32s(clusters),
            group: r.u32s(clusters),
            refined: r.u32s(clusters),
            subset: r.u32s(clusters),
            bounds: r.vecs(clusters),
        };
        set.groups = GroupTable {
            depth: r.u32s(groups),
            bounds: r.vecs(groups),
            error: r.f32s(groups),
            cluster_start: r.u32s(groups),
            cluster_count: r.u32s(groups),
        };
        set.nodes = NodeTable {
            bounds: r.vecs(nodes),
            error: r.f32s(nodes),
            group: r.u32s(nodes),
            child_start: r.u32s(nodes),
            child_count: r.u32s(nodes),
        };
        set.report = ClusterReport {
            vertices: r.u32(),
            triangles: r.u64(),
            subsets: r.u32(),
            subset_border_vertices: r.u32(),
            seam_vertices: r.u32(),
            levels: (0..report_levels)
                .map(|_| LevelReport {
                    groups: r.u32(),
                    clusters: r.u32(),
                    triangles: r.u64(),
                    stuck_groups: r.u32(),
                    sloppy_groups: r.u32(),
                })
                .collect(),
        };
        check(set)?;
        Ok(file)
    }
}

/// Checks that every index in `set` is in range, so a renderer can trust
/// a decoded set as it trusts a built one.
fn check(set: &ClusterSet) -> Result<(), DecodeError> {
    let vertices = set.vertex_count;
    if !set.indices.len().is_multiple_of(3) || set.indices.iter().any(|&i| i >= vertices) {
        return Err(DecodeError::Inconsistent("an index out of range"));
    }
    let groups = set.groups.len();
    let clusters = set.clusters.len();
    let c = &set.clusters;
    for cluster in 0..clusters {
        let start = c.index_start[cluster] as usize;
        let end = start + c.triangle_count[cluster] as usize * 3;
        if end > set.indices.len()
            || c.group[cluster] as usize >= groups
            || (c.refined[cluster] != crate::NO_GROUP && c.refined[cluster] as usize >= groups)
        {
            return Err(DecodeError::Inconsistent("a cluster out of range"));
        }
    }
    let g = &set.groups;
    for group in 0..groups {
        if g.cluster_start[group] as usize + g.cluster_count[group] as usize > clusters {
            return Err(DecodeError::Inconsistent("a group out of range"));
        }
    }
    let n = &set.nodes;
    if (set.levels as usize) > n.len() {
        return Err(DecodeError::Inconsistent("more levels than nodes"));
    }
    for node in 0..n.len() {
        let leaf_bad = n.group[node] != crate::NO_GROUP && n.group[node] as usize >= groups;
        let children_bad = n.group[node] == crate::NO_GROUP
            && n.child_start[node] as usize + n.child_count[node] as usize > n.len();
        if leaf_bad || children_bad {
            return Err(DecodeError::Inconsistent("a node out of range"));
        }
    }
    Ok(())
}

fn len32(n: usize) -> u32 {
    u32::try_from(n).expect("a cluster set's counts fit 32 bits")
}

struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        // Low word first, then high: two aligned `u32`.
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u32s(&mut self, values: &[u32]) {
        for &value in values {
            self.u32(value);
        }
    }

    fn f32s(&mut self, values: &[f32]) {
        for &value in values {
            self.0.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn vecs<const N: usize>(&mut self, values: &[[f32; N]]) {
        for value in values {
            self.f32s(value);
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn word(&mut self) -> [u8; 4] {
        let word = [
            self.bytes[self.at],
            self.bytes[self.at + 1],
            self.bytes[self.at + 2],
            self.bytes[self.at + 3],
        ];
        self.at += 4;
        word
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.word())
    }

    fn u64(&mut self) -> u64 {
        let low = u64::from(self.u32());
        low | (u64::from(self.u32()) << 32)
    }

    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.word())
    }

    fn u32s(&mut self, len: usize) -> Vec<u32> {
        (0..len).map(|_| self.u32()).collect()
    }

    fn f32s(&mut self, len: usize) -> Vec<f32> {
        (0..len).map(|_| self.f32()).collect()
    }

    fn vecs<const N: usize>(&mut self, len: usize) -> Vec<[f32; N]> {
        (0..len).map(|_| [(); N].map(|()| self.f32())).collect()
    }
}
