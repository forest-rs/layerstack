// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The selection globs of the `variant` collection predicate.
//!
//! OpenUSD matches them with `ArchRegex(glob, ArchRegex::GLOB)`
//! (`pxr/base/arch/regex.cpp`): the glob becomes a POSIX extended regular
//! expression by replacing `.` with `\.`, then `*` with `.*`, then `?`
//! with `.`, which `regcomp` compiles and `regexec` searches for anywhere
//! in the selection. Any other regular expression syntax in the glob
//! (`|`, `(…)`, `[…]`, `+`, `{m,n}`, `^`, `$`) keeps its meaning.
//!
//! What `regcomp` accepts beyond what POSIX defines differs between
//! platforms (the libc of macOS accepts `()`, `a**` and `\a`, glibc other
//! things, and their repetition limits differ), so OpenUSD itself matches
//! differently on each. This accepts POSIX-defined extended regular
//! expressions, where platforms agree, and rejects the rest, so the
//! predicate fails to bind and its expression matches nothing (as one
//! `regcomp` rejects does in OpenUSD). It accepts:
//!
//! - ordinary characters, `.`, `\` before a special character, bracket
//!   expressions (`[a-z]`, `[^…]`, a leading `]`, the twelve `[:class:]`
//!   names), groups, alternation of non-empty branches, the anchors `^`
//!   and `$`;
//! - one of `*`, `+`, `?`, `{m}`, `{m,}`, `{m,n}` after a character,
//!   bracket expression or group, with `m <= n <= 255` (`RE_DUP_MAX` on
//!   macOS, the POSIX minimum; glibc allows more).
//!
//! It rejects three kinds of expression, and says which:
//!
//! - **invalid**, as `regcomp` rejects it: an empty pattern, an unclosed
//!   `(` or `[`, counts out of order or over 255, an unknown `[:class:]`;
//! - **undefined** syntax, which POSIX leaves undefined and platforms read
//!   differently: an unmatched `)`, empty groups and branches, a
//!   repetition of nothing, of an anchor or of a repetition, `\` before an
//!   ordinary character (including back-references), `{` that does not
//!   start an interval;
//! - **unsupported** POSIX features this does not implement: equivalence
//!   classes (`[=a=]`), collating symbols (`[.x.]`), and expressions that
//!   expand to more than [`MAX_PROGRAM`] instructions.
//!
//! Matching is a Pike VM: a breadth-first simulation of the expression's
//! automaton, without recursion or backtracking, in time proportional to
//! the program's size times the selection's length.

use alloc::{format, string::String, vec::Vec};

/// The largest repetition count, `RE_DUP_MAX`.
pub(crate) const MAX_REPEAT: usize = 255;

/// The most instructions an expression may compile to, bounded
/// repetitions expanded.
pub(crate) const MAX_PROGRAM: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
enum Inst {
    Char(char),
    Any,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
    /// Continue at both targets.
    Split(usize, usize),
    Jump(usize),
    Start,
    End,
    Match,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClassItem {
    Range(char, char),
    Named(NamedClass),
}

/// A POSIX character class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NamedClass {
    Alpha,
    Digit,
    Alnum,
    Upper,
    Lower,
    Space,
    Blank,
    Punct,
    Xdigit,
    Cntrl,
    Print,
    Graph,
}

impl NamedClass {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "alpha" => Self::Alpha,
            "digit" => Self::Digit,
            "alnum" => Self::Alnum,
            "upper" => Self::Upper,
            "lower" => Self::Lower,
            "space" => Self::Space,
            "blank" => Self::Blank,
            "punct" => Self::Punct,
            "xdigit" => Self::Xdigit,
            "cntrl" => Self::Cntrl,
            "print" => Self::Print,
            "graph" => Self::Graph,
            _ => return None,
        })
    }

    fn contains(self, c: char) -> bool {
        match self {
            Self::Alpha => c.is_ascii_alphabetic(),
            Self::Digit => c.is_ascii_digit(),
            Self::Alnum => c.is_ascii_alphanumeric(),
            Self::Upper => c.is_ascii_uppercase(),
            Self::Lower => c.is_ascii_lowercase(),
            Self::Space => c.is_ascii_whitespace() || c == '\x0b',
            Self::Blank => c == ' ' || c == '\t',
            Self::Punct => c.is_ascii_punctuation(),
            Self::Xdigit => c.is_ascii_hexdigit(),
            Self::Cntrl => c.is_ascii_control(),
            Self::Print => (' '..='~').contains(&c),
            Self::Graph => ('!'..='~').contains(&c),
        }
    }
}

/// A piece of program whose jumps are relative to its own start, so pieces
/// concatenate by appending.
#[derive(Clone, Debug, Default, PartialEq)]
struct Fragment {
    insts: Vec<Rel>,
}

/// An instruction with relative jump targets.
#[derive(Clone, Debug, PartialEq)]
enum Rel {
    Inst(Inst),
    Split(isize, isize),
    Jump(isize),
}

fn offset(n: usize) -> isize {
    isize::try_from(n).unwrap_or(isize::MAX)
}

impl Fragment {
    fn one(inst: Inst) -> Self {
        Self {
            insts: Vec::from([Rel::Inst(inst)]),
        }
    }

    fn len(&self) -> usize {
        self.insts.len()
    }

    fn append(&mut self, other: &Self) {
        self.insts.extend(other.insts.iter().cloned());
    }

    /// `a|b`.
    fn alternate(a: Self, b: &Self) -> Self {
        let mut out = Self::default();
        out.insts.push(Rel::Split(1, offset(a.len() + 2)));
        out.append(&a);
        out.insts.push(Rel::Jump(offset(b.len() + 1)));
        out.append(b);
        out
    }

    /// `a*`.
    fn star(a: &Self) -> Self {
        let mut out = Self::default();
        out.insts.push(Rel::Split(1, offset(a.len() + 2)));
        out.append(a);
        out.insts.push(Rel::Jump(-offset(a.len() + 1)));
        out
    }

    /// `a?`.
    fn optional(a: &Self) -> Self {
        let mut out = Self::default();
        out.insts.push(Rel::Split(1, offset(a.len() + 1)));
        out.append(a);
        out
    }

    /// `a{min,max}` (`max` `None` for unbounded), or `None` when it would
    /// exceed [`MAX_PROGRAM`].
    fn repeat(a: &Self, min: usize, max: Option<usize>) -> Option<Self> {
        let copies = max.unwrap_or(min + 1).max(1);
        if copies.saturating_mul(a.len() + 2) > MAX_PROGRAM {
            return None;
        }
        let mut out = Self::default();
        for _ in 0..min {
            out.append(a);
        }
        match max {
            None => out.append(&Self::star(a)),
            Some(max) => {
                for _ in min..max {
                    out.append(&Self::optional(a));
                }
            }
        }
        Some(out)
    }
}

/// A compiled variant selection glob.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SelectionGlob {
    program: Vec<Inst>,
}

/// One nesting level of the parse: the finished branches of its
/// alternation, the branch being built, and the atom a repetition may
/// still apply to.
#[derive(Default)]
struct Level {
    branches: Vec<Fragment>,
    branch: Fragment,
    /// Whether the branch being built has an authored atom. An atom
    /// repeated zero times (`a{0}`) compiles to no instructions yet makes
    /// the branch non-empty.
    authored: bool,
    pending: Option<Pending>,
}

struct Pending {
    fragment: Fragment,
    /// Whether a repetition may apply: not to an anchor or a repetition.
    repeatable: bool,
}

impl Level {
    fn flush(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.branch.append(&pending.fragment);
        }
    }

    fn push(&mut self, fragment: Fragment, repeatable: bool) {
        self.flush();
        self.authored = true;
        self.pending = Some(Pending {
            fragment,
            repeatable,
        });
    }

    /// Ends the branch being built at a `|`; `false` when it is empty.
    fn next_branch(&mut self) -> bool {
        self.flush();
        if !self.authored {
            return false;
        }
        let branch = core::mem::take(&mut self.branch);
        self.branches.push(branch);
        self.authored = false;
        true
    }

    /// The level's alternation; `None` when a branch is empty (has no
    /// authored atom, as opposed to atoms that compile to nothing).
    fn finish(mut self) -> Option<Fragment> {
        self.flush();
        if !self.authored {
            return None;
        }
        self.branches.push(self.branch);
        let mut branches = self.branches.into_iter().rev();
        let mut out = branches.next()?;
        for branch in branches {
            out = Fragment::alternate(branch, &out);
        }
        Some(out)
    }
}

impl SelectionGlob {
    /// Compiles `glob` as `ArchRegex` does with `GLOB`.
    ///
    /// # Errors
    ///
    /// Why the expression is not one this accepts (see the module
    /// documentation).
    pub(crate) fn compile(glob: &str) -> Result<Self, String> {
        if glob.is_empty() {
            return Err("invalid: an empty pattern".into());
        }
        let regex = glob
            .replace('.', "\\.")
            .replace('*', ".*")
            .replace('?', ".");
        let chars: Vec<char> = regex.chars().collect();
        let error = |what: &str, at: usize| format!("{what} at character {} of `{regex}`", at + 1);
        let mut levels: Vec<Level> = Vec::from([Level::default()]);
        // The instructions of every fragment built so far, and the jumps
        // their alternations will add: the program's size once joined.
        let mut size: usize = 0;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '(' {
                levels.push(Level::default());
                i += 1;
                continue;
            }
            if c == ')' {
                if levels.len() == 1 {
                    return Err(error("undefined: an unmatched `)`", i));
                }
                let inner = levels.pop().and_then(Level::finish);
                let (Some(inner), Some(parent)) = (inner, levels.last_mut()) else {
                    return Err(error("undefined: an empty group or branch", i));
                };
                parent.push(inner, true);
                i += 1;
                continue;
            }
            let Some(level) = levels.last_mut() else {
                return Err(error("undefined: an unmatched `)`", i));
            };
            let mut next = i + 1;
            match c {
                '|' => {
                    if !level.next_branch() {
                        return Err(error("undefined: an empty branch", i));
                    }
                    size += 2;
                }
                '*' | '+' | '?' | '{' => {
                    let (min, max) = if c == '{' {
                        let Some((min, max, after)) = interval(&chars, i + 1) else {
                            return Err(error("undefined: a `{` that starts no interval", i));
                        };
                        next = after;
                        (min, max)
                    } else if c == '*' {
                        (0, None)
                    } else if c == '+' {
                        (1, None)
                    } else {
                        (0, Some(1))
                    };
                    if min > MAX_REPEAT || max.is_some_and(|max| max > MAX_REPEAT || max < min) {
                        return Err(error(
                            "invalid: a repetition count over 255 or out of order",
                            i,
                        ));
                    }
                    let Some(pending) = level.pending.take().filter(|p| p.repeatable) else {
                        return Err(error(
                            "undefined: a repetition of nothing, an anchor or a repetition",
                            i,
                        ));
                    };
                    let Some(repeated) = Fragment::repeat(&pending.fragment, min, max) else {
                        return Err(error("unsupported: an expression too large", i));
                    };
                    size = size - pending.fragment.len() + repeated.len();
                    level.pending = Some(Pending {
                        fragment: repeated,
                        repeatable: false,
                    });
                }
                '\\' => {
                    let Some(&escaped) = chars.get(i + 1) else {
                        return Err(error("invalid: a trailing `\\`", i));
                    };
                    if !".[]\\()*+?{}|^$".contains(escaped) {
                        return Err(error("undefined: `\\` before an ordinary character", i));
                    }
                    level.push(Fragment::one(Inst::Char(escaped)), true);
                    size += 1;
                    next = i + 2;
                }
                '[' => {
                    let (inst, after) = bracket(&chars, i + 1).map_err(|what| error(what, i))?;
                    level.push(Fragment::one(inst), true);
                    size += 1;
                    next = after;
                }
                '^' | '$' => {
                    let anchor = if c == '^' { Inst::Start } else { Inst::End };
                    level.push(Fragment::one(anchor), false);
                    size += 1;
                }
                '.' => {
                    level.push(Fragment::one(Inst::Any), true);
                    size += 1;
                }
                c => {
                    level.push(Fragment::one(Inst::Char(c)), true);
                    size += 1;
                }
            }
            if size > MAX_PROGRAM {
                return Err(error("unsupported: an expression too large", i));
            }
            i = next;
        }
        if levels.len() != 1 {
            return Err(format!("invalid: an unmatched `(` in `{regex}`"));
        }
        let Some(fragment) = levels.pop().and_then(Level::finish) else {
            return Err(format!("undefined: an empty branch in `{regex}`"));
        };
        if fragment.len() + 1 > MAX_PROGRAM {
            return Err(format!("unsupported: `{regex}` is too large"));
        }
        let mut program = Vec::with_capacity(fragment.len() + 1);
        let target = |pc: usize, delta: isize| pc.saturating_add_signed(delta);
        for (pc, rel) in fragment.insts.into_iter().enumerate() {
            program.push(match rel {
                Rel::Inst(inst) => inst,
                Rel::Split(a, b) => Inst::Split(target(pc, a), target(pc, b)),
                Rel::Jump(a) => Inst::Jump(target(pc, a)),
            });
        }
        program.push(Inst::Match);
        Ok(Self { program })
    }

    /// Whether the expression is found anywhere in `text`.
    pub(crate) fn is_found_in(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        let size = self.program.len();
        let mut current = Threads::new(size);
        let mut next = Threads::new(size);
        let mut stack = Vec::new();
        for at in 0..=chars.len() {
            // A search: a thread starts at every position.
            if self.add(&mut current, &mut stack, 0, at, chars.len()) {
                return true;
            }
            let Some(&c) = chars.get(at) else {
                break;
            };
            next.clear();
            for index in 0..current.len() {
                let pc = current.dense[index];
                let step = match &self.program[pc] {
                    Inst::Char(want) => *want == c,
                    Inst::Any => c != '\n',
                    Inst::Class { negated, items } => {
                        let inside = items.iter().any(|item| match item {
                            ClassItem::Range(lo, hi) => (*lo..=*hi).contains(&c),
                            ClassItem::Named(class) => class.contains(c),
                        });
                        inside != *negated && !(*negated && c == '\n')
                    }
                    _ => false,
                };
                if step && self.add(&mut next, &mut stack, pc + 1, at + 1, chars.len()) {
                    return true;
                }
            }
            core::mem::swap(&mut current, &mut next);
        }
        false
    }

    /// Adds the thread at `pc`, following splits, jumps and anchors at
    /// position `at` without recursion; whether a thread reached `Match`.
    fn add(
        &self,
        threads: &mut Threads,
        stack: &mut Vec<usize>,
        pc: usize,
        at: usize,
        len: usize,
    ) -> bool {
        stack.clear();
        stack.push(pc);
        while let Some(pc) = stack.pop() {
            if !threads.insert(pc) {
                continue;
            }
            match self.program[pc] {
                Inst::Match => return true,
                Inst::Jump(to) => stack.push(to),
                Inst::Split(a, b) => {
                    stack.push(b);
                    stack.push(a);
                }
                Inst::Start if at == 0 => stack.push(pc + 1),
                Inst::End if at == len => stack.push(pc + 1),
                _ => {}
            }
        }
        false
    }
}

/// A set of program counters, in insertion order.
struct Threads {
    dense: Vec<usize>,
    member: Vec<bool>,
}

impl Threads {
    fn new(size: usize) -> Self {
        Self {
            dense: Vec::new(),
            member: alloc::vec![false; size],
        }
    }

    fn len(&self) -> usize {
        self.dense.len()
    }

    fn insert(&mut self, pc: usize) -> bool {
        if self.member[pc] {
            return false;
        }
        self.member[pc] = true;
        self.dense.push(pc);
        true
    }

    fn clear(&mut self) {
        for pc in self.dense.drain(..) {
            self.member[pc] = false;
        }
    }
}

/// An interval `m}`, `m,}` or `m,n}` after `{` at `at`: its bounds and the
/// position after it.
fn interval(chars: &[char], at: usize) -> Option<(usize, Option<usize>, usize)> {
    let number = |mut i: usize| -> Option<(usize, usize)> {
        let start = i;
        let mut value: usize = 0;
        while let Some(digit) = chars.get(i).and_then(|c| c.to_digit(10)) {
            value = value.saturating_mul(10).saturating_add(digit as usize);
            i += 1;
        }
        (i > start).then_some((value, i))
    };
    let (min, mut i) = number(at)?;
    let max = if chars.get(i) == Some(&',') {
        i += 1;
        match number(i) {
            Some((max, next)) => {
                i = next;
                Some(max)
            }
            None => None,
        }
    } else {
        Some(min)
    };
    (chars.get(i) == Some(&'}')).then_some((min, max, i + 1))
}

/// A bracket expression after `[` at `at`: its instruction and the
/// position after its `]`; why not, if it is not one this supports.
fn bracket(chars: &[char], mut at: usize) -> Result<(Inst, usize), &'static str> {
    let negated = chars.get(at) == Some(&'^');
    if negated {
        at += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let c = *chars.get(at).ok_or("invalid: an unclosed `[`")?;
        at += 1;
        if c == ']' && !first {
            break;
        }
        first = false;
        if c == '[' {
            match chars.get(at) {
                Some(':') => {
                    let end = (at + 1..chars.len().saturating_sub(1))
                        .find(|&j| chars[j] == ':' && chars[j + 1] == ']')
                        .ok_or("invalid: an unclosed `[:`")?;
                    let name: String = chars[at + 1..end].iter().collect();
                    let class =
                        NamedClass::from_name(&name).ok_or("invalid: an unknown `[:class:]`")?;
                    items.push(ClassItem::Named(class));
                    at = end + 2;
                    continue;
                }
                Some('.') => return Err("unsupported: a collating symbol `[.x.]`"),
                Some('=') => return Err("unsupported: an equivalence class `[=x=]`"),
                _ => {}
            }
        }
        if chars.get(at) == Some(&'-') && chars.get(at + 1).is_some_and(|c| *c != ']') {
            let hi = chars[at + 1];
            if hi < c || hi == '[' {
                return Err("invalid: a range out of order");
            }
            items.push(ClassItem::Range(c, hi));
            at += 2;
        } else {
            items.push(ClassItem::Range(c, c));
        }
    }
    Ok((Inst::Class { negated, items }, at))
}

#[cfg(test)]
mod tests {
    use alloc::{format, string::String};

    use super::{MAX_PROGRAM, SelectionGlob};

    fn found(glob: &str, text: &str) -> bool {
        SelectionGlob::compile(glob)
            .unwrap_or_else(|e| panic!("{glob}: {e}"))
            .is_found_in(text)
    }

    /// An atom repeated zero times is authored, so its branch or group is
    /// not empty, and it compiles to nothing. Checked against OpenUSD
    /// 26.08, through the `variant` predicate.
    #[test]
    fn atoms_repeated_zero_times_match_the_empty_string() {
        for glob in [
            "a{0}", "(a{0})", "a{0}|b", "a{0,0}", "(a|b){0}", "(a{0})*", "$a{0}",
        ] {
            assert!(found(glob, "") && found(glob, "red"), "{glob:?}");
        }
        assert!(found("^a{0}$", "") && !found("^a{0}$", "red"));
        assert!(!found("x(a{0})y", "red") && found("x(a{0})y", "xy"));
        assert!(found("b{0}lue", "blue") && !found("r(e{0})d", "red"));
        assert!(found("^(a{0})$", "") && !found("^(a{0})$", "a"));
        // `a{0}*` is `a{0}.*` once `*` is a glob's; a second repetition of
        // `a{0}` is undefined, and rejected.
        assert!(found("a{0}*", "red"));
        assert!(SelectionGlob::compile("a{0}{2}").is_err());
        assert!(
            SelectionGlob::compile("(a{0}|)").is_err(),
            "an empty branch"
        );
    }

    /// Each checked against `ArchRegex(glob, ArchRegex::GLOB)` in OpenUSD
    /// 26.08, through the `variant` predicate.
    #[test]
    fn globs_search_as_arch_regex_does() {
        assert!(found("b*", "blue") && found("l", "blue") && found("l|z", "blue"));
        assert!(!found("^b.*e$", "blue"), "`.` is literal");
        assert!(found("^b*e$", "blue"));
        assert!(found("1", "big1") && found("u?", "blue") && !found("x", "blue"));
        assert!(
            found("^(bl|gl)[a-z]e$", "blue") && found("[a-c]", "blue") && found("[^b]", "blue")
        );
        assert!(found("^[[:alpha:]]+$", "blue") && !found("^[[:digit:]]+$", "blue"));
        assert!(found("l{1}", "blue") && found("^(b|l){2}", "blue") && !found("^b{2}", "blue"));
        assert!(found("*", "") && found("a.b", "a.b") && !found("a.b", "axb"));
        assert!(found("(a|aa)+b", "aab") && !found("(a|aa)+b", "aa"));
        assert!(found("[]a]", "]") && found("[^]a]", "b") && !found("[^]a]", "a"));
        assert!(found("a{2,}", "xaaa") && !found("^a{2,3}$", "aaaa"));
    }

    /// Invalid expressions, syntax POSIX leaves undefined, and unsupported
    /// POSIX features are rejected, each saying which.
    #[test]
    fn undefined_and_invalid_expressions_are_rejected() {
        for bad in [
            "",
            "(",
            "a)",
            "+a",
            "a|",
            "|a",
            "(|a)",
            "()",
            "()*",
            "a{2,1}",
            "\\",
            "\\a",
            "(a)\\1",
            "a{",
            "a{x}",
            "{",
            "a+{2}",
            "a*+",
            "^+",
            "$+",
            "[a",
            "[[=a=]]",
            "[[:bogus:]]",
            "[z-a]",
            "a{256}",
            "(*){1000000}",
            "a{0,256}",
        ] {
            assert!(SelectionGlob::compile(bad).is_err(), "{bad:?} compiles");
        }
        for (glob, kind) in [
            ("[[=a=]]", "unsupported:"),
            ("((a{255}){255}){255}", "unsupported:"),
            ("()", "undefined:"),
            ("\\a", "undefined:"),
            ("a{256}", "invalid:"),
            ("[[:bogus:]]", "invalid:"),
        ] {
            let error = SelectionGlob::compile(glob).expect_err(glob);
            assert!(error.starts_with(kind), "{glob:?}: {error}");
        }
    }

    /// Pathological expressions compile to bounded programs or are
    /// rejected, and match in time bounded by program size times input
    /// length, without recursion.
    #[test]
    fn pathological_expressions_stay_bounded() {
        // Deep nesting: no recursion while parsing or matching.
        let deep = format!("{}a{}", "(".repeat(20_000), ")".repeat(20_000));
        assert!(found(&deep, "xa"));
        // Nested and zero-width repetitions terminate.
        for glob in [
            "(a*)*b",
            "(a|a)*b",
            "((a+)+)+b",
            "(a*)+$",
            "(^)+a",
            "((a{0,5}){0,5}){0,5}b",
        ] {
            let long: String = "a".repeat(10_000);
            let _ = SelectionGlob::compile(glob).map(|g| g.is_found_in(&long));
        }
        assert!(!found("(a*)*b", &"a".repeat(10_000)));
        assert!(found("(a|aa)+b", &format!("{}b", "a".repeat(10_000))));
        // Counts at the limit expand; beyond the size budget they are
        // rejected rather than allocated.
        assert!(
            found("(a{255}){3}", &"a".repeat(765)) && !found("^(a{255}){3}$", &"a".repeat(764))
        );
        assert!(
            !found("(a{255}){255}", "aaa"),
            "compiles to a bounded program"
        );
        assert!(SelectionGlob::compile("((a{255}){255}){255}").is_err());
        let wide = "(ab)".repeat(MAX_PROGRAM);
        assert!(SelectionGlob::compile(&wide).is_err());
    }
}
