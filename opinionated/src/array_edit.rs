// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse array edits over any element type.
//!
//! An [`ArrayEdit`] is a program of [`ArrayEditOp`] instructions that rewrites
//! a dense array in order. Each instruction sees the array as edited so far:
//! a [`ArrayEditOperand::CopyFrom`] operand reads the destination after every
//! earlier instruction, not the original input. Instructions whose indices
//! fall outside the current array are skipped, so a program is total over
//! every input.
//!
//! The instruction set and its index rules follow OpenUSD's `VtArrayEdit`
//! (`pxr/base/vt/arrayEdit.h` and `pxr/base/vt/arrayEditOps.h`), which the
//! OpenUSD sparse-array-edits proposal introduces for attribute values. The
//! crate does not know about attributes: the element type is the host's, and
//! so is the element that fills growth when an instruction does not carry
//! one ([`ArrayFill`]).
//!
//! Edits compose by concatenation. [`ArrayEdit::compose_over`] places a weaker
//! program before a stronger one, and applying the result equals applying the
//! weaker program and then the stronger one. A host folding an opinion chain
//! can therefore keep edits sparse until a dense value or block ends the
//! chain, then apply the composed program once.

use alloc::vec::Vec;
use core::fmt;

/// A position in the destination array.
///
/// Indices resolve against the array as edited so far. See
/// [`ArrayEditOp`] for how each instruction treats an index that does not
/// resolve.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArrayIndex {
    /// A concrete index.
    ///
    /// Non-negative values count from the front. Negative values count from
    /// the end: `-1` names the last element, as in OpenUSD's `VtArrayEdit`.
    Position(i64),
    /// The position one past the last element.
    ///
    /// Only an insertion can target `End`; no element lives there to read,
    /// write or erase.
    End,
}

/// Where an inserted or written element comes from.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ArrayEditOperand<T> {
    /// A value carried by the edit.
    Literal(T),
    /// A copy of the element currently stored at an index of the destination
    /// array, as edited by the instructions before this one.
    CopyFrom(ArrayIndex),
}

/// One sparse array edit instruction.
///
/// An instruction whose destination or source index does not resolve to a
/// valid position is skipped without changing the array. Growth that needs a
/// host fill element ([`MinSize`](Self::MinSize), [`Resize`](Self::Resize))
/// is skipped when the host supplies none.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ArrayEditOp<T> {
    /// Overwrites an existing element without changing the array's length.
    ///
    /// `index` must name an existing element; [`ArrayIndex::End`] never does.
    Write {
        /// Source of the new element.
        src: ArrayEditOperand<T>,
        /// Element to overwrite.
        index: ArrayIndex,
    },
    /// Inserts an element, shifting later elements towards the end.
    ///
    /// `index` may be any existing position or [`ArrayIndex::End`], which
    /// appends. A negative index inserts before the element it names, so `-1`
    /// inserts before the last element.
    Insert {
        /// Source of the new element.
        src: ArrayEditOperand<T>,
        /// Insertion point.
        index: ArrayIndex,
    },
    /// Removes an existing element, shifting later elements towards the front.
    Erase {
        /// Element to remove.
        index: ArrayIndex,
    },
    /// Grows the array to at least `len` elements, filling added elements with
    /// the host's fill element ([`ArrayFill`]).
    ///
    /// This is OpenUSD's `minsize N` (`OpMinSize`). Without a fill element the
    /// array is left unchanged.
    MinSize {
        /// Minimum number of elements after the instruction.
        len: usize,
    },
    /// Grows the array to at least `len` elements, filling added elements with
    /// `fill`.
    ///
    /// This is OpenUSD's `minsize N fill <literal>` (`OpMinSizeFill`).
    MinSizeFill {
        /// Minimum number of elements after the instruction.
        len: usize,
        /// Value given to each added element.
        fill: T,
    },
    /// Shrinks the array to at most `len` elements, dropping them from the end.
    ///
    /// This is OpenUSD's `maxsize N` (`OpMaxSize`).
    MaxSize {
        /// Maximum number of elements after the instruction.
        len: usize,
    },
    /// Sets the array's length to exactly `len`, dropping elements from the
    /// end or filling added elements with the host's fill element
    /// ([`ArrayFill`]).
    ///
    /// This is OpenUSD's `resize N` (`OpSetSize`). Shrinking always happens;
    /// without a fill element, growth is skipped.
    Resize {
        /// Number of elements after the instruction.
        len: usize,
    },
    /// Sets the array's length to exactly `len`, dropping elements from the
    /// end or filling added elements with `fill`.
    ///
    /// This is OpenUSD's `resize N fill <literal>` (`OpSetSizeFill`).
    ResizeFill {
        /// Number of elements after the instruction.
        len: usize,
        /// Value given to each added element.
        fill: T,
    },
}

/// The element a host supplies when an edit grows an array without carrying a
/// fill value of its own.
///
/// [`ArrayEditOp::MinSize`] and [`ArrayEditOp::Resize`] ask the policy for one
/// element each time they add elements, and clone it into every added slot.
/// A policy that returns `None` leaves the array unchanged: the interpreter
/// never invents an element.
///
/// Two policies are provided: an [`Option<T>`] is a fixed, possibly missing
/// fill value, and [`FillWith`] wraps a closure that computes the element only
/// when growth needs it.
pub trait ArrayFill<T> {
    /// Returns the element to fill added slots with, or `None` when the host
    /// has none.
    fn fill_element(&mut self) -> Option<T>;
}

impl<T: Clone> ArrayFill<T> for Option<T> {
    fn fill_element(&mut self) -> Self {
        self.clone()
    }
}

/// An [`ArrayFill`] that calls a closure when growth needs a fill element.
///
/// The closure runs at most once per growing instruction, and not at all for
/// programs that never grow the array without a fill of their own.
#[derive(Clone, Copy)]
pub struct FillWith<F>(pub F);

impl<F> fmt::Debug for FillWith<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FillWith(..)")
    }
}

impl<T, F: FnMut() -> Option<T>> ArrayFill<T> for FillWith<F> {
    fn fill_element(&mut self) -> Option<T> {
        (self.0)()
    }
}

/// A sparse array edit: an instruction program applied in order.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ArrayEdit<T> {
    /// Instructions, applied first to last.
    pub ops: Vec<ArrayEditOp<T>>,
}

impl<T> Default for ArrayEdit<T> {
    fn default() -> Self {
        Self { ops: Vec::new() }
    }
}

impl<T> ArrayEdit<T> {
    /// Returns `true` when the edit has no instructions and so leaves every
    /// array unchanged.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.ops.is_empty()
    }
}

impl<T: Clone> ArrayEdit<T> {
    /// Composes this stronger edit over a weaker one.
    ///
    /// The result runs `weaker`'s instructions and then this edit's, so
    /// applying it to an array with any fill policy equals applying `weaker`
    /// and then `self` with that policy.
    #[must_use]
    pub fn compose_over(&self, weaker: &Self) -> Self {
        let mut ops = Vec::with_capacity(weaker.ops.len() + self.ops.len());
        ops.extend(weaker.ops.iter().cloned());
        ops.extend(self.ops.iter().cloned());
        Self { ops }
    }

    /// Applies the edit over a dense array and returns the edited copy.
    #[must_use]
    pub fn compose_over_array(&self, weaker: &[T], fill: impl ArrayFill<T>) -> Vec<T> {
        let mut result = weaker.to_vec();
        self.apply_in_place(&mut result, fill);
        result
    }

    /// Applies the edit to `array` in place.
    ///
    /// `fill` supplies the element for [`ArrayEditOp::MinSize`] and
    /// [`ArrayEditOp::Resize`] growth; it is consulted only when one of them
    /// adds elements.
    pub fn apply_in_place(&self, array: &mut Vec<T>, mut fill: impl ArrayFill<T>) {
        for op in &self.ops {
            match op {
                ArrayEditOp::Write { src, index } => {
                    let Some(dst) = element_index(array.len(), *index) else {
                        continue;
                    };
                    let Some(value) = operand_value(array, src) else {
                        continue;
                    };
                    array[dst] = value;
                }
                ArrayEditOp::Insert { src, index } => {
                    let Some(dst) = insertion_index(array.len(), *index) else {
                        continue;
                    };
                    let Some(value) = operand_value(array, src) else {
                        continue;
                    };
                    array.insert(dst, value);
                }
                ArrayEditOp::Erase { index } => {
                    let Some(dst) = element_index(array.len(), *index) else {
                        continue;
                    };
                    array.remove(dst);
                }
                ArrayEditOp::MinSize { len } => grow_from_policy(array, *len, &mut fill),
                ArrayEditOp::MinSizeFill { len, fill } => grow_with(array, *len, fill),
                ArrayEditOp::MaxSize { len } => array.truncate(*len),
                ArrayEditOp::Resize { len } => {
                    array.truncate(*len);
                    grow_from_policy(array, *len, &mut fill);
                }
                ArrayEditOp::ResizeFill { len, fill } => {
                    array.truncate(*len);
                    grow_with(array, *len, fill);
                }
            }
        }
    }
}

/// Reads an operand against the array as edited so far.
fn operand_value<T: Clone>(array: &[T], operand: &ArrayEditOperand<T>) -> Option<T> {
    match operand {
        ArrayEditOperand::Literal(value) => Some(value.clone()),
        ArrayEditOperand::CopyFrom(index) => {
            element_index(array.len(), *index).map(|idx| array[idx].clone())
        }
    }
}

/// Resolves an index naming an existing element.
fn element_index(len: usize, index: ArrayIndex) -> Option<usize> {
    match index {
        ArrayIndex::End => None,
        ArrayIndex::Position(value) if value >= 0 => {
            let idx = usize::try_from(value).ok()?;
            (idx < len).then_some(idx)
        }
        ArrayIndex::Position(value) => {
            let offset = usize::try_from(value.unsigned_abs()).ok()?;
            len.checked_sub(offset)
        }
    }
}

/// Resolves an insertion point: any existing position, or the end.
fn insertion_index(len: usize, index: ArrayIndex) -> Option<usize> {
    match index {
        ArrayIndex::End => Some(len),
        ArrayIndex::Position(value) if value >= 0 => {
            let idx = usize::try_from(value).ok()?;
            (idx <= len).then_some(idx)
        }
        ArrayIndex::Position(value) => {
            let offset = usize::try_from(value.unsigned_abs()).ok()?;
            len.checked_sub(offset)
        }
    }
}

/// Grows `array` to `len` with the host's fill element, if it has one.
fn grow_from_policy<T: Clone>(array: &mut Vec<T>, len: usize, fill: &mut impl ArrayFill<T>) {
    if array.len() >= len {
        return;
    }
    if let Some(element) = fill.fill_element() {
        array.resize(len, element);
    }
}

/// Grows `array` to `len` with copies of `fill`.
fn grow_with<T: Clone>(array: &mut Vec<T>, len: usize, fill: &T) {
    if array.len() < len {
        array.resize(len, fill.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // Vectors ported from OpenUSD v26.08 `pxr/base/vt/testenv/testVtArrayEdit.cpp`
    // (`testBuilderAndComposition`) and `testVtArrayEdit.py`. OpenUSD fills
    // `minsize`/`resize` growth with value-initialized elements; these tests
    // supply `0` as the host fill to match.

    type Edit = ArrayEdit<i32>;
    type Op = ArrayEditOp<i32>;

    fn lit(value: i32) -> ArrayEditOperand<i32> {
        ArrayEditOperand::Literal(value)
    }

    fn edit(ops: Vec<Op>) -> Edit {
        ArrayEdit { ops }
    }

    fn apply(edit: &Edit, weaker: &[i32]) -> Vec<i32> {
        edit.compose_over_array(weaker, Some(0))
    }

    fn zero_nine() -> Edit {
        edit(vec![
            Op::Insert {
                src: lit(0),
                index: ArrayIndex::Position(0),
            },
            Op::Insert {
                src: lit(9),
                index: ArrayIndex::End,
            },
        ])
    }

    fn mix_and_trim() -> Edit {
        edit(vec![
            Op::Write {
                src: ArrayEditOperand::CopyFrom(ArrayIndex::Position(-1)),
                index: ArrayIndex::Position(2),
            },
            Op::Write {
                src: ArrayEditOperand::CopyFrom(ArrayIndex::Position(0)),
                index: ArrayIndex::Position(4),
            },
            Op::Erase {
                index: ArrayIndex::Position(-1),
            },
            Op::Erase {
                index: ArrayIndex::Position(0),
            },
        ])
    }

    #[test]
    fn compose_over_concatenates_instruction_sequences() {
        let weak = edit(vec![Op::Insert {
            src: lit(1),
            index: ArrayIndex::End,
        }]);
        let strong = edit(vec![Op::Write {
            src: lit(9),
            index: ArrayIndex::Position(0),
        }]);

        let composed = strong.compose_over(&weak);
        assert_eq!(composed.ops.len(), 2);
        assert_eq!(apply(&composed, &[]), vec![9]);
    }

    #[test]
    fn identity_edit_leaves_arrays_unchanged() {
        let identity = Edit::default();
        assert!(identity.is_identity());
        assert_eq!(apply(&identity, &[3, 1, 2]), vec![3, 1, 2]);
        assert!(!zero_nine().is_identity());
    }

    #[test]
    fn vt_prepend_append_and_self_composition() {
        let zero_nine = zero_nine();
        assert_eq!(apply(&zero_nine, &[]), vec![0, 9]);
        assert_eq!(apply(&zero_nine, &[5]), vec![0, 5, 9]);

        let zero09_nine = zero_nine.compose_over(&zero_nine);
        assert_eq!(apply(&zero09_nine, &[]), vec![0, 0, 9, 9]);
        assert_eq!(apply(&zero09_nine, &[3, 4, 5]), vec![0, 0, 3, 4, 5, 9, 9]);
    }

    #[test]
    fn vt_references_and_out_of_bounds_ops() {
        let mix_and_trim = mix_and_trim();
        assert_eq!(
            apply(&mix_and_trim, &[0, 0, 3, 4, 5, 9, 9]),
            vec![0, 9, 4, 0, 9]
        );
        // Out-of-bounds operations are ignored.
        assert_eq!(apply(&mix_and_trim, &[4, 5, 6, 7]), vec![5, 7]);

        let composed = mix_and_trim.compose_over(&zero_nine());
        assert_eq!(
            apply(&composed, &[1, 2, 3, 4, 5, 6, 7]),
            vec![1, 9, 3, 0, 5, 6, 7]
        );
        assert_eq!(apply(&composed, &[4, 5]), vec![4, 9]);
    }

    #[test]
    fn vt_size_ops() {
        let min_size10 = edit(vec![Op::MinSize { len: 10 }]);
        assert_eq!(apply(&min_size10, &[]), vec![0; 10]);
        assert_eq!(apply(&min_size10, &[7; 15]), vec![7; 15]);

        let min_size10_fill9 = edit(vec![Op::MinSizeFill { len: 10, fill: 9 }]);
        assert_eq!(apply(&min_size10_fill9, &[]), vec![9; 10]);
        assert_eq!(apply(&min_size10_fill9, &[7; 15]), vec![7; 15]);

        let max_size15 = edit(vec![Op::MaxSize { len: 15 }]);
        assert_eq!(apply(&max_size15, &[]), Vec::<i32>::new());
        assert_eq!(apply(&max_size15, &[2; 20]), vec![2; 15]);

        let size10to15 = max_size15.compose_over(&min_size10);
        assert_eq!(
            apply(&size10to15, &[1; 7]),
            vec![1, 1, 1, 1, 1, 1, 1, 0, 0, 0]
        );
        assert_eq!(apply(&size10to15, &[2; 20]), vec![2; 15]);
        assert_eq!(apply(&size10to15, &[3; 13]), vec![3; 13]);

        let size7 = edit(vec![Op::Resize { len: 7 }]);
        assert_eq!(apply(&size7, &[1; 7]), vec![1; 7]);
        assert_eq!(apply(&size7, &[]), vec![0; 7]);
        assert_eq!(apply(&size7, &[9; 27]), vec![9; 7]);

        let size7_fill3 = edit(vec![Op::ResizeFill { len: 7, fill: 3 }]);
        assert_eq!(apply(&size7_fill3, &[1; 7]), vec![1; 7]);
        assert_eq!(apply(&size7_fill3, &[]), vec![3; 7]);
        assert_eq!(apply(&size7_fill3, &[9; 27]), vec![9; 7]);
    }

    #[test]
    fn vt_nested_prepend_append_composition() {
        let prepend_append = |front: i32, back: i32| {
            edit(vec![
                Op::Insert {
                    src: lit(front),
                    index: ArrayIndex::Position(0),
                },
                Op::Insert {
                    src: lit(back),
                    index: ArrayIndex::End,
                },
            ])
        };
        let composed = prepend_append(0, 9)
            .compose_over(&prepend_append(1, 8).compose_over(&prepend_append(2, 7)));
        assert_eq!(apply(&composed, &[]), vec![0, 1, 2, 7, 8, 9]);
    }

    #[test]
    fn negative_and_end_indices() {
        let base = [10, 20, 30];
        let write = |index| edit(vec![Op::Write { src: lit(0), index }]);
        assert_eq!(apply(&write(ArrayIndex::Position(-1)), &base), [10, 20, 0]);
        assert_eq!(apply(&write(ArrayIndex::Position(-3)), &base), [0, 20, 30]);
        // Beyond the front, at the end and past the end name no element.
        assert_eq!(apply(&write(ArrayIndex::Position(-4)), &base), base);
        assert_eq!(apply(&write(ArrayIndex::Position(3)), &base), base);
        assert_eq!(apply(&write(ArrayIndex::End), &base), base);
        assert_eq!(apply(&write(ArrayIndex::Position(i64::MIN)), &base), base);

        let insert = |index| edit(vec![Op::Insert { src: lit(0), index }]);
        assert_eq!(
            apply(&insert(ArrayIndex::Position(-1)), &base),
            [10, 20, 0, 30]
        );
        assert_eq!(
            apply(&insert(ArrayIndex::Position(-3)), &base),
            [0, 10, 20, 30]
        );
        assert_eq!(
            apply(&insert(ArrayIndex::Position(3)), &base),
            [10, 20, 30, 0]
        );
        assert_eq!(apply(&insert(ArrayIndex::End), &base), [10, 20, 30, 0]);
        assert_eq!(apply(&insert(ArrayIndex::Position(-4)), &base), base);
        assert_eq!(apply(&insert(ArrayIndex::Position(4)), &base), base);

        let erase = |index| edit(vec![Op::Erase { index }]);
        assert_eq!(apply(&erase(ArrayIndex::Position(-1)), &base), [10, 20]);
        assert_eq!(apply(&erase(ArrayIndex::End), &base), base);
    }

    #[test]
    fn copy_reads_the_array_as_edited_so_far() {
        let program = edit(vec![
            Op::Insert {
                src: lit(4),
                index: ArrayIndex::End,
            },
            Op::Insert {
                src: ArrayEditOperand::CopyFrom(ArrayIndex::Position(-1)),
                index: ArrayIndex::Position(0),
            },
            // Copying from an index that does not resolve skips the instruction.
            Op::Insert {
                src: ArrayEditOperand::CopyFrom(ArrayIndex::End),
                index: ArrayIndex::Position(0),
            },
        ]);
        assert_eq!(apply(&program, &[1, 2]), [4, 1, 2, 4]);
    }

    #[test]
    fn growth_without_a_fill_is_skipped() {
        let grow = edit(vec![Op::MinSize { len: 4 }, Op::Resize { len: 3 }]);
        assert_eq!(grow.compose_over_array(&[1], None), [1]);
        assert_eq!(grow.compose_over_array(&[1, 2, 3, 4, 5], None), [1, 2, 3]);
        assert_eq!(grow.compose_over_array(&[1], Some(7)), [1, 7, 7]);
    }

    #[test]
    fn lazy_fill_runs_only_for_growth() {
        let mut calls = 0;
        let program = edit(vec![
            Op::MinSize { len: 2 },
            Op::Resize { len: 1 },
            Op::MinSizeFill { len: 3, fill: 5 },
            Op::Resize { len: 4 },
        ]);
        let result = program.compose_over_array(
            &[1, 2],
            FillWith(|| {
                calls += 1;
                Some(8)
            }),
        );
        assert_eq!(result, [1, 5, 5, 8]);
        assert_eq!(calls, 1, "only the growing `resize` asks for a fill");
    }
}
