//! psycopg 3, on PostgreSQL: each statement runs on a raw cursor, which takes
//! PostgreSQL's own `$N` placeholders (`docs/python-output.md` §6).

use super::dbapi::{Api, Driver};
use crate::api::models::Field;
use crate::python::{Json, Type, py_expr};
use ruff_python_ast as ast;

/// A raw cursor of tuples, whatever the connection's row factory.
pub const DRIVER: Driver = Driver {
    sync: Api {
        connection: "psycopg.Connection[Any]",
        closing: "psycopg.RawCursor(conn, row_factory=tuple_row)",
        cursor: "psycopg.RawCursor(conn, row_factory=tuple_row)",
        cursor_type: "psycopg.RawCursor[tuple[Any, ...]]",
        setup: None,
    },
    asynchronous: Some(Api {
        connection: "psycopg.AsyncConnection[Any]",
        closing: "psycopg.AsyncRawCursor(conn, row_factory=tuple_row)",
        cursor: "psycopg.AsyncRawCursor(conn, row_factory=tuple_row)",
        cursor_type: "psycopg.AsyncRawCursor[tuple[Any, ...]]",
        setup: None,
    }),
    placeholder: None,
    formats: false,
    execute_counts: false,
    pipelines: true,
    // PostgreSQL has no `sqlc.slice`.
    no_rows: "",
    bind,
    // psycopg knows no type the schema declares.
    text_arrays: true,
    copies: true,
};

/// The types whose value psycopg returns as its text, where asyncpg returns
/// an object of its own (`tests/python/typemap.py`).
const TEXT: &[&str] = &[
    "hstore", "bit", "varbit", "point", "line", "lseg", "box", "path", "polygon", "circle", "cid",
    "xid", "xid8", "tid", "pg_lsn",
];

/// The Python type psycopg returns for a column of `data_type`, of those it
/// alone decides.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = data_type.strip_prefix("pg_catalog.").unwrap_or(data_type);
    if let Some((bound, _)) = super::range(name) {
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
fn bind(param: &Field, value: ast::Expr) -> ast::Expr {
    let json = match param.ty.element.json() {
        Some(Json::Json) => "Json",
        Some(Json::Jsonb) => "Jsonb",
        None => return value,
    };
    py_expr!(
        "None if $value is None else $json($value)",
        value = value,
        json = json,
    )
}
