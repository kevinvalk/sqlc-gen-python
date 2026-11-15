//! NOTE: We use asyncmy for coroutines as it is PyMySQL's protocol and conversions in Cython, so it returns and binds
//! every type as PyMySQL does (`tests/python/typemap_mysql.py`). The others do not: aiomysql cannot bind `bytes` with
//! PyMySQL 1.2, and mysql-connector reads a `BIT` as an `int`, leaves a doubled `%` doubled, and its prepared cursors
//! read a negative `TIME` as positive.

use ruff_python_ast as ast;

use super::dbapi::{Driver, Flavor, Placeholders};
use crate::python::Expression;
use crate::python::vocabulary::{ASYNCMY, CONNECTION, PYMYSQL, Symbol};

/// Both drivers format the statement with `%`, so each placeholder is `%s` and every other `%` is doubled
/// (`docs/python-output.md` §6).
pub const DRIVER: Driver = Driver {
    sync: Flavor {
        connection: || PYMYSQL.attr("Connection"),
        closing: || cursor(PYMYSQL),
        cursor: || cursor(PYMYSQL),
        cursor_type: || cursor_type(PYMYSQL),
        generic: true,
        untyped: false,
    },
    asynchronous: Flavor {
        connection: || ASYNCMY.attr("Connection"),
        closing: || cursor(ASYNCMY),
        cursor: || cursor(ASYNCMY),
        cursor_type: || cursor_type(ASYNCMY),
        generic: false,
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
    plain_rows: None,
};

/// `connection.cursor(<library>.cursors.Cursor)`, so we get tuples whatever the connection's cursor class.
fn cursor(library: Symbol) -> ast::Expr {
    CONNECTION.attr("cursor").call([cursor_type(library)])
}

fn cursor_type(library: Symbol) -> ast::Expr {
    library.attr("cursors").attr("Cursor")
}
