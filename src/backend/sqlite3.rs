use ruff_python_ast as ast;

use super::dbapi::{Driver, Flavor, Placeholders};
use crate::python::vocabulary::{AIOSQLITE, CONNECTION, CONTEXTLIB, CUR, SQLITE3};
use crate::python::{Expression, assign_to, none};

/// We write a bare `?` per value, as `?N` would not mix with a `sqlc.slice`'s items (`docs/python-output.md` §6).
pub const DRIVER: Driver = Driver {
    sync: Flavor {
        connection: || SQLITE3.attr("Connection"),
        closing: || CONTEXTLIB.attr("closing").call([cursor()]),
        cursor,
        cursor_type: || SQLITE3.attr("Cursor"),
        generic: false,
        untyped: false,
    },
    asynchronous: Flavor {
        connection: || AIOSQLITE.attr("Connection"),
        closing: cursor,
        cursor: || cursor().awaited(true),
        cursor_type: || AIOSQLITE.attr("Cursor"),
        generic: false,
        untyped: false,
    },
    placeholders: Some(Placeholders {
        text: "?",
        formats: false,
        no_rows: "SELECT NULL WHERE 0",
    }),
    execute_counts: false,
    pipelines: false,
    bind: |_, value| value,
    text_arrays: false,
    copies: false,
    plain_rows: Some(plain_rows),
};

fn cursor() -> ast::Expr {
    CONNECTION.attr("cursor").call([])
}

/// Rows as tuples, whatever the connection's row factory.
fn plain_rows() -> ast::Stmt {
    assign_to(CUR.attr("row_factory"), none())
}
