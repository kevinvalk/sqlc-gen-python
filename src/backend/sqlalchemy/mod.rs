pub mod text;

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use crate::api::models::{Field, FieldType};
use crate::api::queries::{Command, Query, Shape};
use crate::engine::Engine;
use crate::python::vocabulary::{
    ANY, ASYNC_CONNECTION, CONNECTION, DICT, LEN, LIST, MODELS, PARAMS, POSTGRESQL, R, RESULT, ROW, ROWS, SQLALCHEMY,
    STR, VALUE, VALUES,
};
use crate::python::{
    Expression, Json, Scope, Type, ann_assign, assign, dict, each, expression, for_, generator_of, if_, int, keyword,
    list, list_of, name, return_, starred, stmts, string, true_, unless_none, with, yield_,
};

pub fn check(engine: Engine, query: &Query) -> Result<()> {
    text::check(&query.sql)?;

    // SQLAlchemy reads asyncpg's row count off the command tag, with an expression that knows no `MERGE n`. So a count
    // of a MERGE would be -1 on asyncpg and the real count on psycopg!
    let command = match query.command {
        Command::ExecRows => ":execrows",
        Command::ExecResult => ":execresult",
        _ => return Ok(()),
    };
    if engine.merges(&query.sql.text()) {
        bail!(
            "{command} counts -1 rows for a MERGE on asyncpg, whose row count SQLAlchemy cannot read, \
             and the real count on psycopg; use :exec"
        );
    }
    Ok(())
}

/// `sqlalchemy.text(...)`, typed only where a driver needs it: a
/// JSON parameter binds through SQLAlchemy's JSON type, a `sqlc.slice` expands to a placeholder per item, and a column
/// of enum arrays loads through an `ARRAY` of the enum, as psycopg would return the array's text.
///
/// We type the columns by position, every one of them, as SQLAlchemy then matches a positional list. By name it would
/// miss a column PostgreSQL names differently from sqlc (`coalesce`), and it cannot compile an empty name. The names we
/// pass are the fields', which SQLAlchemy ignores.
pub fn statement(query: &Query) -> ast::Expr {
    let mut statement = (SQLALCHEMY.attr("text")).call([string(&text::text(&query.sql))]);
    let json = |class: &str| (POSTGRESQL.attr(class)).call_with([], [keyword("none_as_null", true_())]);
    let binds: Vec<ast::Expr> = (query.params.iter())
        .filter_map(|parameter| {
            let how = match (&parameter.ty.element, parameter.ty.element.json()) {
                _ if query.sql.is_slice(&parameter.name) => keyword("expanding", true_()),
                (_, Some(Json::Json)) => keyword("type_", json("JSON")),
                (_, Some(Json::Jsonb)) => keyword("type_", json("JSONB")),
                (
                    Type::Library {
                        sql_type: Some(sql_type),
                        ..
                    },
                    _,
                ) => keyword("type_", POSTGRESQL.attr(sql_type)),
                _ => return None,
            };
            Some((SQLALCHEMY.attr("bindparam")).call_with([string(&parameter.name)], [how]))
        })
        .collect();
    if !binds.is_empty() {
        statement = statement.attr("bindparams").call(binds);
    }
    let types: Vec<Option<ast::Expr>> = query.columns.iter().map(column_type).collect();
    if types.iter().all(Option::is_none) {
        return statement;
    }
    let columns = (query.columns.iter().zip(types))
        .map(|(column, ty)| (SQLALCHEMY.attr("column")).call(std::iter::once(string(&column.name)).chain(ty)));
    statement.attr("columns").call(columns)
}

/// The types SQLAlchemy decides whatever the driver: a range as SQLAlchemy's `Range`, and an `hstore` as a `dict`.
pub fn scalar_type(pg_type: &str) -> Option<Type> {
    // SQLAlchemy registers psycopg's `hstore` codec and parses asyncpg's text through its own type
    if pg_type == "hstore" {
        return Some(Type::Library {
            annotation: DICT.index_each(vec![STR.into(), STR.or_none()]),
            sql_type: Some("HSTORE"),
        });
    }
    let (bound, sql_type) = super::range(pg_type)?;
    Some(Type::Library {
        annotation: POSTGRESQL.attr("Range").index(bound),
        sql_type: Some(sql_type),
    })
}

/// The SQLAlchemy type a column loads through, if a driver needs one. We pass a type of SQLAlchemy's own class as the
/// class, which SQLAlchemy instantiates, as SQLAlchemy 2.0's `HSTORE()` is untyped.
fn column_type(column: &Field) -> Option<ast::Expr> {
    match (&column.ty.element, column.ty.array_dims) {
        (Type::Enum(class), 1..) => {
            let labels = SQLALCHEMY.attr("Enum").call([starred(MODELS.attr(class))]);
            Some(POSTGRESQL.attr("ARRAY").call([labels]))
        }
        (
            Type::Library {
                sql_type: Some(sql_type),
                ..
            },
            0,
        ) => Some(POSTGRESQL.attr(sql_type)),
        _ => None,
    }
}

pub fn connection(asynchronous: bool) -> ast::Expr {
    match asynchronous {
        true => ASYNC_CONNECTION.into(),
        false => SQLALCHEMY.attr("Connection"),
    }
}

/// What an `:execresult` returns.
pub fn result() -> ast::Expr {
    SQLALCHEMY.attr("CursorResult").index(ANY)
}

/// One execution of the statement, with the result shaped by the command. A
/// batch executes once per row and collects each result.
pub fn body(query: &Query, asynchronous: bool) -> Vec<ast::Stmt> {
    let rows = query.takes.rows();

    // A batch binds each row's values (`r.p`), but all at once as `params` if it returns nothing
    let values = match (rows, &query.command, query.params.is_empty()) {
        (true, Command::Exec | Command::CopyFrom { .. }, _) => Some(PARAMS.into()),
        (false, _, true) => None,
        _ => Some(named(query)),
    };
    let executed = (CONNECTION.attr("execute"))
        .call(std::iter::once(name(&query.constant)).chain(values))
        .awaited(asynchronous);

    let (lines, value) = match &query.command {
        // We execute nothing for no rows, as SQLAlchemy raises on an empty list
        Command::Exec if rows => {
            return stmts![
                assign(PARAMS, list_of(named(query), each(R, ROWS))),
                if_(PARAMS, vec![expression(executed)]),
            ];
        }
        Command::Exec => return vec![expression(executed)],
        // asyncpg counts no list of rows (-1), but on PostgreSQL every row is written or the call raises
        Command::CopyFrom { every_row } => {
            let params = assign(PARAMS, list_of(named(query), each(R, ROWS)));
            return match every_row {
                true => stmts![
                    params,
                    if_(PARAMS, vec![expression(executed)]),
                    return_(LEN.call([PARAMS.into()])),
                ],
                false => stmts![
                    params,
                    if_(PARAMS, vec![return_(executed.attr("rowcount"))]),
                    return_(int(0)),
                ],
            };
        }
        Command::ExecRows => (vec![], executed.attr("rowcount")),
        Command::ExecLastId => (vec![], executed.attr("lastrowid")),
        Command::ExecResult => (vec![], executed),
        Command::One(Shape::Scalar(ty)) => {
            // Without a row the value is `None`
            let (ty, method) = match query.raises {
                true => (ty.clone(), "scalar_one"),
                false => (
                    FieldType {
                        optional: true,
                        ..ty.clone()
                    },
                    "scalar_one_or_none",
                ),
            };
            let one = executed.attr(method).call([]);
            // The row's value is `Any`, so we annotate a local to state the type, unless we convert it anyway
            let line = match ty.converts() {
                false => ann_assign(VALUE, ty.annotation(Scope::Queries), one),
                true => assign(VALUE, one),
            };
            (vec![line], ty.read(VALUE, Scope::Queries))
        }
        Command::Many(Shape::Scalar(ty)) => {
            let values = executed.attr("scalars").call([]);
            let value = match ty.converts() {
                false => LIST.call([values]),
                true => list_of(ty.read(VALUE, Scope::Queries), each(VALUE, values)),
            };
            (vec![], value)
        }
        Command::One(shape) if query.raises => (vec![], shape.read(executed.attr("one").call([]))),
        Command::One(shape) => {
            let row = assign(ROW, executed.attr("one_or_none").call([]));
            (vec![row], unless_none(ROW, shape.read(ROW)))
        }
        Command::Many(shape) => (vec![], list_of(shape.read(ROW), each(ROW, executed))),
    };
    if !rows {
        return stmts![lines, return_(value)];
    }
    let per_row = each(R, ROWS);
    match lines.is_empty() {
        true => vec![return_(list_of(value, per_row))],
        false => stmts![
            ann_assign(VALUES, super::returns(query, result), list(vec![])),
            for_(per_row, stmts![lines, expression(VALUES.attr("append").call([value]))],),
            return_(VALUES),
        ],
    }
}

/// The rows of a `:many` as the caller iterates them, on a server-side cursor where the dialect has one, which closes
/// with the result:
///
/// ```python
/// with connection.execute(_LIST_AUTHORS, {...}, execution_options={"stream_results": True}) as result:
///     yield (models.Author.from_row(row) for row in result)
/// ```
pub fn stream(query: &Query, shape: &Shape, asynchronous: bool) -> Vec<ast::Stmt> {
    let values = (!query.params.is_empty()).then(|| named(query));
    let arguments = std::iter::once(name(&query.constant)).chain(values);
    let result = match asynchronous {
        true => CONNECTION.attr("stream").call(arguments),
        false => (CONNECTION.attr("execute")).call_with(
            arguments,
            [keyword(
                "execution_options",
                dict(vec![(string("stream_results"), true_())]),
            )],
        ),
    };
    let rows = generator_of(shape.read(ROW), each(ROW, RESULT).asynchronous(asynchronous));
    vec![with(asynchronous, vec![result.entered_as(RESULT)], vec![yield_(rows)])]
}

/// `{"id": id}`
fn named(query: &Query) -> ast::Expr {
    let items = (query.params.iter())
        .map(|parameter| (string(&parameter.name), super::value(query, &parameter.name)))
        .collect();
    dict(items)
}
