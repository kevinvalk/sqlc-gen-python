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

use super::dbapi::{Api, Driver};

pub const DRIVER: Driver = Driver {
    sync: Api {
        // Generic only in its stubs, over a cursor class defaulting to the
        // plain one.
        connection: "pymysql.Connection",
        closing: "conn.cursor(pymysql.cursors.Cursor)",
        cursor: "conn.cursor(pymysql.cursors.Cursor)",
        cursor_type: "pymysql.cursors.Cursor",
        setup: None,
    },
    asynchronous: None,
    placeholder: Some("%s"),
    formats: true,
    execute_counts: true,
    pipelines: false,
    no_rows: "SELECT NULL FROM DUAL WHERE FALSE",
    bind: |_, value| value.into(),
    text_arrays: false,
    copies: false,
};
