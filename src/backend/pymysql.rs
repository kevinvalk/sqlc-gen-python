//! PyMySQL, on MySQL: each statement runs on a cursor of tuples, whatever the
//! connection's cursor class. PyMySQL formats the statement with `%`, so each
//! placeholder is `%s` and every other `%` is doubled
//! (`docs/python-output.md` §6).
//!
//! It has no coroutines: no async driver for MySQL is both typed and right.
//! asyncmy's stubs leave `execute` untyped, aiomysql has none (and cannot
//! bind `bytes` with PyMySQL 1.2), and
//! mysql-connector's prepared cursors read a negative `TIME` as positive (and
//! its C extension an unsigned `BIGINT`'s largest value as `-1`).

use ruff_python_ast as ast;

use super::dbapi::{Driver, Flavor, Placeholders};
use crate::python::Expression;
use crate::python::vocabulary::{CONN, PYMYSQL};

pub const DRIVER: Driver = Driver {
    sync: Flavor {
        // Generic only in its stubs, over a cursor class defaulting to the
        // plain one.
        connection: || PYMYSQL.attr("Connection"),
        closing: cursor,
        cursor,
        cursor_type,
        setup: None,
    },
    asynchronous: None,
    placeholders: Some(Placeholders {
        text: "%s",
        formats: true,
        no_rows: "SELECT NULL FROM DUAL WHERE FALSE",
    }),
    execute_counts: true,
    pipelines: false,
    bind: |_, value| value,
    text_arrays: false,
    copies: false,
};

/// `conn.cursor(pymysql.cursors.Cursor)`: a cursor of tuples, whatever the
/// connection's cursor class.
fn cursor() -> ast::Expr {
    CONN.attr("cursor").call([cursor_type()])
}

/// `pymysql.cursors.Cursor`.
fn cursor_type() -> ast::Expr {
    PYMYSQL.attr("cursors").attr("Cursor")
}
