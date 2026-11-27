// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A validated namespace in emission order. Most prims only need a name,
//! a declaration and properties. Additional metadata stays with the prims
//! that author it. Array values retain their native storage until emission.

use super::borrowed::{BorrowedPrim, Declaration};
use super::{Document, Prim, Property, Specifier, WriteError, Writer};
use alloc::{boxed::Box, string::String, vec::Vec};
use core::fmt;

pub(super) struct TextBody<'a> {
    pub(super) name: &'a str,
    pub(super) type_name: Option<&'a str>,
    pub(super) specifier: Option<Specifier>,
    pub(super) fields: Option<&'a Prim>,
    pub(super) properties: &'a [Property],
}

impl<'a> From<&'a Prim> for TextBody<'a> {
    fn from(prim: &'a Prim) -> Self {
        Self {
            name: &prim.name,
            type_name: prim.type_name.as_deref(),
            specifier: prim.specifier,
            fields: Some(prim),
            properties: &prim.properties,
        }
    }
}

struct Body {
    name: String,
    type_name: Option<String>,
    specifier: Option<Specifier>,
    fields: Option<Box<Prim>>,
    properties: Vec<Property>,
}

impl Body {
    fn view(&self) -> TextBody<'_> {
        TextBody {
            name: &self.name,
            type_name: self.type_name.as_deref(),
            specifier: self.specifier,
            fields: self.fields.as_deref(),
            properties: &self.properties,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub(crate) depth: usize,
    pub(crate) variant: bool,
    pub(crate) blank_before: bool,
    pub(crate) children: bool,
}

enum Step {
    Body(Layout),
    Borrowed(Layout),
    Set(String, usize),
    Leave { depth: usize, variant: bool },
}

/// Destinations for checked text bodies. The private string can be patched
/// when the walk discovers a newer version. A forward-only sink needs the
/// checked sequence and its final version before it receives the header.
pub(crate) trait TextOutput<'a>: Default {
    type Finished;
    fn declarations(&mut self) -> &mut Vec<Declaration<'a>>;
    fn root(&mut self, root: Document);
    fn body(&mut self, prim: Prim, layout: Layout);
    fn borrowed(&mut self, prim: BorrowedPrim<'a>, layout: Layout);
    fn set(&mut self, name: &str, depth: usize);
    fn leave(&mut self, depth: usize, variant: bool);
    fn finish(self, minor: u8) -> Self::Finished;
}

/// String saves only need the current prim's declarations.
#[derive(Default)]
pub(crate) struct TextString<'a> {
    text: String,
    declarations: Vec<Declaration<'a>>,
}

impl<'a> TextOutput<'a> for TextString<'a> {
    type Finished = String;

    fn declarations(&mut self) -> &mut Vec<Declaration<'a>> {
        &mut self.declarations
    }

    fn borrowed(&mut self, prim: BorrowedPrim<'a>, layout: Layout) {
        prim.write(&mut Writer::new(&mut self.text), layout, &self.declarations);
        self.declarations.clear();
    }
    fn root(&mut self, root: Document) {
        Writer::new(&mut self.text).header(&root, 0);
    }
    fn body(&mut self, prim: Prim, layout: Layout) {
        Writer::new(&mut self.text).open_body((&prim).into(), layout);
    }
    fn set(&mut self, name: &str, depth: usize) {
        Writer::new(&mut self.text).open_set(name, depth);
    }
    fn leave(&mut self, depth: usize, variant: bool) {
        Writer::new(&mut self.text).close(depth, variant);
    }
    fn finish(mut self, minor: u8) -> String {
        if minor != 0 {
            let digit = super::HEADER_PREFIX.len();
            self.text.replace_range(
                digit..digit + 1,
                &alloc::string::ToString::to_string(&minor),
            );
        }
        self.text.push('\n');
        self.text
    }
}

/// A checked authored layer ready to stream as USDA. Its header version is
/// known before the first write. Property values are formatted at the sink.
pub struct PreparedUsda<'a> {
    declarations: Vec<Declaration<'a>>,
    borrowed: Vec<BorrowedPrim<'a>>,
    root: Document,
    minor: u8,
    steps: Vec<Step>,
    bodies: Vec<Body>,
}

impl fmt::Debug for PreparedUsda<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedUsda")
            .field("minor", &self.minor)
            .field("bodies", &(self.bodies.len() + self.borrowed.len()))
            .finish_non_exhaustive()
    }
}

impl Default for PreparedUsda<'_> {
    fn default() -> Self {
        Self {
            declarations: Vec::new(),
            borrowed: Vec::new(),
            root: Document::new(),
            minor: 0,
            steps: Vec::new(),
            bodies: Vec::new(),
        }
    }
}

impl<'a> TextOutput<'a> for PreparedUsda<'a> {
    fn declarations(&mut self) -> &mut Vec<Declaration<'a>> {
        &mut self.declarations
    }

    fn borrowed(&mut self, prim: BorrowedPrim<'a>, layout: Layout) {
        self.steps.push(Step::Borrowed(layout));
        self.borrowed.push(prim);
    }
    type Finished = Self;
    fn root(&mut self, root: Document) {
        self.root = root;
    }
    fn finish(mut self, minor: u8) -> Self {
        self.minor = minor;
        self
    }
    fn body(&mut self, mut prim: Prim, layout: Layout) {
        self.steps.push(Step::Body(layout));
        self.bodies.push(Body {
            name: core::mem::take(&mut prim.name),
            type_name: prim.type_name.take(),
            specifier: prim.specifier,
            properties: core::mem::take(&mut prim.properties),
            fields: (prim.has_metadata()
                || prim.property_order.is_some()
                || prim.prim_order.is_some())
            .then(|| Box::new(prim)),
        });
    }
    fn set(&mut self, name: &str, depth: usize) {
        self.steps.push(Step::Set(name.into(), depth));
    }
    fn leave(&mut self, depth: usize, variant: bool) {
        self.steps.push(Step::Leave { depth, variant });
    }
}

impl PreparedUsda<'_> {
    fn emit(&self, writer: &mut Writer<'_>) {
        writer.header(&self.root, self.minor);
        let mut bodies = self.bodies.iter();
        let mut borrowed = self.borrowed.iter();
        for step in &self.steps {
            match step {
                Step::Borrowed(layout) => borrowed
                    .next()
                    .expect("one borrowed body per spec")
                    .write(writer, *layout, &self.declarations),
                Step::Body(layout) => {
                    writer.open_body(bodies.next().expect("one body per spec").view(), *layout);
                }
                Step::Set(name, depth) => writer.open_set(name, *depth),
                Step::Leave { depth, variant } => writer.close(*depth, *variant),
            }
        }
        writer.out.push('\n');
    }

    /// Writes the prepared USDA in pieces of about 64 KiB.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError::Sink`] if the sink refuses a piece.
    pub fn write_to<W: fmt::Write + ?Sized>(&self, sink: &mut W) -> Result<(), WriteError> {
        Writer::stream(sink, |writer| self.emit(writer))
    }
}
