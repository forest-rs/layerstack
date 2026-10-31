// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An undo history over applied transactions' inverses.

use alloc::{collections::VecDeque, string::String, vec::Vec};
use core::fmt;

use super::{Applied, EditError, Rejection, Transaction};
use crate::{LiveStage, doc::LayerId, doc::LayerStore};

/// An undo stack and a redo stack of steps, each step the inverse of one
/// or more applied transactions, combined.
///
/// The history applies nothing on its own: the host applies a
/// transaction ([`LiveStage::apply`]) and records the inverse its receipt
/// carries ([`UndoHistory::record`]). [`UndoHistory::undo`] applies the
/// newest step's inverse as one transaction and moves the step to the
/// redo stack, holding the inverse of the undo, which
/// [`UndoHistory::redo`] applies. Recording clears the redo stack: a new
/// edit forks history.
///
/// - **Grouping.** Transactions recorded between
///   [`UndoHistory::begin_group`] and [`UndoHistory::end_group`] become
///   one step, named by the group's label, whose inverses are combined
///   ([`Transaction::append`]), so the step undoes as one atomic
///   transaction. Groups nest; a nested group flattens into the outer
///   one, and the outermost label names the step. A group that recorded
///   nothing adds no step, and neither does an inverse with no edits (a
///   transaction that changed nothing).
/// - **Staleness.** An inverse restores each slot only while it holds what
///   its transaction wrote there ([`Transaction`]). An edit the history
///   did not record (applied without recording, by another tool, or
///   through the store directly) that changed such a slot makes the step
///   stale: undoing it fails with [`HistoryError::Stale`], nothing is
///   applied, and the step stays where it was. The history never skips a
///   stale step; [`UndoHistory::reset`] forgets every step when the host
///   gives up on them. Outside edits of other slots, even of the same
///   spec, do not stop an undo.
/// - **Bound.** [`UndoHistory::with_limit`] keeps at most that many steps
///   on both stacks together. Recording past it drops the oldest undo
///   step, which [`Recorded::Step`] returns and [`UndoHistory::dropped`]
///   counts.
/// - **Unwinding.** [`UndoHistory::abandon_group`] undoes what the
///   innermost open group recorded and closes it;
///   [`UndoHistory::unwind_to`] undoes every step recorded since a
///   [`HistoryMark`]. Both undo as one transaction and forget what they
///   undid: nothing goes to the redo stack.
///
/// A `layerstack_io::StageDocument` host undoes through
/// `StageDocument::parts_mut` and then loads what the undo asks for with
/// `StageDocument::load_requested_assets`, as `StageDocument::apply`
/// does for an edit.
///
/// There is no OpenUSD counterpart to check this against: OpenUSD records
/// no inverses (`SdfChangeBlock` neither rolls back nor undoes), and its
/// undo helpers are not part of `usd-core`. Its behaviour rests on the
/// transaction invariants: an inverse restores exactly the slots its
/// transaction wrote, applies all or nothing, and its own inverse redoes.
///
/// Spec: AOUSD Core §7 (scene description), whose specs the inverses
/// restore.
///
/// ```
/// use layerstack::{
///     EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PropertySpec, PropertyType,
///     Specifier, StageOptions, Time, Transaction, UndoHistory, Value,
/// };
///
/// let mut store = InMemoryStore::default();
/// let root = LayerId(1);
/// store.insert_layer(Layer::new(root));
/// let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
/// let at = EditTarget::for_layer(root);
/// let (rock, radius) = (store.path("/Rock"), store.property_path("/Rock.radius"));
/// let double = PropertyType::parse("double").unwrap();
/// let mut history = UndoHistory::with_limit(100);
///
/// // Two transactions, one step.
/// history.begin_group("define and set radius");
/// let mut define = Transaction::new();
/// define.create_prim(at.prim(rock), Specifier::Def, None);
/// history.record("define", live.apply(&mut store, &define).unwrap().inverse);
/// let mut set = Transaction::new();
/// set.create_property(at.property(radius), PropertySpec::typed_attribute(double))
///     .set_default(at.property(radius), Value::Double(2.0));
/// history.record("set radius", live.apply(&mut store, &set).unwrap().inverse);
/// history.end_group().unwrap();
/// assert_eq!(history.undo_steps().len(), 1);
/// assert_eq!(history.next_undo().unwrap().label(), "define and set radius");
/// assert_eq!(history.next_undo().unwrap().layers(), [root]);
///
/// let undone = history.undo(&mut live, &mut store).unwrap();
/// assert_eq!(undone.changes.removed, [rock]);
/// assert!(live.stage().prim(rock, &store).is_none());
/// history.redo(&mut live, &mut store).unwrap();
/// let value = live.view(&store).attribute(radius).unwrap().value::<f64>(Time::Default);
/// assert_eq!(value, Some(2.0));
/// ```
#[derive(Clone, Debug, Default)]
pub struct UndoHistory {
    /// Undo steps, oldest first.
    undo: VecDeque<HistoryStep>,
    /// Redo steps, the next to redo last.
    redo: Vec<HistoryStep>,
    limit: Option<usize>,
    group: Option<OpenGroup>,
    /// The id the next step takes.
    next_id: u64,
    /// Steps the bound dropped, in all.
    dropped: u64,
    /// Moves on every reset, so marks taken before one are gone.
    epoch: u64,
}

/// The inverses recorded while a group is open.
#[derive(Clone, Debug)]
struct OpenGroup {
    /// The inverses, in the order their transactions applied.
    inverses: Vec<Transaction>,
    /// Each open level, outermost first: its label and the number of
    /// inverses recorded before it began.
    levels: Vec<(String, usize)>,
}

/// One step of an [`UndoHistory`]: the transaction that undoes it (on the
/// undo stack) or redoes it (on the redo stack), with its label.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryStep {
    id: u64,
    label: String,
    transaction: Transaction,
    layers: Vec<LayerId>,
    receipts: usize,
}

impl HistoryStep {
    /// The label it was recorded with, or its group's.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The layers undoing (or redoing) it writes, ascending: those the
    /// recorded transactions changed ([`Transaction::layers`]).
    #[must_use]
    pub fn layers(&self) -> &[LayerId] {
        &self.layers
    }

    /// The number of applied transactions it combines: one for a
    /// [`UndoHistory::record`] outside a group, the number recorded in it
    /// for a group.
    #[must_use]
    pub fn receipts(&self) -> usize {
        self.receipts
    }

    /// The transaction that undoes the step, or redoes it for a step on
    /// the redo stack: the recorded inverses combined, newest first.
    #[must_use]
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }
}

/// What [`UndoHistory::record`] or [`UndoHistory::end_group`] did.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Recorded {
    /// A step went onto the undo stack, and the redo stack was cleared.
    Step {
        /// The oldest step, which the bound ([`UndoHistory::with_limit`])
        /// dropped to make room, if any.
        dropped: Option<HistoryStep>,
    },
    /// The inverse joined the open group, which becomes one step when its
    /// outermost level ends; the redo stack was cleared.
    Grouped,
    /// Nothing was recorded: the inverse has no edits, or the group that
    /// ended recorded none, or a nested group ended inside its outer one.
    /// The redo stack is as it was, unless the group recorded something.
    Nothing,
}

/// A point in an [`UndoHistory`] to unwind back to
/// ([`UndoHistory::unwind_to`]), taken with [`UndoHistory::mark`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryMark {
    epoch: u64,
    /// The newest undo step when the mark was taken.
    top: Option<u64>,
    /// Steps dropped when the mark was taken.
    dropped: u64,
}

/// Why an [`UndoHistory`] operation failed. When one is returned, no layer
/// was changed and the history is as it was.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum HistoryError {
    /// There is no step to undo, or to redo.
    Empty,
    /// A group is open ([`UndoHistory::begin_group`]): undo, redo and
    /// [`UndoHistory::unwind_to`] wait for it to end or be abandoned.
    GroupOpen {
        /// The outermost open group's label.
        label: String,
    },
    /// [`UndoHistory::end_group`] or [`UndoHistory::abandon_group`] with no
    /// group open.
    NoGroup,
    /// The mark passed to [`UndoHistory::unwind_to`] is gone: the step it
    /// stands on was undone, dropped by the bound or reset, so undoing
    /// what is above it would not get back to it.
    MarkGone,
    /// The step's transaction no longer matches the layers: an edit the
    /// history did not record changed a slot it restores since
    /// ([`EditError::StaleValue`], [`EditError::StaleGeneration`], or a
    /// diverged inverse step, [`Rejection::Diverged`]).
    Stale {
        /// The label of the step; for [`UndoHistory::abandon_group`], the
        /// innermost open group's, and for [`UndoHistory::unwind_to`], the
        /// newest step's.
        label: String,
        /// Why the transaction was refused.
        error: EditError,
    },
    /// The step's transaction was refused for another reason, such as a
    /// store the stage was not composed from ([`EditError::ForeignStore`]).
    Rejected {
        /// The label of the step, as for [`Self::Stale`].
        label: String,
        /// Why the transaction was refused.
        error: EditError,
    },
}

impl HistoryError {
    /// The stable code naming this error's kind.
    ///
    /// | Variant | Code |
    /// |---|---|
    /// | [`Self::Empty`] | `history.empty` |
    /// | [`Self::GroupOpen`] | `history.group_open` |
    /// | [`Self::NoGroup`] | `history.no_group` |
    /// | [`Self::MarkGone`] | `history.mark_gone` |
    /// | [`Self::Stale`] | `history.stale` |
    /// | [`Self::Rejected`] | `history.rejected` |
    ///
    /// Codes are part of the public contract: a released code keeps its
    /// spelling and meaning.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Empty => "history.empty",
            Self::GroupOpen { .. } => "history.group_open",
            Self::NoGroup => "history.no_group",
            Self::MarkGone => "history.mark_gone",
            Self::Stale { .. } => "history.stale",
            Self::Rejected { .. } => "history.rejected",
        }
    }

    /// The edit error behind a [`Self::Stale`] or [`Self::Rejected`].
    #[must_use]
    pub fn edit_error(&self) -> Option<&EditError> {
        match self {
            Self::Stale { error, .. } | Self::Rejected { error, .. } => Some(error),
            _ => None,
        }
    }

    /// The error in words, an edit error's paths and names written as
    /// text with `store` ([`EditError::display`]).
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl fmt::Display + 'a {
        struct Shown<'a>(&'a HistoryError, &'a dyn LayerStore);
        impl fmt::Display for Shown<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self.0 {
                    HistoryError::Stale { label, error } => {
                        write!(f, "{label:?} is stale: {}", error.display(self.1))
                    }
                    HistoryError::Rejected { label, error } => {
                        write!(f, "{label:?} was refused: {}", error.display(self.1))
                    }
                    other => write!(f, "{other}"),
                }
            }
        }
        Shown(self, store)
    }

    pub(crate) fn from_edit(label: &str, error: EditError) -> Self {
        let label = String::from(label);
        let stale = matches!(
            error,
            EditError::StaleValue { .. }
                | EditError::StaleGeneration { .. }
                | EditError::Rejected {
                    reason: Rejection::Diverged(_),
                    ..
                }
        );
        if stale {
            Self::Stale { label, error }
        } else {
            Self::Rejected { label, error }
        }
    }
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("there is no step"),
            Self::GroupOpen { label } => write!(f, "the group {label:?} is open"),
            Self::NoGroup => f.write_str("no group is open"),
            Self::MarkGone => f.write_str(
                "the mark's step was undone, dropped or reset, so unwinding cannot get back to it",
            ),
            Self::Stale { label, error } => write!(f, "{label:?} is stale: {error}"),
            Self::Rejected { label, error } => write!(f, "{label:?} was refused: {error}"),
        }
    }
}

impl core::error::Error for HistoryError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        self.edit_error()
            .map(|error| error as &(dyn core::error::Error + 'static))
    }
}

impl UndoHistory {
    /// Creates an empty history that keeps every step.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty history that keeps at most `limit` steps, undo and
    /// redo together. Recording past it drops the oldest undo step
    /// ([`Recorded::Step`]); a limit of 0 keeps none.
    #[must_use]
    pub fn with_limit(limit: usize) -> Self {
        Self {
            limit: Some(limit),
            ..Self::default()
        }
    }

    /// The most steps it keeps, `None` for no bound.
    #[must_use]
    pub fn limit(&self) -> Option<usize> {
        self.limit
    }

    /// Records `inverse`, the inverse an applied transaction's receipt
    /// carries ([`Applied::inverse`], or what [`Transaction::apply`]
    /// returns), as a step labelled `label`, and clears the redo stack.
    ///
    /// Inside a group the inverse joins the group, and `label` is unused:
    /// the group's label names the step. An inverse with no edits records
    /// nothing. Do not record the receipts [`Self::undo`] and
    /// [`Self::redo`] return: the history holds their inverses already.
    pub fn record(&mut self, label: impl Into<String>, inverse: Transaction) -> Recorded {
        if inverse.is_empty() {
            return Recorded::Nothing;
        }
        self.redo.clear();
        match &mut self.group {
            Some(group) => {
                group.inverses.push(inverse);
                Recorded::Grouped
            }
            None => self.push(label.into(), inverse, 1),
        }
    }

    /// Opens a group labelled `label`: what is recorded until the matching
    /// [`Self::end_group`] becomes one step. Inside an open group, it opens
    /// a nested level, which flattens into the outer one; its label names
    /// it only for [`Self::abandon_group`] errors.
    pub fn begin_group(&mut self, label: impl Into<String>) {
        let group = self.group.get_or_insert_with(|| OpenGroup {
            inverses: Vec::new(),
            levels: Vec::new(),
        });
        group.levels.push((label.into(), group.inverses.len()));
    }

    /// Closes the innermost open group. Closing the outermost pushes what
    /// the group recorded as one step, named by its label; a group that
    /// recorded nothing adds no step.
    ///
    /// # Errors
    ///
    /// [`HistoryError::NoGroup`] when no group is open.
    pub fn end_group(&mut self) -> Result<Recorded, HistoryError> {
        let group = self.group.as_mut().ok_or(HistoryError::NoGroup)?;
        let (label, _) = group.levels.pop().expect("an open group has a level");
        if !group.levels.is_empty() {
            return Ok(Recorded::Nothing);
        }
        let group = self.group.take().expect("open");
        let receipts = group.inverses.len();
        if receipts == 0 {
            return Ok(Recorded::Nothing);
        }
        Ok(self.push(label, combine(group.inverses), receipts))
    }

    /// Undoes what the innermost open group recorded, as one transaction,
    /// and closes it, adding no step: what it recorded is forgotten. An
    /// enclosing group keeps what it recorded before. Returns the receipt
    /// of the undo, or `None` when the group recorded nothing.
    ///
    /// # Errors
    ///
    /// [`HistoryError::NoGroup`] when no group is open;
    /// [`HistoryError::Stale`] or [`HistoryError::Rejected`] when the undo
    /// is refused: nothing changed and the group stays open.
    pub fn abandon_group(
        &mut self,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
    ) -> Result<Option<Applied>, HistoryError> {
        let group = self.group.as_mut().ok_or(HistoryError::NoGroup)?;
        let (label, start) = group.levels.last().expect("an open group has a level");
        let applied = if group.inverses.len() > *start {
            let undo = combine(group.inverses[*start..].to_vec());
            let applied = live
                .apply(store, &undo)
                .map_err(|error| HistoryError::from_edit(label, error))?;
            group.inverses.truncate(*start);
            Some(applied)
        } else {
            None
        };
        group.levels.pop();
        if group.levels.is_empty() {
            self.group = None;
        }
        Ok(applied)
    }

    /// Whether a group is open, and its outermost label.
    #[must_use]
    pub fn open_group(&self) -> Option<&str> {
        self.group.as_ref().map(|group| group.levels[0].0.as_str())
    }

    /// The number of open group levels: 0 when no group is open.
    #[must_use]
    pub fn group_depth(&self) -> usize {
        self.group.as_ref().map_or(0, |group| group.levels.len())
    }

    /// Undoes the newest step: applies its transaction through `live`
    /// (which recomposes) and moves the step to the redo stack, holding
    /// the inverse of the undo. Returns the undo's receipt; its
    /// [`Applied::inverse`] is a copy of what the redo step holds, for
    /// inspection, not for recording.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Empty`] when there is no step,
    /// [`HistoryError::GroupOpen`] while a group is open, and
    /// [`HistoryError::Stale`] (an edit the history did not record changed
    /// what the step restores) or [`HistoryError::Rejected`] when the undo
    /// is refused. Either way nothing changed and the step stays.
    pub fn undo(
        &mut self,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
    ) -> Result<Applied, HistoryError> {
        self.check_closed()?;
        let step = self.undo.back().ok_or(HistoryError::Empty)?;
        let applied = live
            .apply(store, &step.transaction)
            .map_err(|error| HistoryError::from_edit(&step.label, error))?;
        let step = self.undo.pop_back().expect("checked");
        self.redo.push(step.turned(applied.inverse.clone()));
        Ok(applied)
    }

    /// Redoes the most recently undone step, as [`Self::undo`] undoes one,
    /// and moves it back to the undo stack.
    ///
    /// # Errors
    ///
    /// As [`Self::undo`]: [`HistoryError::Empty`] when nothing was undone.
    pub fn redo(
        &mut self,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
    ) -> Result<Applied, HistoryError> {
        self.check_closed()?;
        let step = self.redo.last().ok_or(HistoryError::Empty)?;
        let applied = live
            .apply(store, &step.transaction)
            .map_err(|error| HistoryError::from_edit(&step.label, error))?;
        let step = self.redo.pop().expect("checked");
        self.undo.push_back(step.turned(applied.inverse.clone()));
        Ok(applied)
    }

    /// Whether [`Self::undo`] has a step, and no group is open.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.group.is_none() && !self.undo.is_empty()
    }

    /// Whether [`Self::redo`] has a step, and no group is open.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.group.is_none() && !self.redo.is_empty()
    }

    /// The step [`Self::undo`] would undo.
    #[must_use]
    pub fn next_undo(&self) -> Option<&HistoryStep> {
        self.undo.back()
    }

    /// The step [`Self::redo`] would redo.
    #[must_use]
    pub fn next_redo(&self) -> Option<&HistoryStep> {
        self.redo.last()
    }

    /// The undo steps, oldest first: the next to undo is last.
    pub fn undo_steps(&self) -> impl ExactSizeIterator<Item = &HistoryStep> + DoubleEndedIterator {
        self.undo.iter()
    }

    /// The redo steps, the most recently undone last: the next to redo is
    /// last.
    pub fn redo_steps(&self) -> impl ExactSizeIterator<Item = &HistoryStep> + DoubleEndedIterator {
        self.redo.iter()
    }

    /// The number of steps the bound dropped since the history was made.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Forgets every step, both stacks and any open group, without
    /// applying anything: the layers keep every edit. Marks taken before
    /// are gone.
    pub fn reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.group = None;
        self.epoch += 1;
    }

    /// A mark of where the history stands, for [`Self::unwind_to`]. Steps
    /// recorded in a group open now are above the mark once it ends.
    #[must_use]
    pub fn mark(&self) -> HistoryMark {
        HistoryMark {
            epoch: self.epoch,
            top: self.undo.back().map(|step| step.id),
            dropped: self.dropped,
        }
    }

    /// Undoes every step recorded since `mark`, as one transaction, and
    /// forgets them: they do not go to the redo stack, which is left as it
    /// is. Returns the receipt of the undo, or `None` when no step was
    /// recorded since.
    ///
    /// # Errors
    ///
    /// [`HistoryError::GroupOpen`] while a group is open;
    /// [`HistoryError::MarkGone`] when the mark's step was undone (redo it
    /// first), or dropped by the bound or reset since;
    /// [`HistoryError::Stale`] or [`HistoryError::Rejected`] when the undo
    /// is refused. Either way nothing changed.
    pub fn unwind_to(
        &mut self,
        mark: HistoryMark,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
    ) -> Result<Option<Applied>, HistoryError> {
        self.check_closed()?;
        if mark.epoch != self.epoch {
            return Err(HistoryError::MarkGone);
        }
        let keep = match mark.top {
            Some(id) => {
                1 + self
                    .undo
                    .iter()
                    .rposition(|step| step.id == id)
                    .ok_or(HistoryError::MarkGone)?
            }
            None if mark.dropped == self.dropped => 0,
            None => return Err(HistoryError::MarkGone),
        };
        if keep == self.undo.len() {
            return Ok(None);
        }
        let mut undo = Transaction::new();
        for step in self.undo.range(keep..).rev() {
            undo.append(step.transaction.clone());
        }
        let label = &self.undo.back().expect("steps above the mark").label;
        let applied = live
            .apply(store, &undo)
            .map_err(|error| HistoryError::from_edit(label, error))?;
        self.undo.truncate(keep);
        Ok(Some(applied))
    }

    fn check_closed(&self) -> Result<(), HistoryError> {
        match self.open_group() {
            Some(label) => Err(HistoryError::GroupOpen {
                label: String::from(label),
            }),
            None => Ok(()),
        }
    }

    fn push(&mut self, label: String, transaction: Transaction, receipts: usize) -> Recorded {
        let id = self.next_id;
        self.next_id += 1;
        self.undo.push_back(HistoryStep {
            id,
            label,
            layers: transaction.layers(),
            transaction,
            receipts,
        });
        let over = self
            .limit
            .is_some_and(|limit| self.undo.len() + self.redo.len() > limit);
        let dropped = over.then(|| self.undo.pop_front()).flatten();
        if dropped.is_some() {
            self.dropped += 1;
        }
        Recorded::Step { dropped }
    }
}

impl HistoryStep {
    /// The step with `transaction` in place of its own: the step moved to
    /// the other stack. Its layers stay: undoing writes the layers the
    /// edits wrote.
    fn turned(self, transaction: Transaction) -> Self {
        Self {
            transaction,
            ..self
        }
    }
}

/// The inverses of transactions applied in order, combined into one that
/// undoes them all: the newest first.
fn combine(inverses: Vec<Transaction>) -> Transaction {
    let mut combined = Transaction::new();
    for inverse in inverses.into_iter().rev() {
        combined.append(inverse);
    }
    combined
}
