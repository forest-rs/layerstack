// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::vec::Vec;

use layerstack::path_expression::{PathExpression, PathPattern, SetOp};
use layerstack::sort;

use super::{ExpansionRule, MembershipQuery, MembershipRule, Scene};

impl MembershipQuery {
    /// Exports the effective membership as a complete path expression. With a
    /// rule map this preserves includes, excludes, and expansion rules, even
    /// for descendants added later. Otherwise it uses the resolved expression,
    /// or the empty expression if the query has no working evaluator.
    ///
    /// Pass the scene used to compute the query so its path IDs resolve to
    /// the correct names. The returned expression owns those names.
    /// [`Self::expression`] provides borrowed access to the authored expression.
    ///
    /// See AOUSD Core §15 (collections) and OpenUSD's
    /// `UsdComputePathExpressionFromCollectionMembershipQueryRuleMap`.
    #[must_use]
    pub fn to_path_expression(&self, scene: &Scene<'_>) -> PathExpression {
        if !self.uses_rule_map() {
            return self
                .evaluator()
                .map_or_else(PathExpression::nothing, |e| e.expression().clone());
        }

        // Ancestor rules apply first. Same-depth entries are disjoint, so
        // their unions can be balanced even for very wide collections.
        let mut rules: Vec<_> = self
            .rules()
            .map(|(&path, &rule)| {
                let pattern = PathPattern::from_target(path, scene.store());
                let depth = pattern.prefix_prims().len() + usize::from(pattern.is_property());
                (depth, pattern, rule)
            })
            .collect();
        sort::sort_unstable_by(&mut rules, |(da, a, _), (db, b, _)| {
            da.cmp(db)
                .then_with(|| a.prefix_prims().cmp(b.prefix_prims()))
                .then_with(|| a.prefix_property().cmp(&b.prefix_property()))
        });
        let mut expression = PathExpression::nothing();
        let mut rules = rules.into_iter().peekable();
        let mut includes = Vec::new();
        let mut prims_only = Vec::new();
        let mut excludes = Vec::new();
        while let Some(&(depth, _, _)) = rules.peek() {
            while let Some((_, pattern, rule)) = rules.next_if(|(d, _, _)| *d == depth) {
                let exact = pattern.is_property()
                    || rule == MembershipRule::Include(ExpansionRule::ExplicitOnly);
                if depth == 0 && exact {
                    continue; // The pseudo-root itself is never a rule-map member.
                }
                let atom = if exact {
                    PathExpression::pattern(pattern)
                } else if depth == 0 {
                    PathExpression::parse("/*//").expect("literal descendant pattern")
                } else {
                    PathExpression::pattern(pattern.with_descendants())
                };
                match rule {
                    MembershipRule::Exclude => excludes.push(atom),
                    MembershipRule::Include(ExpansionRule::ExpandPrims) if !exact => {
                        prims_only.push(atom);
                    }
                    MembershipRule::Include(_) => includes.push(atom),
                }
            }

            if !prims_only.is_empty() {
                includes.push(PathExpression::op(
                    SetOp::Difference,
                    union(&mut prims_only),
                    PathExpression::parse("//*.*").expect("literal property pattern"),
                ));
                prims_only.clear();
            }
            expression = PathExpression::op(SetOp::Union, expression, union(&mut includes));
            expression = PathExpression::op(SetOp::Difference, expression, union(&mut excludes));
            includes.clear();
            excludes.clear();
        }
        expression
    }
}

/// Combine siblings without building an expression tree as deep as the list.
fn union(items: &mut [PathExpression]) -> PathExpression {
    match items {
        [] => PathExpression::nothing(),
        [one] => core::mem::take(one),
        many => {
            let (left, right) = many.split_at_mut(many.len() / 2);
            PathExpression::op(SetOp::Union, union(left), union(right))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layerstack::path_expression::NoPredicates;
    use layerstack::{InMemoryStore, Layer, LayerId, Stage, StageOptions, TargetPath};

    #[test]
    fn exported_rules_apply_to_future_children_and_properties() {
        let mut store = InMemoryStore::default();
        let root_layer = LayerId(1);
        store.insert_layer(Layer::new(root_layer));
        let stage = Stage::compose(&mut store, root_layer, StageOptions::default());
        let scene = Scene::new(&stage, &store);
        let target = |text: &str| TargetPath::parse(text, store.tokens(), store.paths()).unwrap();
        let mut query = MembershipQuery::compute(&scene, target("/").prim_path(), "set");

        use ExpansionRule::{ExpandPrims, ExpandPrimsAndProperties, ExplicitOnly};
        use MembershipRule::{Exclude, Include};
        for (path, rule) in [
            ("/A", Include(ExpandPrimsAndProperties)),
            ("/A/B", Exclude),
            ("/A/B/C", Include(ExpandPrims)),
            ("/A/B/C.attr:keep", Include(ExplicitOnly)),
            ("/A/B/D", Include(ExplicitOnly)),
            ("/A/E", Include(ExpandPrims)),
        ] {
            query.rules.insert(target(path), rule);
        }
        let expression = query.to_path_expression(&scene);
        let matcher = expression.matcher(&NoPredicates).unwrap();
        for (path, included) in [
            ("/A/B", false),
            ("/A/B/C/Child", true),
            ("/A/B/C/Child.attr", false),
            ("/A/B/C.attr:keep", true),
            ("/A/B/D", true),
            ("/A/B/D/Child", false),
            ("/A/E/Child.attr", true),
        ] {
            assert_eq!(
                matcher.matches(&NoPredicates, &store, target(path)).value,
                included,
                "{expression} at {path}"
            );
        }
    }
}
