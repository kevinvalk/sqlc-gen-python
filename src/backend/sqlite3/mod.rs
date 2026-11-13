//! `sqlite3`, and `aiosqlite` for coroutines, on SQLite: each statement runs
//! on a cursor of the connection, which takes SQLite's own `?` placeholders
//! (`docs/python-output.md` §6).

use super::dbapi::Driver;

pub const DRIVER: Driver = Driver {
    connection: ["sqlite3.Connection", "aiosqlite.Connection"],
    cursor: ["conn.cursor()", "await conn.cursor()"],
    cursor_type: ["sqlite3.Cursor", "aiosqlite.Cursor"],
    placeholder: None,
    execute_counts: false,
    no_rows: "SELECT NULL WHERE 0",
    bind: |_, value| value.into(),
    text_arrays: false,
    copies: false,
};
