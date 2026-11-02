// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The per-level bounding hierarchy over groups: a port of
//! `clodBuildHierarchy` from meshoptimizer's `demo/clusterlod.h`, version
//! 1.3 (<https://github.com/zeux/meshoptimizer/blob/v1.3/demo/clusterlod.h>),
//! Copyright (c) 2016-2026 Arseny Kapoulkine, MIT License; the notice is
//! reproduced in full in `build.rs`.

use meshopt::VertexDataAdapter;

use crate::build::{merge_spheres, to_u32};
use crate::{GroupTable, NO_GROUP, NodeTable};

/// One node while the hierarchy is built.
#[derive(Clone, Copy, Debug)]
struct Node {
    sphere: [f32; 4],
    error: f32,
    group: u32,
    child_start: u32,
    child_count: u32,
}

/// A forest with one tree per level: nodes `0..levels` are the roots,
/// then each tree's nodes, built bottom-up by spatially clustering the
/// level's groups `width` at a time.
pub(crate) fn build(groups: &GroupTable, levels: u32, width: usize) -> NodeTable {
    let levels = levels as usize;
    let mut nodes: Vec<Node> = vec![
        Node {
            sphere: [0.0; 4],
            error: 0.0,
            group: NO_GROUP,
            child_start: 0,
            child_count: 0,
        };
        levels
    ];
    let mut row: Vec<Node> = Vec::new();
    let mut centers: Vec<[f32; 3]> = Vec::new();
    let mut spheres: Vec<[f32; 4]> = Vec::new();
    for (level, root) in (0..levels).zip(0..) {
        row.clear();
        for (group, &depth) in groups.depth.iter().enumerate() {
            if depth as usize == level {
                row.push(Node {
                    sphere: groups.bounds[group],
                    error: groups.error[group],
                    group: to_u32(group),
                    child_start: 0,
                    child_count: 0,
                });
            }
        }
        while row.len() > 1 {
            centers.clear();
            centers.extend(row.iter().map(|n| [n.sphere[0], n.sphere[1], n.sphere[2]]));
            let vertices = VertexDataAdapter::new(meshopt::typed_to_bytes(&centers), 12, 0)
                .expect("centers are whole float triples");
            let order = meshopt::spatial_cluster_points(&vertices, width);
            let start = nodes.len();
            nodes.extend(order.iter().map(|&i| row[i as usize]));
            row.clear();
            // Every chunk but the last is full.
            for chunk_start in (start..nodes.len()).step_by(width) {
                let children = &nodes[chunk_start..(chunk_start + width).min(nodes.len())];
                spheres.clear();
                spheres.extend(children.iter().map(|n| n.sphere));
                row.push(Node {
                    sphere: merge_spheres(&spheres),
                    error: children.iter().fold(0.0, |e, n| n.error.max(e)),
                    group: NO_GROUP,
                    child_start: to_u32(chunk_start),
                    child_count: to_u32(children.len()),
                });
            }
        }
        if let Some(&top) = row.first() {
            nodes[root] = top;
        }
    }
    let mut table = NodeTable::default();
    for node in nodes {
        table.bounds.push(node.sphere);
        table.error.push(node.error);
        table.group.push(node.group);
        table.child_start.push(node.child_start);
        table.child_count.push(node.child_count);
    }
    table
}
