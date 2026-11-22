use ruff_python_ast as ast;

use super::dbapi::{Driver, Flavor};
use crate::api::models::Field;
use crate::python::vocabulary::{ANY, CONNECTION, JSON, JSONB, PSYCOPG, RANGE, TUPLE, TUPLE_ROW};
use crate::python::{Expression, Json, Type, ellipsis, keyword, string};

/// We run each statement on a raw cursor, as it takes PostgreSQL's own `$N` placeholders as sqlc writes them, and ask
/// for tuples whatever the connection's row factory.
pub const DRIVER: Driver = Driver {
    sync: Flavor {
        connection: || PSYCOPG.attr("Connection").index(ANY),
        closing: || cursor("RawCursor"),
        cursor: || cursor("RawCursor"),
        streaming: |name| server_cursor("RawServerCursor", name),
        cursor_type: || cursor_type("RawCursor"),
        generic: false,
        untyped: false,
    },
    asynchronous: Flavor {
        connection: || PSYCOPG.attr("AsyncConnection").index(ANY),
        closing: || cursor("AsyncRawCursor"),
        cursor: || cursor("AsyncRawCursor"),
        streaming: |name| server_cursor("AsyncRawServerCursor", name),
        cursor_type: || cursor_type("AsyncRawCursor"),
        generic: false,
        untyped: false,
    },
    placeholders: None,
    execute_counts: false,
    pipelines: true,
    bind,
    text_arrays: true, // psycopg knows no type the schema declares
    copies: true,
    plain_rows: None,
};

/// `psycopg.<class>(connection, row_factory=tuple_row)`
fn cursor(class: &str) -> ast::Expr {
    (PSYCOPG.attr(class)).call_with([CONNECTION.into()], [keyword("row_factory", TUPLE_ROW)])
}

/// `psycopg.<class>(connection, "<name>", row_factory=tuple_row)`, a cursor PostgreSQL holds, which psycopg fetches
/// from a page at a time. We name it after its stream, so one is open at a time per connection. Its `stream()` would
/// need no transaction, but holds the connection's lock between rows, so one left before its end blocks the connection
/// until it is garbage, and its stubs type it as an iterator that cannot be closed sooner.
fn server_cursor(class: &str, name: &str) -> ast::Expr {
    (PSYCOPG.attr(class)).call_with([CONNECTION.into(), string(name)], [keyword("row_factory", TUPLE_ROW)])
}

/// `psycopg.<class>[tuple[Any, ...]]`
fn cursor_type(class: &str) -> ast::Expr {
    let row = TUPLE.index_each(vec![ANY.into(), ellipsis()]);
    PSYCOPG.attr(class).index(row)
}

/// psycopg returns these as their text where asyncpg returns an object of its own (`tests/python/typemap_psycopg.py`).
pub(super) const TEXT: &[&str] = &[
    "hstore", "bit", "varbit", "point", "line", "lseg", "box", "path", "polygon", "circle", "cid", "xid", "xid8",
    "tid", "pg_lsn",
];

pub fn scalar_type(pg_type: &str) -> Option<Type> {
    if let Some((bound, _)) = super::range(pg_type) {
        return Some(Type::Library {
            annotation: RANGE.index(bound),
            sql_type: None,
        });
    }
    TEXT.contains(&pg_type).then_some(Type::STR)
}

/// We wrap a JSON parameter in psycopg's JSON type, as psycopg cannot tell it from any other `dict` or `list`. The
/// `None` of a nullable parameter stays SQL `NULL`. For a required scalar we wrap the whole document, including JSON
/// `null`.
fn bind(parameter: &Field, value: ast::Expr) -> ast::Expr {
    let wrapper = parameter.ty.element.json().map(|json| match json {
        Json::Json => JSON,
        Json::Jsonb => JSONB,
    });
    parameter.ty.bind(value, wrapper)
}
