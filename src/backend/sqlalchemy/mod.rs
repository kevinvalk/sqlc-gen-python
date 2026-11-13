//! SQLAlchemy Core: each statement a `sqlalchemy.text()` constant, run on a
//! `sqlalchemy.Connection` or an `AsyncConnection` (`docs/python-output.md`
//! §6).

pub mod text;

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use super::{block, class};
use crate::api::models::Field;
use crate::api::queries::{Command, Query, Shape};
use crate::engine::Engine;
use crate::python::{self, Json, Scope, Type};

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
    let mut statement = python::call(
        python::expr("sqlalchemy.text"),
        [python::string(&text::text(&query.sql))],
    );
    let binds: Vec<ast::Expr> = (query.params.iter())
        .filter_map(|param| {
            let how = match (&param.ty.element, param.ty.element.json()) {
                _ if query.sql.is_slice(&param.name) => "expanding=True".to_string(),
                (_, Some(Json::Json)) => "type_=postgresql.JSON(none_as_null=True)".into(),
                (_, Some(Json::Jsonb)) => "type_=postgresql.JSONB(none_as_null=True)".into(),
                (
                    Type::Library {
                        sql_type: Some(sql_type),
                        ..
                    },
                    _,
                ) => format!("type_={sql_type}"),
                _ => return None,
            };
            Some(python::expr(&format!(
                "sqlalchemy.bindparam(\"{}\", {how})",
                param.name
            )))
        })
        .collect();
    if !binds.is_empty() {
        statement = python::call(python::attribute(statement, "bindparams"), binds);
    }
    let types: Vec<Option<ast::Expr>> = query.columns.iter().map(column_type).collect();
    if types.iter().all(Option::is_none) {
        return statement;
    }
    let columns = (query.columns.iter().zip(types)).map(|(column, ty)| {
        let args = [python::string(&column.name)].into_iter().chain(ty);
        python::call(python::expr("sqlalchemy.column"), args)
    });
    python::call(python::attribute(statement, "columns"), columns)
}

/// The Python type of a column of `data_type` that SQLAlchemy decides,
/// whatever the driver: a range, as SQLAlchemy's `Range`; an `hstore`, as a
/// `dict`.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = data_type.strip_prefix("pg_catalog.").unwrap_or(data_type);
    // SQLAlchemy registers psycopg's `hstore` codec, and parses asyncpg's
    // text, through its own type.
    if name == "hstore" {
        return Some(Type::Library {
            annotation: "dict[str, str | None]".into(),
            sql_type: Some("postgresql.HSTORE"),
        });
    }
    let (bound, sql_type) = super::range(name)?;
    Some(Type::Library {
        annotation: format!("postgresql.Range[{bound}]"),
        sql_type: Some(sql_type),
    })
}

/// The SQLAlchemy type a column loads through, if a driver needs one: an
/// array of enums, or a type of SQLAlchemy's own class, as the class, which
/// SQLAlchemy instantiates (SQLAlchemy 2.0's `HSTORE()` is untyped).
fn column_type(column: &Field) -> Option<ast::Expr> {
    match (&column.ty.element, column.ty.array_dims) {
        (Type::Enum(class), 1..) => Some(python::expr(&format!(
            "postgresql.ARRAY(sqlalchemy.Enum(*models.{class}))"
        ))),
        (
            Type::Library {
                sql_type: Some(sql_type),
                ..
            },
            0,
        ) => Some(python::expr(sql_type)),
        _ => None,
    }
}

/// The annotation of the connection the queries run on.
pub fn connection(asynchronous: bool) -> &'static str {
    match asynchronous {
        true => "AsyncConnection",
        false => "sqlalchemy.Connection",
    }
}

/// What a query's function returns, and its body: one execution of its
/// statement, the result shaped by its command (§6, "Functions"). A batch
/// executes once per row, collecting each result.
pub fn body(query: &Query, asynchronous: bool) -> (String, String) {
    // Each parameter as `each` writes it, comma-separated.
    let each = |write: fn(&Field) -> String| {
        (query.params.iter().map(write))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // A batch binds each row's values, `r.p`; at once, as `params`, if it
    // returns nothing.
    let values = match (&query.rows, &query.command, query.params.is_empty()) {
        (Some(_), Command::Exec, _) => ", params".to_string(),
        (Some(_), _, _) => format!(", {{{}}}", each(|p| format!("\"{0}\": r.{0}", p.name))),
        (None, _, true) => String::new(),
        (None, _, false) => format!(", {{{}}}", each(|p| format!("\"{0}\": {0}", p.name))),
    };
    // `conn.execute(...)` then `then`, awaited if need be.
    let execute = |then: &str| {
        let call = format!("conn.execute({}{values})", query.constant);
        match (asynchronous, then) {
            (false, _) => format!("{call}{then}"),
            (true, "") => format!("await {call}"),
            (true, _) => format!("(await {call}){then}"),
        }
    };
    // What a row's execution returns: its type, the lines that compute it,
    // and the value.
    let (returns, lines, value): (String, Vec<String>, String) = match &query.command {
        Command::Exec if query.rows.is_some() => {
            let params = format!(
                "params = [{{{}}} for r in rows]",
                each(|p| format!("\"{0}\": r.{0}", p.name))
            );
            let lines = [vec![params], block("if params:", vec![execute("")])].concat();
            return ("None".into(), lines.join("\n"));
        }
        Command::Exec => return ("None".into(), execute("")),
        Command::ExecRows => ("int".into(), vec![], execute(".rowcount")),
        Command::ExecLastId => ("int".into(), vec![], execute(".lastrowid")),
        Command::ExecResult => ("sqlalchemy.CursorResult[Any]".into(), vec![], execute("")),
        Command::One(Shape::Scalar(ty)) => {
            let annotation = ty.annotation(Scope::Queries);
            // `scalar_one()` is `Any`: the annotated local states the type the
            // value would leak, unless it is converted.
            let local = match ty.converts() {
                false => format!("value: {annotation}"),
                true => "value".into(),
            };
            let line = format!("{local} = {}", execute(".scalar_one()"));
            let value = ty.read("value", Scope::Queries, false);
            (annotation, vec![line], value)
        }
        Command::Many(Shape::Scalar(ty)) => {
            let values = execute(".scalars()");
            let value = match ty.converts() {
                false => format!("list({values})"),
                true => format!(
                    "[{} for value in {values}]",
                    ty.read("value", Scope::Queries, false)
                ),
            };
            (
                format!("list[{}]", ty.annotation(Scope::Queries)),
                vec![],
                value,
            )
        }
        Command::One(shape) => {
            let class = class(shape);
            let value = format!("{class}.from_row({})", execute(".one()"));
            (class, vec![], value)
        }
        Command::Many(shape) => {
            let class = class(shape);
            let value = format!("[{class}.from_row(row) for row in {}]", execute(""));
            (format!("list[{class}]"), vec![], value)
        }
    };
    let (returns, body) = match (&query.rows, lines.is_empty()) {
        (None, _) => (returns, [lines, vec![format!("return {value}")]].concat()),
        (Some(_), true) => (
            format!("list[{returns}]"),
            vec![format!("return [{value} for r in rows]")],
        ),
        (Some(_), false) => {
            let each = [lines, vec![format!("values.append({value})")]].concat();
            let body = [
                vec![format!("values: list[{returns}] = []")],
                block("for r in rows:", each),
                vec!["return values".into()],
            ];
            (format!("list[{returns}]"), body.concat())
        }
    };
    (returns, body.join("\n"))
}
