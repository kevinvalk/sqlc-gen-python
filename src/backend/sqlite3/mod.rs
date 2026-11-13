//! `sqlite3`, and `aiosqlite` for the async API, on SQLite: each statement
//! runs on the connection, which takes SQLite's own `?` placeholders
//! (`docs/python-output.md` §6).

use super::dbapi::Driver;

pub const DRIVER: Driver = Driver {
    connection: ["sqlite3.Connection", "aiosqlite.Connection"],
    cursor: ["conn", "conn"],
    cursor_type: ["sqlite3.Cursor", "aiosqlite.Cursor"],
    bind: |_, value| value.into(),
    text_arrays: false,
};
