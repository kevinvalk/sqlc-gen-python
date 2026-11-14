//! SQLAlchemy Core: each statement a `sqlalchemy.text()` constant, run on a
//! `sqlalchemy.Connection` or an `AsyncConnection` (`docs/python-output.md`
//! §6).

pub mod text;

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use crate::api::models::{Field, FieldType};
use crate::api::queries::{Command, Query, Shape};
use crate::engine::Engine;
use crate::python::vocabulary::{
    ANY, ASYNC_CONNECTION, CONN, DICT, LIST, MODELS, PARAMS, POSTGRESQL, R, ROW, ROWS, SQLALCHEMY,
    STR, VALUE, VALUES,
};
use crate::python::{
    Expression, Json, Scope, Type, ann_assign, assign, dict, each, expression, for_, if_, keyword,
    list, list_of, name, return_, starred, stmts, string, true_, unless_none,
};

/// An error for what SQLAlchemy cannot run faithfully: a placeholder `text()`
/// would not bind ([`text::check`]), and a count of a MERGE. SQLAlchemy reads
/// asyncpg's row count off the command tag, with an expression that knows no
/// `MERGE n`, so it would be -1 there and the real count on psycopg.
pub fn check(engine: Engine, query: &Query) -> Result<()> {
    let cmd = match query.command {
        Command::ExecRows => ":execrows",
        Command::ExecResult => ":execresult",
        _ => return text::check(&query.sql),
    };
    if engine.merges(&query.sql.text()) {
        bail!(
            "{cmd} counts -1 rows for a MERGE on asyncpg, whose row count SQLAlchemy cannot read, \
             and the real count on psycopg; use :exec"
        );
    }
    text::check(&query.sql)
}

/// `sqlalchemy.text(...)`, typed only where a driver needs it (§6, "On SQLAlchemy",
/// rule 4):
/// each JSON parameter binds through SQLAlchemy's JSON type, each
/// `sqlc.slice` expands to a placeholder per item, and each column of enum
/// arrays loads through an `ARRAY` of the enum, without which psycopg returns
/// the array's text.
///
/// The columns are typed by position, every one of them, as SQLAlchemy matches
/// a positional list: by name, it would miss a column PostgreSQL names
/// differently from sqlc (`coalesce`), and cannot compile an empty one. The
/// names are the fields', which SQLAlchemy then ignores.
pub fn statement(query: &Query) -> ast::Expr {
    let mut statement = (SQLALCHEMY.attr("text")).call([string(&text::text(&query.sql))]);
    let json =
        |class: &str| (POSTGRESQL.attr(class)).call_with([], [keyword("none_as_null", true_())]);
    let binds: Vec<ast::Expr> = (query.params.iter())
        .filter_map(|param| {
            let how = match (&param.ty.element, param.ty.element.json()) {
                _ if query.sql.is_slice(&param.name) => keyword("expanding", true_()),
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
            Some((SQLALCHEMY.attr("bindparam")).call_with([string(&param.name)], [how]))
        })
        .collect();
    if !binds.is_empty() {
        statement = statement.attr("bindparams").call(binds);
    }
    let types: Vec<Option<ast::Expr>> = query.columns.iter().map(column_type).collect();
    if types.iter().all(Option::is_none) {
        return statement;
    }
    let columns = (query.columns.iter().zip(types)).map(|(column, ty)| {
        (SQLALCHEMY.attr("column")).call(std::iter::once(string(&column.name)).chain(ty))
    });
    statement.attr("columns").call(columns)
}

/// The Python type of a column of PostgreSQL's `pg_type` that SQLAlchemy decides,
/// whatever the driver: a range, as SQLAlchemy's `Range`; an `hstore`, as a
/// `dict`.
pub fn scalar_type(pg_type: &str) -> Option<Type> {
    // SQLAlchemy registers psycopg's `hstore` codec, and parses asyncpg's
    // text, through its own type.
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

/// The SQLAlchemy type a column loads through, if a driver needs one: an
/// array of enums, or a type of SQLAlchemy's own class, as the class, which
/// SQLAlchemy instantiates (SQLAlchemy 2.0's `HSTORE()` is untyped).
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

/// The annotation of the connection the queries run on.
pub fn connection(asynchronous: bool) -> ast::Expr {
    match asynchronous {
        true => ASYNC_CONNECTION.into(),
        false => SQLALCHEMY.attr("Connection"),
    }
}

/// `sqlalchemy.CursorResult[Any]`: what an `:execresult` returns.
pub fn result() -> ast::Expr {
    SQLALCHEMY.attr("CursorResult").index(ANY)
}

/// The body of a query's function: one execution of its statement, the
/// result shaped by its command (§6, "Functions"). A batch executes once per
/// row, collecting each result.
pub fn body(query: &Query, asynchronous: bool) -> Vec<ast::Stmt> {
    let rows = query.takes.rows();
    // A batch binds each row's values, `r.p`; at once, as `params`, if it
    // returns nothing.
    let values = match (rows, &query.command, query.params.is_empty()) {
        (true, Command::Exec, _) => Some(PARAMS.into()),
        (false, _, true) => None,
        _ => Some(named(query)),
    };
    let executed = (CONN.attr("execute"))
        .call(std::iter::once(name(&query.constant)).chain(values))
        .awaited(asynchronous);

    // What a run returns: the statements that compute it, and the value.
    let (lines, value) = match &query.command {
        // Nothing to execute for no rows.
        Command::Exec if rows => {
            return stmts![
                assign(PARAMS, list_of(named(query), each(R, ROWS))),
                if_(PARAMS, vec![expression(executed)]),
            ];
        }
        Command::Exec => return vec![expression(executed)],
        Command::ExecRows => (vec![], executed.attr("rowcount")),
        Command::ExecLastId => (vec![], executed.attr("lastrowid")),
        Command::ExecResult => (vec![], executed),
        Command::One(Shape::Scalar(ty)) => {
            // Without a row, `None`: the value then may be one.
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
            // The row's value is `Any`: the annotated local states the type
            // the value would leak, unless it is converted.
            let line = match ty.converts() {
                false => ann_assign(VALUE, ty.annotation(Scope::Queries), one),
                true => assign(VALUE, one),
            };
            (vec![line], ty.read(VALUE, Scope::Queries, false))
        }
        Command::Many(Shape::Scalar(ty)) => {
            let values = executed.attr("scalars").call([]);
            let value = match ty.converts() {
                false => LIST.call([values]),
                true => list_of(ty.read(VALUE, Scope::Queries, false), each(VALUE, values)),
            };
            (vec![], value)
        }
        Command::One(shape) if query.raises => {
            (vec![], shape.read(executed.attr("one").call([]), false))
        }
        Command::One(shape) => {
            let row = assign(ROW, executed.attr("one_or_none").call([]));
            (vec![row], unless_none(ROW, shape.read(ROW, false)))
        }
        Command::Many(shape) => (vec![], list_of(shape.read(ROW, false), each(ROW, executed))),
    };
    if !rows {
        return stmts![lines, return_(value)];
    }
    let per_row = each(R, ROWS);
    match lines.is_empty() {
        true => vec![return_(list_of(value, per_row))],
        false => stmts![
            ann_assign(VALUES, super::returns(query, result), list(vec![])),
            for_(
                per_row,
                stmts![lines, expression(VALUES.attr("append").call([value]))],
            ),
            return_(VALUES),
        ],
    }
}

/// The parameters by name, each read as the function takes it: `{"id": id}`.
fn named(query: &Query) -> ast::Expr {
    let items = (query.params.iter())
        .map(|p| (string(&p.name), super::value(query, &p.name)))
        .collect();
    dict(items)
}
