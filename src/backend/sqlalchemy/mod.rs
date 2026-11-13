//! SQLAlchemy Core: each statement a `sqlalchemy.text()` constant, run on a
//! `sqlalchemy.Connection` or an `AsyncConnection` (`docs/python-output.md`
//! §6).

pub mod text;

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use super::class;
use crate::api::models::Field;
use crate::api::queries::{Command, Query, Shape};
use crate::engine::Engine;
use crate::python::{self, Json, Scope, Type, py_expr, py_stmt, py_stmts};

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
    let sql = python::string(&text::text(&query.sql));
    let mut statement = py_expr!("sqlalchemy.text($sql)", sql = sql);
    let binds: Vec<ast::Expr> = (query.params.iter())
        .filter_map(|param| {
            // The keyword that says how, and its value.
            let (how, value) = match (&param.ty.element, param.ty.element.json()) {
                _ if query.sql.is_slice(&param.name) => ("expanding", "True"),
                (_, Some(Json::Json)) => ("type_", "postgresql.JSON(none_as_null=True)"),
                (_, Some(Json::Jsonb)) => ("type_", "postgresql.JSONB(none_as_null=True)"),
                (
                    Type::Library {
                        sql_type: Some(sql_type),
                        ..
                    },
                    _,
                ) => ("type_", *sql_type),
                _ => return None,
            };
            Some(py_expr!(
                "sqlalchemy.bindparam($name, **$how)",
                name = python::string(&param.name),
                how = vec![python::keyword(how, python::expr(value))],
            ))
        })
        .collect();
    if !binds.is_empty() {
        statement = py_expr!("$s.bindparams(*$binds)", s = statement, binds = binds);
    }
    let types: Vec<Option<ast::Expr>> = query.columns.iter().map(column_type).collect();
    if types.iter().all(Option::is_none) {
        return statement;
    }
    let columns = (query.columns.iter().zip(types))
        .map(|(column, ty)| {
            py_expr!(
                "sqlalchemy.column($name, *$type_)",
                name = python::string(&column.name),
                type_ = ty.into_iter().collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    py_expr!("$s.columns(*$columns)", s = statement, columns = columns)
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
        (Type::Enum(class), 1..) => Some(py_expr!(
            "postgresql.ARRAY(sqlalchemy.Enum(*models.$class))",
            class = class,
        )),
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
pub fn body(query: &Query, asynchronous: bool) -> (ast::Expr, Vec<ast::Stmt>) {
    // The parameters by name, each read as `read` gives it.
    let named = |read: &dyn Fn(&str) -> ast::Expr| {
        let items = (query.params.iter())
            .map(|p| (python::string(&p.name), read(&p.name)))
            .collect();
        python::dict(items)
    };
    let own = |name: &str| python::name(name);
    let of_row = |name: &str| py_expr!("r.$name", name = name);
    // A batch binds each row's values, `r.p`; at once, as `params`, if it
    // returns nothing.
    let values = match (&query.rows, &query.command, query.params.is_empty()) {
        (Some(_), Command::Exec, _) => vec![python::name("params")],
        (Some(_), _, _) => vec![named(&of_row)],
        (None, _, true) => vec![],
        (None, _, false) => vec![named(&own)],
    };
    // `conn.execute(...)`, awaited in a coroutine.
    let executed = py_expr!(
        &python::coroutine("await conn.execute($statement, *$values)", asynchronous),
        statement = &query.constant,
        values = values,
    );
    // What a row's execution returns: its type, the statements that compute
    // it, and the value.
    let (returns, lines, value) = match &query.command {
        Command::Exec if query.rows.is_some() => {
            let body = py_stmts!(
                "
                params = [$row for r in rows]
                if params:
                    $executed
                ",
                row = named(&of_row),
                executed = executed,
            );
            return (python::expr("None"), body);
        }
        Command::Exec => return (python::expr("None"), vec![python::expression(executed)]),
        Command::ExecRows => (
            python::name("int"),
            vec![],
            py_expr!("$r.rowcount", r = executed),
        ),
        Command::ExecLastId => (
            python::name("int"),
            vec![],
            py_expr!("$r.lastrowid", r = executed),
        ),
        Command::ExecResult => (
            python::expr("sqlalchemy.CursorResult[Any]"),
            vec![],
            executed,
        ),
        Command::One(Shape::Scalar(ty)) => {
            let annotation = ty.annotation(Scope::Queries);
            let one = py_expr!("$r.scalar_one()", r = executed);
            // `scalar_one()` is `Any`: the annotated local states the type the
            // value would leak, unless it is converted.
            let line = match ty.converts() {
                false => py_stmt!(
                    "value: $annotation = $one",
                    annotation = annotation.clone(),
                    one = one,
                ),
                true => py_stmt!("value = $one", one = one),
            };
            let value = ty.read(python::name("value"), Scope::Queries, false);
            (annotation, vec![line], value)
        }
        Command::Many(Shape::Scalar(ty)) => {
            let values = py_expr!("$r.scalars()", r = executed);
            let value = match ty.converts() {
                false => py_expr!("list($values)", values = values),
                true => py_expr!(
                    "[$read for value in $values]",
                    read = ty.read(python::name("value"), Scope::Queries, false),
                    values = values,
                ),
            };
            let returns = py_expr!("list[$item]", item = ty.annotation(Scope::Queries));
            (returns, vec![], value)
        }
        Command::One(shape) => {
            let value = py_expr!(
                "$class.from_row($r.one())",
                class = class(shape),
                r = executed
            );
            (class(shape), vec![], value)
        }
        Command::Many(shape) => {
            let value = py_expr!(
                "[$class.from_row(row) for row in $r]",
                class = class(shape),
                r = executed,
            );
            (
                py_expr!("list[$class]", class = class(shape)),
                vec![],
                value,
            )
        }
    };
    let body = match (&query.rows, lines.is_empty()) {
        (None, _) => {
            let returned = py_stmt!("return $value", value = value);
            return (returns, lines.into_iter().chain([returned]).collect());
        }
        (Some(_), true) => py_stmts!("return [$value for r in rows]", value = value),
        (Some(_), false) => py_stmts!(
            "
            values: list[$returns] = []
            for r in rows:
                $lines
                values.append($value)
            return values
            ",
            returns = returns.clone(),
            lines = lines,
            value = value,
        ),
    };
    (py_expr!("list[$returns]", returns = returns), body)
}
