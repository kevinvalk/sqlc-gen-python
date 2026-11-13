//! A driver that takes sqlc's SQL as written, placeholders and all, and the
//! parameters by position (PEP 249): psycopg's raw cursors and `sqlite3`.
//! Nothing is rewritten, so the database runs exactly sqlc's statement.

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use super::class;

use crate::api::models::{Enum, Field};
use crate::api::queries::{Command, Query, Shape};
use crate::python::{self, Scope};

#[cfg(test)]
#[path = "dbapi_spec.rs"]
mod spec;

/// How a driver is written, for the sync API and then the async one.
pub struct Driver {
    /// The annotation of the connection.
    pub connection: [&'static str; 2],
    /// What a statement is executed on.
    pub cursor: [&'static str; 2],
    /// What `:execresult` returns: the cursor the statement ran on.
    pub cursor_type: [&'static str; 2],
    /// A parameter's value, from the expression holding it.
    pub bind: fn(&Field, &str) -> String,
    /// Whether an array of enums comes back as the array's text.
    pub text_arrays: bool,
}

/// sqlc's statement, as it wrote it.
pub fn statement(query: &Query) -> ast::Expr {
    python::string(&query.sql.text())
}

/// What a query's function returns, and its body: one execution of its
/// statement, the result shaped by its command (`docs/python-output.md` §6,
/// "Functions").
pub fn body(driver: &Driver, query: &Query, asynchronous: bool) -> (String, String) {
    let at = usize::from(asynchronous);
    let wait = if asynchronous { "await " } else { "" };
    // The parameters as a tuple, each read from `of(name)`.
    let tuple = |of: &dyn Fn(&str) -> String| {
        let values: Vec<String> = (query.params.iter())
            .map(|param| (driver.bind)(param, &of(&param.name)))
            .collect();
        match values.as_slice() {
            [value] => format!("({value},)"),
            values => format!("({})", values.join(", ")),
        }
    };
    let args = match query.params.is_empty() {
        true => String::new(),
        false => format!(", {}", tuple(&|name| name.to_string())),
    };
    let cursor = driver.cursor[at];
    let execute = format!("{wait}{cursor}.execute({}{args})", query.constant);
    // `cur`, the cursor the statement ran on, then `ret`.
    let fetch = |ret: String| format!("cur = {execute}\n{ret}");

    match &query.command {
        Command::Exec => ("None".into(), execute),
        Command::ExecRows => match asynchronous {
            false => ("int".into(), format!("return {execute}.rowcount")),
            true => ("int".into(), format!("return ({execute}).rowcount")),
        },
        Command::ExecResult => (driver.cursor_type[at].into(), format!("return {execute}")),
        Command::CopyFrom(_) => (
            "None".into(),
            format!(
                "{wait}{cursor}.executemany({}, [{} for r in rows])",
                query.constant,
                tuple(&|name| format!("r.{name}"))
            ),
        ),
        Command::One(shape) => {
            let (returns, ret) = match shape {
                Shape::Scalar(ty) if !ty.converts() => {
                    let annotation = ty.annotation(Scope::Queries);
                    // A row's value is `Any`: the annotated local states the
                    // type a bare `return` would leak.
                    let ret = format!("value: {annotation} = row[0]\n        return value");
                    (annotation, ret)
                }
                Shape::Scalar(ty) => (
                    ty.annotation(Scope::Queries),
                    format!(
                        "return {}",
                        ty.read("row[0]", Scope::Queries, driver.text_arrays)
                    ),
                ),
                shape => (
                    class(shape),
                    format!("return {}.from_row(row)", class(shape)),
                ),
            };
            let body = fetch(format!(
                "match {wait}cur.fetchmany(2):\n    case [row]:\n        {ret}\n    \
                 case []:\n        raise NoRowError\n    case _:\n        raise MultipleRowsError"
            ));
            (returns, body)
        }
        Command::Many(shape) => {
            let (item, each) = match shape {
                Shape::Scalar(ty) => (
                    ty.annotation(Scope::Queries),
                    ty.read("row[0]", Scope::Queries, driver.text_arrays),
                ),
                shape => (class(shape), format!("{}.from_row(row)", class(shape))),
            };
            let body = fetch(format!("return [{each} for row in {wait}cur.fetchall()]"));
            (format!("list[{item}]"), body)
        }
    }
}

/// An error if `sql_enum` has a label PostgreSQL quotes in an array's text,
/// which `from_array` reads, for a driver of [`Driver::text_arrays`], by
/// splitting it at its commas: an empty label,
/// `NULL`, or one holding whitespace, a quote, a backslash, a brace or a
/// comma.
pub fn check_text_array(sql_enum: &Enum) -> Result<()> {
    let quoted = |label: &str| {
        label.is_empty()
            || label.eq_ignore_ascii_case("null")
            || (label.chars()).any(|c| "{},\"\\ \t\n\r\x0b\x0c".contains(c))
    };
    if let Some(member) = sql_enum.members.iter().find(|m| quoted(&m.value)) {
        bail!(
            "psycopg returns an array of this enum as its text, in which PostgreSQL quotes the \
             label {:?}; rename the label, or use the sqlalchemy backend",
            member.value
        );
    }
    Ok(())
}
