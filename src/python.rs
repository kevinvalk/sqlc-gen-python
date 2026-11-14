//! Writing Python.
//!
//! Four things live here, because they are one concern seen at four levels:
//! the [`syntax`] a generated file is built from, the names of its
//! [`vocabulary`], the [`types`] it refers to, and the [`imports`] that make
//! them resolve. Imports are never declared: [`render()`] reads them off the
//! names its body reads, so a name and the import that gives it meaning
//! cannot drift apart.
//!
//! A generated file is a tree from the first node to the last: [`syntax`]
//! builds each expression and statement as ruff's own node, so nothing this
//! crate writes passes through a parser, and data (a column called `class`,
//! SQL holding `"`) is a name or a string and nothing else. The one Python
//! this crate reads is an override's annotation ([`Annotation`]), which is
//! the user's.
//!
//! [`render()`] closes the loop: ruff unparses the tree, then ruff formats it.

mod imports;
mod render;
mod syntax;
mod types;
pub mod vocabulary;

pub use render::{render, trips_s105};
pub use syntax::*;
pub use types::{Annotation, Json, Scope, Type};
pub use vocabulary::is_imported;
