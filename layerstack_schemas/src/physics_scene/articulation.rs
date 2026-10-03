// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
/// Rooted rigid-body graph under an articulation API.
#[derive(Clone, Debug, PartialEq)]
pub struct ArticulationDescriptor {
    /// Floating roots are body paths; fixed roots are joints connected to world.
    pub roots: Vec<PathId>,
    /// Connected bodies, including `None` for world endpoints, in path order.
    pub bodies: Vec<Option<PathId>>,
    /// Connected joint paths, including excluded loop/ordinary constraints.
    pub joints: Vec<PathId>,
    /// Pair exclusions authored on the articulation root.
    pub filtered_collisions: Vec<TargetPath>,
}
struct Link {
    path: PathId,
    children: Vec<Option<PathId>>,
    joints: Vec<PathId>,
    weight: usize,
    root_joint: Option<PathId>,
}
fn under(scene: &Scene<'_>, path: PathId, ancestor: PathId) -> bool {
    scene
        .store()
        .paths()
        .resolve(ancestor)
        .is_prefix_of(scene.store().paths().resolve(path))
}
fn path_order(scene: &Scene<'_>, a: PathId, b: PathId) -> core::cmp::Ordering {
    let store = scene.store();
    store
        .paths()
        .display(a, store.tokens())
        .cmp(&store.paths().display(b, store.tokens()))
}
pub(super) fn capture(
    scene: &Scene<'_>,
    path: PathId,
    bodies: &[PhysicsRecord<RigidBodyDescriptor>],
    joints: &[PhysicsRecord<JointDescriptor>],
) -> Result<ArticulationDescriptor, PhysicsSceneError> {
    let mut ancestor = scene.parent(path);
    while let Some(p) = ancestor {
        if scene.has_api(p, "PhysicsArticulationRootAPI", None) {
            return Err(PhysicsSceneError::NestedArticulation);
        }
        ancestor = scene.parent(p);
    }
    let active_bodies: HashSet<_> = bodies
        .iter()
        .filter(|b| b.descriptor.as_ref().is_ok_and(|b| b.enabled))
        .map(|b| b.path)
        .collect();
    let mut ordered_joints: Vec<_> = joints
        .iter()
        .filter_map(|j| {
            j.descriptor
                .as_ref()
                .ok()
                .filter(|j| j.enabled)
                .map(|d| (j.path, d))
        })
        .collect();
    ordered_joints.sort_by(|a, b| path_order(scene, a.0, b.0));
    let mut roots = Vec::new();
    let mut base = path;
    if bodies.iter().any(|b| b.path == path) {
        roots.push(path);
    } else if let Some(joint) = joints
        .iter()
        .find(|j| j.path == path)
        .and_then(|j| j.descriptor.as_ref().ok())
        && (joint.bodies[0].is_none() || joint.bodies[1].is_none())
    {
        roots.push(path);
        if let Some(body) = joint.bodies[0].or(joint.bodies[1]) {
            base = body;
        }
    }
    let mut components: Vec<Vec<Link>> = Vec::new();
    let mut visited = HashSet::new();
    // Namespace order supplies tie-breaking, as OpenUSD's articulationLinkOrder.
    for candidate in bodies.iter().filter(|b| under(scene, b.path, base)) {
        if visited.contains(&candidate.path) {
            continue;
        }
        let mut component = Vec::new();
        let mut pending = alloc::vec![candidate.path];
        while let Some(body) = pending.pop() {
            if !visited.insert(body) || !active_bodies.contains(&body) {
                continue;
            }
            let body_joints: Vec<_> = ordered_joints
                .iter()
                .copied()
                .filter(|(_, j)| j.bodies.contains(&Some(body)))
                .collect();
            if body_joints.is_empty() {
                continue;
            }
            let mut link = Link {
                path: body,
                children: Vec::new(),
                joints: Vec::new(),
                weight: 0,
                root_joint: None,
            };
            let mut children_to_visit = Vec::new();
            for (joint_path, joint) in body_joints {
                link.joints.push(joint_path);
                let other = if joint.bodies[0] == Some(body) {
                    joint.bodies[1]
                } else {
                    joint.bodies[0]
                };
                if other.is_none_or(|b| !active_bodies.contains(&b)) {
                    link.children.push(None);
                    if joint.exclude_from_articulation {
                        link.weight += 1000;
                    } else {
                        link.weight += 100_000;
                        link.root_joint = Some(joint_path);
                    }
                } else {
                    link.children.push(other);
                    if joint.exclude_from_articulation {
                        link.weight += 1000;
                    } else {
                        link.weight += 100;
                        children_to_visit.push(other.expect("non-world endpoint"));
                    }
                }
            }
            pending.extend(children_to_visit.into_iter().rev());
            component.push(link);
        }
        if !component.is_empty() {
            components.push(component);
        }
    }
    if roots.is_empty() {
        for links in &components {
            let root = if links.iter().any(|l| l.root_joint.is_some()) {
                // C++ iterates a map ordered by body path. Its tie-break
                // searches DFS body order for the candidate root path: joint
                // roots are absent, so equal joint roots retain lexical order.
                let mut candidates: Vec<_> = links.iter().collect();
                candidates.sort_by(|a, b| path_order(scene, a.path, b.path));
                let rank = |link: &Link| {
                    let root = link.root_joint.unwrap_or(link.path);
                    links
                        .iter()
                        .position(|l| l.path == root)
                        .unwrap_or(usize::MAX)
                };
                let mut best = candidates[0];
                for candidate in candidates.into_iter().skip(1) {
                    if candidate.weight > best.weight
                        || (candidate.weight == best.weight && rank(candidate) < rank(best))
                    {
                        best = candidate;
                    }
                }
                best.root_joint.unwrap_or(best.path)
            } else if links.iter().all(|l| under(scene, l.path, links[0].path)) {
                links[0].path
            } else {
                center(links)
            };
            roots.push(root);
        }
    }
    if roots.is_empty() {
        return Err(PhysicsSceneError::EmptyArticulation);
    }
    let mut articulated_bodies: Vec<Option<PathId>> = components
        .iter()
        .flat_map(|c| c.iter().flat_map(|l| l.children.iter().copied()))
        .collect();
    articulated_bodies.sort_by(|a, b| match (a, b) {
        (None, None) => core::cmp::Ordering::Equal,
        (None, Some(_)) => core::cmp::Ordering::Less,
        (Some(_), None) => core::cmp::Ordering::Greater,
        (Some(a), Some(b)) => path_order(scene, *a, *b),
    });
    articulated_bodies.dedup();
    let mut articulated_joints: Vec<_> = components
        .iter()
        .flat_map(|c| c.iter().flat_map(|l| l.joints.iter().copied()))
        .collect();
    articulated_joints.sort_by(|a, b| path_order(scene, *a, *b));
    articulated_joints.dedup();
    Ok(ArticulationDescriptor {
        roots,
        bodies: articulated_bodies,
        joints: articulated_joints,
        filtered_collisions: filtered(&PrimView::new(*scene, path)),
    })
}
fn center(links: &[Link]) -> PathId {
    // Follow OpenUSD's depth-first graph distance and first-traversed tie-break.
    // No quadratic distance matrix is retained: each start is independent.
    let mut best = (usize::MAX, 0, 0);
    for (start, link) in links.iter().enumerate() {
        let mut distances = alloc::vec![None;links.len()];
        let mut pending = alloc::vec![(start, 0)];
        while let Some((i, d)) = pending.pop() {
            if distances[i].is_some() {
                continue;
            }
            distances[i] = Some(d);
            for child in links[i].children.iter().rev().flatten() {
                if let Some(j) = links.iter().position(|l| l.path == *child)
                    && distances[j].is_none()
                {
                    pending.push((j, d + 1));
                }
            }
        }
        let longest = distances.iter().flatten().copied().max().unwrap_or(0);
        if longest < best.0 || (longest == best.0 && link.children.len() > best.1) {
            best = (longest, link.children.len(), start);
        }
    }
    links[best.2].path
}
