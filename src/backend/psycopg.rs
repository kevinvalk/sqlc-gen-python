//! psycopg 3, on PostgreSQL: each statement runs on a raw cursor, which takes
//! PostgreSQL's own `$N` placeholders (`docs/python-output.md` §6).

use ruff_python_ast as ast;

use super::dbapi::{Driver, Flavor};
use crate::api::models::Field;
use crate::python::vocabulary::{ANY, CONN, JSON, JSONB, PSYCOPG, RANGE, TUPLE, TUPLE_ROW};
use crate::python::{Expression, Json, Type, ellipsis, keyword, unless_none};

/// A raw cursor of tuples, whatever the connection's row factory.
pub const DRIVER: Driver = Driver {
    sync: Flavor {
        connection: || PSYCOPG.attr("Connection").index(ANY),
        closing: || cursor("RawCursor"),
        cursor: || cursor("RawCursor"),
        cursor_type: || cursor_type("RawCursor"),
        setup: None,
    },
    asynchronous: Some(Flavor {
        connection: || PSYCOPG.attr("AsyncConnection").index(ANY),
        closing: || cursor("AsyncRawCursor"),
        cursor: || cursor("AsyncRawCursor"),
        cursor_type: || cursor_type("AsyncRawCursor"),
        setup: None,
    }),
    // PostgreSQL's own `$N`, as sqlc writes them.
    placeholders: None,
    execute_counts: false,
    pipelines: true,
    bind,
    // psycopg knows no type the schema declares.
    text_arrays: true,
    copies: true,
};

/// `psycopg.<class>(conn, row_factory=tuple_row)`: a cursor of `class` on
/// `conn`.
fn cursor(class: &str) -> ast::Expr {
    (PSYCOPG.attr(class)).call_with([CONN.into()], [keyword("row_factory", TUPLE_ROW)])
}

/// `psycopg.<class>[tuple[Any, ...]]`: what such a cursor is.
fn cursor_type(class: &str) -> ast::Expr {
    let row = TUPLE.index_each(vec![ANY.into(), ellipsis()]);
    PSYCOPG.attr(class).index(row)
}

/// The types whose value psycopg returns as its text, where asyncpg returns
/// an object of its own (`tests/python/typemap.py`).
const TEXT: &[&str] = &[
    "hstore", "bit", "varbit", "point", "line", "lseg", "box", "path", "polygon", "circle", "cid",
    "xid", "xid8", "tid", "pg_lsn",
];

/// The Python type psycopg returns for a column of PostgreSQL's `pg_type`,
/// of those it alone decides.
pub fn scalar_type(pg_type: &str) -> Option<Type> {
    if let Some((bound, _)) = super::range(pg_type) {
        return Some(Type::Library {
            annotation: RANGE.index(bound),
            sql_type: None,
        });
    }
    TEXT.contains(&pg_type).then_some(Type::STR)
}

/// `value`, wrapped in psycopg's JSON type for a JSON parameter, which psycopg
/// cannot tell from any other `dict` or `list`. `None` stays SQL `NULL`,
/// never the JSON `null`.
fn bind(param: &Field, value: ast::Expr) -> ast::Expr {
    let json = match param.ty.element.json() {
        Some(Json::Json) => JSON,
        Some(Json::Jsonb) => JSONB,
        None => return value,
    };
    unless_none(value.clone(), json.call([value]))
}
