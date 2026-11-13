//! `sqlite3`, and `aiosqlite` for coroutines, on SQLite: each statement runs
//! on a cursor of the connection, of tuples whatever the connection's row
//! factory, with a bare `?` per value: `?N` would not mix with a `sqlc.slice`'s
//! items (`docs/python-output.md` §6).

use super::dbapi::{Api, Driver};

pub const DRIVER: Driver = Driver {
    sync: Api {
        connection: "sqlite3.Connection",
        closing: "contextlib.closing(conn.cursor())",
        cursor: "conn.cursor()",
        cursor_type: "sqlite3.Cursor",
        setup: Some("cur.row_factory = None"),
    },
    asynchronous: Some(Api {
        connection: "aiosqlite.Connection",
        closing: "conn.cursor()",
        cursor: "await conn.cursor()",
        cursor_type: "aiosqlite.Cursor",
        setup: Some("cur.row_factory = None"),
    }),
    placeholder: Some("?"),
    formats: false,
    execute_counts: false,
    pipelines: false,
    no_rows: "SELECT NULL WHERE 0",
    bind: |_, value| value,
    text_arrays: false,
    copies: false,
};
