// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for payload load rules.
//!
//! `fixtures/load_rules/oracle.json` records what OpenUSD 26.08's
//! `UsdStageLoadRules` holds after sequences of `LoadWithDescendants`,
//! `LoadWithoutDescendants`, `Unload` and `AddRule` edits, starting from all
//! and from no payloads loaded (`scripts/load_rules_oracle.py`): the literal
//! rules and `GetEffectiveRuleForPath` of every path after each edit, and
//! the same after `Minimize`. `PayloadLoadRules` must hold the same rules
//! and answer the same effective rules, minimized or not.
//!
//! Spec: AOUSD Core §10.3.2.7 (payloads), §11 (population); OpenUSD
//! `pxr/usd/usd/stageLoadRules.cpp`.

use std::collections::BTreeMap;

use layerstack::{InMemoryStore, Path, PayloadLoadRules, PayloadRule};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/load_rules/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    paths: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    start: String,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    op: Vec<String>,
    after: State,
    minimized: State,
}

#[derive(Deserialize, Debug, PartialEq)]
struct State {
    rules: Vec<(String, String)>,
    effective: BTreeMap<String, String>,
}

fn rule_name(rule: PayloadRule) -> &'static str {
    match rule {
        PayloadRule::All => "all",
        PayloadRule::Only => "only",
        PayloadRule::None => "none",
    }
}

fn state(rules: &PayloadLoadRules, paths: &[String], store: &InMemoryStore) -> State {
    let mut literal: Vec<(String, String)> = rules
        .rules()
        .map(|(path, rule)| (path.display(&store.tokens), rule_name(rule).to_owned()))
        .collect();
    literal.sort();
    let effective = paths
        .iter()
        .map(|path| {
            let parsed = Path::parse_absolute(path, &store.tokens).unwrap();
            (
                path.clone(),
                rule_name(rules.effective_rule(&parsed)).to_owned(),
            )
        })
        .collect();
    State {
        rules: literal,
        effective,
    }
}

#[test]
fn load_rule_edits_and_minimizing_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert_eq!(oracle.openusd_version, "26.8");
    let store = InMemoryStore::default();
    for (index, case) in oracle.cases.iter().enumerate() {
        let mut rules = match case.start.as_str() {
            "all" => PayloadLoadRules::default(),
            _ => PayloadLoadRules::load_none(),
        };
        for (at, step) in case.steps.iter().enumerate() {
            let path = Path::parse_absolute(&step.op[1], &store.tokens).unwrap();
            match step.op[0].as_str() {
                "load_with_descendants" => rules.load_with_descendants(path),
                "load_without_descendants" => rules.load_without_descendants(path),
                "unload" => rules.unload(path),
                _ => rules.add_rule(
                    path,
                    match step.op[2].as_str() {
                        "all" => PayloadRule::All,
                        "only" => PayloadRule::Only,
                        _ => PayloadRule::None,
                    },
                ),
            }
            let context = format!("case {index} step {at} {:?}", step.op);
            assert_eq!(
                state(&rules, &oracle.paths, &store),
                step.after,
                "{context}"
            );
            let mut minimized = rules.clone();
            minimized.minimize();
            assert_eq!(
                state(&minimized, &oracle.paths, &store),
                step.minimized,
                "{context}, minimized"
            );
        }
    }
}
