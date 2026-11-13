//! A DB-API driver (PEP 249), which takes parameters by position: psycopg's
//! raw cursors and `sqlite3`, which take sqlc's SQL as written, placeholders
//! and all, and PyMySQL and asyncmy, which take `%s` for each.

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use super::class;

use crate::api::models::{Enum, Field};
use crate::api::queries::{Command, CopyInto, Query, Shape};
use crate::engine::Piece;
use crate::python::{self, Scope};

#[cfg(test)]
#[path = "dbapi_spec.rs"]
mod spec;

/// How a driver is written, for blocking functions and then coroutines.
pub struct Driver {
    /// The annotation of the connection.
    pub connection: [&'static str; 2],
    /// The cursor a statement is executed on, from `conn`.
    pub cursor: [&'static str; 2],
    /// What `:execresult` returns: the cursor the statement ran on.
    pub cursor_type: [&'static str; 2],
    /// The placeholder the driver takes, if not sqlc's own: then every other
    /// `%` is doubled, as the driver formats the statement with `%`.
    pub placeholder: Option<&'static str>,
    /// Whether `execute` returns the rows it affected, which is then the
    /// count: the cursor's `rowcount` is not typed as one.
    pub execute_counts: bool,
    /// A subquery of no rows, for a `sqlc.slice` of none: `x IN (...)` is
    /// then false, and `x NOT IN (...)` true, as of an empty list.
    pub no_rows: &'static str,
    /// A parameter's value, from the expression holding it.
    pub bind: fn(&Field, &str) -> String,
    /// Whether an array of enums comes back as the array's text.
    pub text_arrays: bool,
    /// Whether a `:copyfrom` runs PostgreSQL's `COPY`, through the cursor's
    /// `copy()`, rather than one `INSERT` per row.
    pub copies: bool,
}

impl Driver {
    /// A placeholder of `sql` as the statement constant writes it.
    fn placeholder(&self, placeholder: &str) -> String {
        match self.placeholder {
            None => placeholder.into(),
            // A slice keeps its marker, which the function replaces.
            Some(own) => match placeholder.rfind("*/") {
                Some(end) => format!("{}{own}", &placeholder[..end + 2]),
                None => own.into(),
            },
        }
    }
}

/// sqlc's statement, as the driver takes it: as written, or with its
/// placeholders and each `%` written as the driver formats them. Without
/// parameters, a driver formats nothing, so nothing is rewritten.
pub fn statement(driver: &Driver, query: &Query) -> ast::Expr {
    if driver.copies
        && let Some(into) = &query.copy_into
    {
        return python::string(&copy(into));
    }
    let text = match (driver.placeholder, query.params.is_empty()) {
        (Some(_), false) => (query.sql.0.iter())
            .map(|piece| match piece {
                Piece::Text(text) => text.replace('%', "%%"),
                Piece::Param { placeholder, .. } => driver.placeholder(placeholder),
            })
            .collect(),
        _ => query.sql.text(),
    };
    python::string(&text)
}

/// The `COPY` that writes rows into `into`'s table and columns, every name
/// quoted: as sqlc reports it, it is the name PostgreSQL stores.
fn copy(into: &CopyInto) -> String {
    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    let table = match into.schema.as_str() {
        "" => quote(&into.table),
        schema => format!("{}.{}", quote(schema), quote(&into.table)),
    };
    let columns: Vec<String> = into.columns.iter().map(|c| quote(c)).collect();
    format!("COPY {table} ({}) FROM STDIN", columns.join(", "))
}

/// What a query's function returns, and its body: one execution of its
/// statement on a cursor of its own, the result shaped by its command
/// (`docs/python-output.md` §6, "Functions").
pub fn body(driver: &Driver, query: &Query, asynchronous: bool) -> (String, String) {
    let at = usize::from(asynchronous);
    let wait = if asynchronous { "await " } else { "" };
    // The value of each placeholder, read from `of(name)`, as a tuple: a
    // slice's items each a value of its own.
    let tuple = |names: &[String], of: &dyn Fn(&str) -> String| {
        let values: Vec<String> = (names.iter())
            .map(|name| match query.slices.contains(name) {
                true => format!("*{}", of(name)),
                false => {
                    let param = query.params.iter().find(|p| p.name == *name);
                    param.map_or_else(|| of(name), |param| (driver.bind)(param, &of(name)))
                }
            })
            .collect();
        match values.as_slice() {
            [value] => format!("({value},)"),
            values => format!("({})", values.join(", ")),
        }
    };
    // The statement, each slice's placeholder made one per item.
    let mut statement = query.constant.clone();
    for piece in &query.sql.0 {
        if let Piece::Param { placeholder, name } = piece
            && query.slices.contains(name)
        {
            let own = driver.placeholder.unwrap_or("?");
            statement += &format!(
                ".replace({:?}, \", \".join([{own:?}] * len({name})) or {:?})",
                driver.placeholder(placeholder),
                driver.no_rows
            );
        }
    }
    let args = match query.binds.is_empty() {
        true => String::new(),
        false => format!(", {}", tuple(&query.binds, &|name| name.to_string())),
    };
    let cursor = format!("cur = {}", driver.cursor[at]);
    let execute = format!("{wait}cur.execute({statement}{args})");
    // The cursor, the statement run on it, then `then`.
    let run = |then: &str| format!("{cursor}\n{execute}\n{then}");
    // Each row of the parameters, as its tuple.
    let rows = || {
        let names: Vec<String> = query.params.iter().map(|p| p.name.clone()).collect();
        tuple(&names, &|name| format!("r.{name}"))
    };

    let (returns, body) = match (&query.command, &query.rows) {
        (Command::CopyFrom, _) if driver.copies && query.copy_into.is_some() => {
            let with = if asynchronous { "async with" } else { "with" };
            let body = format!(
                "{cursor}\n{with} cur.copy({}) as copy:\n    for r in rows:\n        {wait}copy.write_row({})",
                query.constant,
                rows()
            );
            ("None".into(), body)
        }
        (Command::CopyFrom | Command::Exec, Some(_)) => {
            let body = format!(
                "{cursor}\n{wait}cur.executemany({}, [{} for r in rows])",
                query.constant,
                rows()
            );
            ("None".into(), body)
        }
        // A batch: every row's statement sent at once (psycopg pipelines
        // `executemany`), then each one's result read in turn.
        (Command::One(_) | Command::Many(_), Some(_)) => {
            let (returns, read) = read(driver, query, "result", wait);
            let each = if asynchronous { "async for" } else { "for" };
            let body = format!(
                "{cursor}\n{wait}cur.executemany({}, [{} for r in rows], returning=True)\n{}",
                query.constant,
                rows(),
                super::per_row(&read, &format!("{each} result in cur.results()"), &returns),
            );
            (format!("list[{returns}]"), body)
        }
        (Command::Exec, None) => ("None".into(), run("")),
        (Command::ExecRows, _) if driver.execute_counts => {
            ("int".into(), format!("{cursor}\nreturn {execute}"))
        }
        (Command::ExecRows, _) => ("int".into(), run("return cur.rowcount")),
        (Command::ExecLastId, _) => ("int".into(), run("return cur.lastrowid")),
        (Command::ExecResult, _) => (driver.cursor_type[at].into(), run("return cur")),
        (Command::CopyFrom, None) => unreachable!("a :copyfrom takes rows"),
        (Command::One(_) | Command::Many(_), None) => {
            let (returns, read) = read(driver, query, "cur", wait);
            (returns, run(&read))
        }
    };
    (returns, body.trim_end().to_string())
}

/// What a `:one` or `:many` returns, and the code returning it from the
/// result `cur` holds.
fn read(driver: &Driver, query: &Query, cur: &str, wait: &str) -> (String, String) {
    match &query.command {
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
            let read = format!(
                "match {wait}{cur}.fetchmany(2):\n    case [row]:\n        {ret}\n    \
                 case []:\n        raise NoRowError\n    case _:\n        raise MultipleRowsError"
            );
            (returns, read)
        }
        Command::Many(shape) => {
            let (item, each) = match shape {
                Shape::Scalar(ty) => (
                    ty.annotation(Scope::Queries),
                    ty.read("row[0]", Scope::Queries, driver.text_arrays),
                ),
                shape => (class(shape), format!("{}.from_row(row)", class(shape))),
            };
            let read = format!("return [{each} for row in {wait}{cur}.fetchall()]");
            (format!("list[{item}]"), read)
        }
        _ => unreachable!("only a :one or a :many reads rows"),
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
