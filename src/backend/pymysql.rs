//! PyMySQL, and asyncmy for coroutines, on MySQL: each statement runs on a
//! cursor of tuples, whatever the connection's cursor class. Either formats
//! the statement with `%`, so each placeholder is `%s` and every other `%` is
//! doubled (`docs/python-output.md` §6).
//!
//! asyncmy is PyMySQL's protocol and conversions in Cython, and returns and
//! binds every type as PyMySQL does (`tests/python/typemap_mysql.py`). The
//! others do not: aiomysql cannot bind `bytes` with PyMySQL 1.2, and
//! mysql-connector reads a `BIT` as an `int`, leaves a doubled `%` doubled,
//! and its prepared cursors read a negative `TIME` as positive.

use ruff_python_ast as ast;

use super::dbapi::{Driver, Flavor, Placeholders};
use crate::python::Expression;
use crate::python::vocabulary::{ASYNCMY, CONN, PYMYSQL, Symbol};

pub const DRIVER: Driver = Driver {
    sync: Flavor {
        // Generic only in its stubs, over a cursor class defaulting to the
        // plain one.
        connection: || PYMYSQL.attr("Connection"),
        closing: || cursor(PYMYSQL),
        cursor: || cursor(PYMYSQL),
        cursor_type: || cursor_type(PYMYSQL),
        setup: None,
        untyped: false,
    },
    asynchronous: Flavor {
        connection: || ASYNCMY.attr("Connection"),
        closing: || cursor(ASYNCMY),
        cursor: || cursor(ASYNCMY),
        cursor_type: || cursor_type(ASYNCMY),
        setup: None,
        untyped: true,
    },
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

/// `conn.cursor(<library>.cursors.Cursor)`: a cursor of tuples, whatever the
/// connection's cursor class.
fn cursor(library: Symbol) -> ast::Expr {
    CONN.attr("cursor").call([cursor_type(library)])
}

/// `<library>.cursors.Cursor`.
fn cursor_type(library: Symbol) -> ast::Expr {
    library.attr("cursors").attr("Cursor")
}
