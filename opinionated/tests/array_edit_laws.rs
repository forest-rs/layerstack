// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Algebraic laws of sparse array edits, checked over generated programs.
//!
//! A small deterministic generator builds edit programs whose indices land in,
//! at and beyond the bounds of short arrays, so every instruction's skip rules
//! are exercised. Each law runs over the same fixed seeds on every run.

use opinionated::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayFill, ArrayIndex, FillWith};

/// A xorshift64* generator: deterministic, dependency-free and good enough to
/// spread programs across the instruction set.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn len(&mut self) -> usize {
        usize::try_from(self.below(8)).unwrap()
    }

    fn index(&mut self) -> ArrayIndex {
        if self.below(6) == 0 {
            ArrayIndex::End
        } else {
            ArrayIndex::Position(i64::try_from(self.below(15)).unwrap() - 7)
        }
    }

    /// Literals are non-negative; fills from a policy are negative, so a
    /// result reveals where each element came from.
    fn literal(&mut self) -> i32 {
        i32::try_from(self.below(100)).unwrap()
    }

    fn operand(&mut self) -> ArrayEditOperand<i32> {
        if self.below(2) == 0 {
            ArrayEditOperand::Literal(self.literal())
        } else {
            ArrayEditOperand::CopyFrom(self.index())
        }
    }

    fn op(&mut self) -> ArrayEditOp<i32> {
        match self.below(8) {
            0 => ArrayEditOp::Write {
                src: self.operand(),
                index: self.index(),
            },
            1 => ArrayEditOp::Insert {
                src: self.operand(),
                index: self.index(),
            },
            2 => ArrayEditOp::Erase {
                index: self.index(),
            },
            3 => ArrayEditOp::MinSize { len: self.len() },
            4 => ArrayEditOp::MinSizeFill {
                len: self.len(),
                fill: self.literal(),
            },
            5 => ArrayEditOp::MaxSize { len: self.len() },
            6 => ArrayEditOp::Resize { len: self.len() },
            _ => ArrayEditOp::ResizeFill {
                len: self.len(),
                fill: self.literal(),
            },
        }
    }

    fn edit(&mut self) -> ArrayEdit<i32> {
        let count = self.below(7);
        ArrayEdit {
            ops: (0..count).map(|_| self.op()).collect(),
        }
    }

    fn array(&mut self) -> Vec<i32> {
        let len = self.len();
        (0..len).map(|_| self.literal()).collect()
    }
}

const CASES: u64 = 2_000;

/// Runs `law` over `CASES` generated cases, each from its own seed so a
/// failure names the seed that reproduces it.
fn check(law: impl Fn(&mut Rng)) {
    for seed in 1..=CASES {
        law(&mut Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
    }
}

/// The fill policies every law is checked under.
fn fills() -> [Option<i32>; 2] {
    [None, Some(-1)]
}

#[test]
fn composed_edit_equals_weaker_then_stronger() {
    check(|rng| {
        let weak = rng.edit();
        let strong = rng.edit();
        let base = rng.array();
        for fill in fills() {
            let composed = strong.compose_over(&weak).compose_over_array(&base, fill);
            let sequential = strong.compose_over_array(&weak.compose_over_array(&base, fill), fill);
            assert_eq!(
                composed, sequential,
                "weak {weak:?}, strong {strong:?}, base {base:?}, fill {fill:?}"
            );
        }
    });
}

#[test]
fn composition_is_associative() {
    check(|rng| {
        let weakest = rng.edit();
        let middle = rng.edit();
        let strongest = rng.edit();
        let base = rng.array();
        let left = strongest.compose_over(&middle).compose_over(&weakest);
        let right = strongest.compose_over(&middle.compose_over(&weakest));
        assert_eq!(left, right);
        for fill in fills() {
            assert_eq!(
                left.compose_over_array(&base, fill),
                strongest.compose_over_array(
                    &middle.compose_over_array(&weakest.compose_over_array(&base, fill), fill),
                    fill,
                ),
            );
        }
    });
}

#[test]
fn identity_is_neutral() {
    check(|rng| {
        let edit = rng.edit();
        let base = rng.array();
        let identity = ArrayEdit::default();
        assert_eq!(edit.compose_over(&identity), edit);
        assert_eq!(identity.compose_over(&edit), edit);
        assert_eq!(identity.compose_over_array(&base, Some(-1)), base);
    });
}

#[test]
fn in_place_matches_copying_apply() {
    check(|rng| {
        let edit = rng.edit();
        let base = rng.array();
        for fill in fills() {
            let mut in_place = base.clone();
            edit.apply_in_place(&mut in_place, fill);
            assert_eq!(in_place, edit.compose_over_array(&base, fill));
        }
    });
}

#[test]
fn missing_fill_never_invents_an_element() {
    check(|rng| {
        let edit = rng.edit();
        let base = rng.array();
        let result = edit.compose_over_array(&base, None);
        // Every element comes from the base or from a literal in the program;
        // generated values are non-negative, so none is a policy fill.
        assert!(
            result.iter().all(|value| *value >= 0),
            "{edit:?} over {base:?}"
        );

        // A policy that is asked but answers `None` behaves like no policy.
        let asked = edit.compose_over_array(&base, FillWith(|| None));
        assert_eq!(asked, result);
    });
}

#[test]
fn lazy_fill_matches_eager_fill() {
    /// Counts how often the interpreter asks for a fill element.
    struct Counting<'a>(&'a mut usize);

    impl ArrayFill<i32> for Counting<'_> {
        fn fill_element(&mut self) -> Option<i32> {
            *self.0 += 1;
            Some(-1)
        }
    }

    check(|rng| {
        let edit = rng.edit();
        let base = rng.array();
        let mut asked = 0;
        let lazy = edit.compose_over_array(&base, Counting(&mut asked));
        assert_eq!(lazy, edit.compose_over_array(&base, Some(-1)));
        let growing = edit
            .ops
            .iter()
            .filter(|op| matches!(op, ArrayEditOp::MinSize { .. } | ArrayEditOp::Resize { .. }))
            .count();
        assert!(
            asked <= growing,
            "asked {asked} times for {growing} growth ops"
        );
    });
}
