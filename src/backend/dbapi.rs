//! A DB-API driver (PEP 249), which takes parameters by position: psycopg's
//! raw cursors, which take sqlc's SQL as written, `$N` and all; and `sqlite3`
//! and PyMySQL, which take one placeholder per value, in the text's order.

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use super::{block, class};

use crate::api::models::{Enum, Field};
use crate::api::queries::{Command, CopyInto, Query, Shape};
use crate::engine::Piece;
use crate::python::{self, Scope};

#[cfg(test)]
#[path = "dbapi_spec.rs"]
mod spec;

/// How a driver is written: its blocking API, its coroutines' if it has
/// them, and what it does with a statement.
pub struct Driver {
    pub sync: Api,
    pub asynchronous: Option<Api>,
    /// The placeholder the driver takes for every value, in the text's
    /// order, if not sqlc's own numbered one.
    pub placeholder: Option<&'static str>,
    /// Whether the driver formats a statement with parameters with `%`, so
    /// that every other `%` is doubled.
    pub formats: bool,
    /// Whether `execute` returns the rows it affected, which is then the
    /// count: the cursor's `rowcount` is not typed as one.
    pub execute_counts: bool,
    /// Whether a count must be read in a pipeline of its own: psycopg counts
    /// a statement in a pipeline only once the pipeline syncs.
    pub pipelines: bool,
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

/// One of a driver's APIs, blocking or coroutines.
pub struct Api {
    /// The annotation of the connection.
    pub connection: &'static str,
    /// The cursor a statement runs on, from `conn`, as `with` enters it,
    /// which closes it after; and as `:execresult` returns it, open, and its
    /// type. Either is then set up with `setup`, if any.
    pub closing: &'static str,
    pub cursor: &'static str,
    pub cursor_type: &'static str,
    pub setup: Option<&'static str>,
}

impl Driver {
    /// The API of blocking functions, or of coroutines.
    pub fn api(&self, asynchronous: bool) -> &Api {
        match asynchronous {
            false => &self.sync,
            true => self
                .asynchronous
                .as_ref()
                .expect("checked by Backend::async_problem"),
        }
    }

    /// Each value a statement of `query` binds, in order, by its parameter's
    /// name: one per placeholder in the text's order for a driver that takes
    /// its own, or one per `$N`, by `N`.
    fn values<'q>(&self, query: &'q Query) -> Vec<&'q str> {
        let mut names: Vec<(i32, &str)> = Vec::new();
        for (at, piece) in query.sql.0.iter().enumerate() {
            match piece {
                Piece::Text(_) => {}
                Piece::Param { placeholder, name }
                | Piece::Slice {
                    placeholder, name, ..
                } => {
                    let number = match self.placeholder {
                        Some(_) => at as i32,
                        None => placeholder.trim_start_matches('$').parse().unwrap_or(0),
                    };
                    if !names.iter().any(|&(n, _)| n == number) {
                        names.push((number, name));
                    }
                }
            }
        }
        names.sort_by_key(|&(number, _)| number);
        names.into_iter().map(|(_, name)| name).collect()
    }
}

/// sqlc's statement, as the driver takes it: as written, or with its
/// placeholders the driver's, and each `%` doubled if it formats with `%`.
/// Without parameters, a driver formats nothing, so `%` stays as written.
pub fn statement(driver: &Driver, query: &Query) -> ast::Expr {
    if driver.copies
        && let Some(into) = &query.copy_into
    {
        return python::string(&copy(into));
    }
    let Some(own) = driver.placeholder else {
        return python::string(&query.sql.text());
    };
    let formats = driver.formats && !query.params.is_empty();
    let text: String = (query.sql.0.iter())
        .map(|piece| match piece {
            Piece::Text(text) if formats => text.replace('%', "%%"),
            Piece::Text(text) => text.clone(),
            // A slice keeps its marker, which the function replaces.
            Piece::Param { .. } => own.into(),
            Piece::Slice { marker, .. } => format!("{marker}{own}"),
        })
        .collect();
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
/// statement on a cursor of its own, closed after, the result shaped by its
/// command (`docs/python-output.md` §6, "Functions").
pub fn body(driver: &Driver, query: &Query, asynchronous: bool) -> (String, String) {
    let api = driver.api(asynchronous);
    let wait = if asynchronous { "await " } else { "" };
    let with = if asynchronous { "async with" } else { "with" };
    // The values as a tuple, each read from `of(name)`: a slice's items each
    // a value of its own.
    let tuple = |names: &[&str], of: &dyn Fn(&str) -> String| {
        let values: Vec<String> = (names.iter())
            .map(|&name| match query.sql.is_slice(name) {
                true => format!("*{}", of(name)),
                false => {
                    let param = query.params.iter().find(|p| p.name == name);
                    param.map_or_else(|| of(name), |param| (driver.bind)(param, &of(name)))
                }
            })
            .collect();
        match values.as_slice() {
            [value] => format!("({value},)"),
            values => format!("({})", values.join(", ")),
        }
    };
    let names = driver.values(query);
    // The statement, each slice's placeholder made one per item.
    let mut statement = query.constant.clone();
    let mut sliced: Vec<&str> = Vec::new();
    for piece in &query.sql.0 {
        if let Piece::Slice { marker, name, .. } = piece
            && !sliced.contains(&name.as_str())
        {
            sliced.push(name);
            let own = driver.placeholder.unwrap_or("?");
            statement += &format!(
                ".replace({}, \", \".join([{}] * len({name})) or {})",
                python::literal(&format!("{marker}{own}")),
                python::literal(own),
                python::literal(driver.no_rows),
            );
        }
    }
    let args = match names.is_empty() {
        true => String::new(),
        false => format!(", {}", tuple(&names, &|name| name.to_string())),
    };
    let execute = format!("{wait}cur.execute({statement}{args})");
    let rows = || tuple(&names, &|name| format!("r.{name}"));
    // The body on a cursor of its own, closed after.
    let on_cursor = |lines: Vec<String>| {
        let setup = api.setup.map(String::from);
        block(
            &format!("{with} {} as cur:", api.closing),
            setup.into_iter().chain(lines).collect(),
        )
    };

    let (returns, lines) = match (&query.command, &query.rows) {
        (Command::Exec, Some(_)) if driver.copies && query.copy_into.is_some() => {
            let names: Vec<&str> = query.params.iter().map(|p| p.name.as_str()).collect();
            let row = tuple(&names, &|name| format!("r.{name}"));
            // One `with` for both (ruff's SIM117); only psycopg copies, with
            // no setup.
            let header = format!(
                "{with} {} as cur, cur.copy({}) as copy:",
                api.closing, query.constant
            );
            let write = block(
                "for r in rows:",
                vec![format!("{wait}copy.write_row({row})")],
            );
            ("None".into(), block(&header, write))
        }
        (Command::Exec, Some(_)) => {
            let many = format!(
                "{wait}cur.executemany({}, [{} for r in rows])",
                query.constant,
                rows()
            );
            ("None".into(), on_cursor(vec![many]))
        }
        // A batch: every row's statement sent at once (psycopg pipelines
        // `executemany`), then each one's result read in turn.
        (Command::One(_) | Command::Many(_), Some(_)) => {
            let each = if asynchronous { "async for" } else { "for" };
            let each = format!("{each} result in cur.results()");
            let (returns, read) = read(driver, query, "result", wait, &|value| {
                format!("values.append({value})")
            });
            let mut lines = vec![format!(
                "{wait}cur.executemany({}, [{} for r in rows], returning=True)",
                query.constant,
                rows()
            )];
            match many(driver, query, "result", wait) {
                Some((_, item)) => lines.push(format!("return [{item} {each}]")),
                None => {
                    lines.push(format!("values: list[{returns}] = []"));
                    lines.extend(block(&format!("{each}:"), read));
                    lines.push("return values".into());
                }
            }
            (format!("list[{returns}]"), on_cursor(lines))
        }
        (Command::Exec, None) => ("None".into(), on_cursor(vec![execute])),
        (Command::ExecRows, _) if driver.execute_counts => {
            ("int".into(), on_cursor(vec![format!("return {execute}")]))
        }
        (Command::ExecRows, _) => {
            let mut lines = counted(driver, &execute, with);
            lines.push("return cur.rowcount".into());
            ("int".into(), on_cursor(lines))
        }
        (Command::ExecLastId, _) => (
            "int".into(),
            on_cursor(vec![execute, "return cur.lastrowid".into()]),
        ),
        // The cursor is the result, so it stays open.
        (Command::ExecResult, _) => {
            let mut lines = vec![format!("cur = {}", api.cursor)];
            lines.extend(api.setup.map(String::from));
            lines.extend(counted(driver, &execute, with));
            lines.push("return cur".into());
            (api.cursor_type.into(), lines)
        }
        (Command::One(_) | Command::Many(_), None) => {
            let (returns, read) = read(driver, query, "cur", wait, &|value| {
                format!("return {value}")
            });
            (
                returns,
                on_cursor(std::iter::once(execute).chain(read).collect()),
            )
        }
    };
    (returns, lines.join("\n"))
}

/// `execute`, run in a pipeline of its own if the driver counts a statement
/// of a pipeline only once it syncs.
fn counted(driver: &Driver, execute: &str, with: &str) -> Vec<String> {
    match driver.pipelines {
        true => block(&format!("{with} conn.pipeline():"), vec![execute.into()]),
        false => vec![execute.into()],
    }
}

/// A `:many`'s result, as one expression, from the result `cur` holds: what
/// it returns, and the expression.
fn many(driver: &Driver, query: &Query, cur: &str, wait: &str) -> Option<(String, String)> {
    let Command::Many(shape) = &query.command else {
        return None;
    };
    let (item, each) = match shape {
        Shape::Scalar(ty) => (
            ty.annotation(Scope::Queries),
            ty.read("row[0]", Scope::Queries, driver.text_arrays),
        ),
        shape => (class(shape), format!("{}.from_row(row)", class(shape))),
    };
    Some((
        format!("list[{item}]"),
        format!("[{each} for row in {wait}{cur}.fetchall()]"),
    ))
}

/// What a `:one` or `:many` returns, and the lines that read it from the
/// result `cur` holds and hand it to `emit` (`return x`, `values.append(x)`).
fn read(
    driver: &Driver,
    query: &Query,
    cur: &str,
    wait: &str,
    emit: &dyn Fn(&str) -> String,
) -> (String, Vec<String>) {
    if let Some((returns, value)) = many(driver, query, cur, wait) {
        return (returns, vec![emit(&value)]);
    }
    let Command::One(shape) = &query.command else {
        unreachable!("only a :one or a :many reads rows")
    };
    let (returns, found) = match shape {
        // A row's value is `Any`: the annotated local states the type the
        // value would leak.
        Shape::Scalar(ty) if !ty.converts() => {
            let annotation = ty.annotation(Scope::Queries);
            let found = vec![format!("value: {annotation} = row[0]"), emit("value")];
            (annotation, found)
        }
        Shape::Scalar(ty) => {
            let value = ty.read("row[0]", Scope::Queries, driver.text_arrays);
            (ty.annotation(Scope::Queries), vec![emit(&value)])
        }
        shape => (
            class(shape),
            vec![emit(&format!("{}.from_row(row)", class(shape)))],
        ),
    };
    let cases = [
        block("case [row]:", found),
        block("case []:", vec!["raise NoRowError".into()]),
        block("case _:", vec!["raise MultipleRowsError".into()]),
    ];
    let lines = block(&format!("match {wait}{cur}.fetchmany(2):"), cases.concat());
    (returns, lines)
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
