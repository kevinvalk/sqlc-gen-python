//! psycopg 3, on PostgreSQL: each statement runs on a raw cursor, which takes
//! PostgreSQL's own `$N` placeholders (`docs/python-output.md` §6).

use super::dbapi::Driver;
use crate::api::models::Field;
use crate::python::{Json, Type};

/// A raw cursor of tuples, whatever the connection's row factory.
pub const DRIVER: Driver = Driver {
    connection: ["psycopg.Connection[Any]", "psycopg.AsyncConnection[Any]"],
    cursor: [
        "psycopg.RawCursor(conn, row_factory=tuple_row)",
        "psycopg.AsyncRawCursor(conn, row_factory=tuple_row)",
    ],
    cursor_type: [
        "psycopg.RawCursor[tuple[Any, ...]]",
        "psycopg.AsyncRawCursor[tuple[Any, ...]]",
    ],
    placeholder: None,
    execute_counts: false,
    // PostgreSQL has no `sqlc.slice`.
    no_rows: "",
    bind,
    // psycopg knows no type the schema declares.
    text_arrays: true,
};

/// `value`, wrapped in psycopg's JSON type for a JSON parameter, which psycopg
/// cannot tell from any other `dict` or `list`. `None` stays SQL `NULL`,
/// never the JSON `null`.
fn bind(param: &Field, value: &str) -> String {
    let json = match param.ty.element {
        Type::Json(Json::Json) => "Json",
        Type::Json(Json::Jsonb) => "Jsonb",
        _ => return value.into(),
    };
    format!("None if {value} is None else {json}({value})")
}
