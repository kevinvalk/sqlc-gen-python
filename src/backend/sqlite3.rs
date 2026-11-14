//! `sqlite3`, and `aiosqlite` for coroutines, on SQLite: each statement runs
//! on a cursor of the connection, of tuples whatever the connection's row
//! factory, with a bare `?` per value: `?N` would not mix with a `sqlc.slice`'s
//! items (`docs/python-output.md` §6).

use ruff_python_ast as ast;

use super::dbapi::{Driver, Flavor, Placeholders};
use crate::python::vocabulary::{AIOSQLITE, CONN, CONTEXTLIB, CUR, SQLITE3};
use crate::python::{Expression, assign_to, none};

pub const DRIVER: Driver = Driver {
    sync: Flavor {
        connection: || SQLITE3.attr("Connection"),
        closing: || CONTEXTLIB.attr("closing").call([cursor()]),
        cursor,
        cursor_type: || SQLITE3.attr("Cursor"),
        setup: Some(plain_rows),
    },
    asynchronous: Some(Flavor {
        connection: || AIOSQLITE.attr("Connection"),
        closing: cursor,
        cursor: || cursor().awaited(true),
        cursor_type: || AIOSQLITE.attr("Cursor"),
        setup: Some(plain_rows),
    }),
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
};

/// `conn.cursor()`.
fn cursor() -> ast::Expr {
    CONN.attr("cursor").call([])
}

/// `cur.row_factory = None`: rows as tuples, whatever the connection's row
/// factory.
fn plain_rows() -> ast::Stmt {
    assign_to(CUR.attr("row_factory"), none())
}
