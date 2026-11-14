//! Writing Python.
//!
//! Three things live here, because they are one concern seen at three levels:
//! the [`syntax`] a generated file is built from, the [`types`] it refers to,
//! and the [`imports`] that make those types resolve. Imports are never
//! declared: [`render()`] reads them off the names its body reads, so a name
//! and the import that gives it meaning cannot drift apart.
//!
//! Most of a generated file is fixed Python with data slotted into a few
//! places. It is written as Python templates ([`py_expr!`], [`py_stmt!`],
//! [`py_stmts!`], see [`quote`]) whose holes take the data as nodes, built by
//! the constructors below: data never passes through the parser, so a column
//! called `class`, or SQL holding `"`, is a name or a string and nothing else.
//! [`expr`] and [`stmt`] parse a fragment this crate writes whole.
//!
//! [`render()`] closes the loop: ruff unparses the tree, then ruff formats it.

mod imports;
pub mod quote;
mod render;
mod syntax;
mod types;

pub use imports::is_imported;
pub use quote::Hole;
pub use render::{render, trips_s105};
pub use syntax::*;
pub use types::{Json, Scope, Type, annotation};

/// An expression from a template ([`quote`]): `py_expr!("$a + 1", a = x)`.
macro_rules! py_expr {
    ($template:expr $(, $hole:ident = $value:expr)* $(,)?) => {
        $crate::python::quote::expr(
            $template,
            vec![$((stringify!($hole), $crate::python::Hole::from($value))),*],
        )
    };
}

/// The one statement of a template ([`quote`]).
macro_rules! py_stmt {
    ($template:expr $(, $hole:ident = $value:expr)* $(,)?) => {{
        let mut stmts = $crate::python::py_stmts!($template $(, $hole = $value)*);
        assert_eq!(stmts.len(), 1, "template `{}` is one statement", $template);
        stmts.remove(0)
    }};
}

/// The statements of a template ([`quote`]).
macro_rules! py_stmts {
    ($template:expr $(, $hole:ident = $value:expr)* $(,)?) => {
        $crate::python::quote::stmts(
            $template,
            vec![$((stringify!($hole), $crate::python::Hole::from($value))),*],
        )
    };
}

pub(crate) use {py_expr, py_stmt, py_stmts};
