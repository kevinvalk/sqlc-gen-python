use ruff_python_ast as ast;

use super::dbapi::{Driver, Flavor};
use crate::api::models::Field;
use crate::python::vocabulary::{ANY, CONNECTION, JSON, JSONB, PSYCOPG, RANGE, TUPLE, TUPLE_ROW};
use crate::python::{Expression, Json, Type, ellipsis, keyword, unless_none};

/// We run each statement on a raw cursor, as it takes PostgreSQL's own `$N` placeholders as sqlc writes them, and ask
/// for tuples whatever the connection's row factory.
pub const DRIVER: Driver = Driver {
    sync: Flavor {
        connection: || PSYCOPG.attr("Connection").index(ANY),
        closing: || cursor("RawCursor"),
        cursor: || cursor("RawCursor"),
        cursor_type: || cursor_type("RawCursor"),
        generic: false,
        untyped: false,
    },
    asynchronous: Flavor {
        connection: || PSYCOPG.attr("AsyncConnection").index(ANY),
        closing: || cursor("AsyncRawCursor"),
        cursor: || cursor("AsyncRawCursor"),
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

/// We wrap a JSON parameter in psycopg's JSON type, as psycopg cannot tell it from any other `dict` or `list`. `None`
/// stays SQL `NULL` and never becomes the JSON `null`. Only a nullable parameter or an override's type admits it, as a
/// JSON document never does.
fn bind(parameter: &Field, value: ast::Expr) -> ast::Expr {
    let json = match parameter.ty.element.json() {
        Some(Json::Json) => JSON,
        Some(Json::Jsonb) => JSONB,
        None => return value,
    };
    match parameter.ty.optional || !matches!(parameter.ty.element, Type::Json(_)) {
        true => unless_none(value.clone(), json.call([value])),
        false => json.call([value]),
    }
}
