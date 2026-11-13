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
    copies: true,
};

/// psycopg's range classes, by the range's PostgreSQL type, with the type of
/// its bounds; and the types whose value psycopg returns as its text, where
/// asyncpg returns an object of its own (`tests/python/typemap.py`).
const RANGES: &[(&str, &str)] = &[
    ("int4range", "int"),
    ("int8range", "int"),
    ("numrange", "Decimal"),
    ("daterange", "dt.date"),
    ("tsrange", "dt.datetime"),
    ("tstzrange", "dt.datetime"),
];
const TEXT: &[&str] = &[
    "hstore", "bit", "varbit", "point", "line", "lseg", "box", "path", "polygon", "circle", "cid",
    "xid", "xid8", "tid", "pg_lsn",
];

/// The Python type psycopg returns for a column of `data_type`, of those it
/// alone decides.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = data_type.strip_prefix("pg_catalog.").unwrap_or(data_type);
    if let Some((_, bound)) = RANGES.iter().find(|(range, _)| *range == name) {
        return Some(Type::Library {
            annotation: format!("Range[{bound}]"),
            sql_type: None,
        });
    }
    TEXT.contains(&name).then_some(Type::STR)
}

/// `value`, wrapped in psycopg's JSON type for a JSON parameter, which psycopg
/// cannot tell from any other `dict` or `list`. `None` stays SQL `NULL`,
/// never the JSON `null`.
fn bind(param: &Field, value: &str) -> String {
    let json = match param.ty.element.json() {
        Some(Json::Json) => "Json",
        Some(Json::Jsonb) => "Jsonb",
        None => return value.into(),
    };
    format!("None if {value} is None else {json}({value})")
}
