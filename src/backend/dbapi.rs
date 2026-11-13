//! A DB-API driver (PEP 249), which takes parameters by position: psycopg's
//! raw cursors, which take sqlc's SQL as written, `$N` and all; and `sqlite3`
//! and PyMySQL, which take one placeholder per value, in the text's order.

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use super::class;

use crate::api::models::{Enum, Field, FieldType};
use crate::api::queries::{Command, CopyInto, Query, Shape};
use crate::engine::Piece;
use crate::python::{self, Scope, py_expr, py_stmt, py_stmts};

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
    pub bind: fn(&Field, ast::Expr) -> ast::Expr,
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
/// command (`docs/python-output.md` §6, "Functions"). Each template is the
/// coroutine's ([`python::coroutine`]).
pub fn body(driver: &Driver, query: &Query, asynchronous: bool) -> (ast::Expr, Vec<ast::Stmt>) {
    let api = driver.api(asynchronous);
    let t = |template: &str| python::coroutine(template, asynchronous);
    // The values as a tuple, each read from `of(name)`: a slice's items each
    // a value of its own.
    let tuple = |names: &[&str], of: &dyn Fn(&str) -> ast::Expr| {
        let values = (names.iter())
            .map(|&name| match query.sql.is_slice(name) {
                true => python::starred(of(name)),
                false => {
                    let param = query.params.iter().find(|p| p.name == name);
                    param.map_or_else(|| of(name), |param| (driver.bind)(param, of(name)))
                }
            })
            .collect();
        python::tuple(values)
    };
    // A parameter's value: itself, or its field of the one record.
    let own = |name: &str| match query.bundle {
        Some(_) => py_expr!("params.$name", name = name),
        None => python::name(name),
    };
    let of_row = |name: &str| py_expr!("r.$name", name = name);
    let names = driver.values(query);
    // The statement, each slice's placeholder made one per item.
    let mut statement = python::name(&query.constant);
    let mut sliced: Vec<&str> = Vec::new();
    for piece in &query.sql.0 {
        if let Piece::Slice { marker, name, .. } = piece
            && !sliced.contains(&name.as_str())
        {
            sliced.push(name);
            let placeholder = driver.placeholder.unwrap_or("?");
            statement = py_expr!(
                r#"$statement.replace($marker, ", ".join([$placeholder] * len($items)) or $none)"#,
                statement = statement,
                marker = python::string(&format!("{marker}{placeholder}")),
                placeholder = python::string(placeholder),
                items = own(name),
                none = python::string(driver.no_rows),
            );
        }
    }
    let values: Vec<ast::Expr> = match names.is_empty() {
        true => vec![],
        false => vec![tuple(&names, &own)],
    };
    let execute = py_expr!(
        &t("await cur.execute($statement, *$values)"),
        statement = statement,
        values = values,
    );
    let rows = || py_expr!("[$row for r in rows]", row = tuple(&names, &of_row));
    // The body on a cursor of its own, closed after.
    let on_cursor = |body: Vec<ast::Stmt>| {
        py_stmts!(
            &t("
                async with $cursor as cur:
                    $setup
                    $body
                "),
            cursor = python::expr(api.closing),
            setup = api.setup.map(python::stmt).into_iter().collect::<Vec<_>>(),
            body = body,
        )
    };
    let none = || python::expr("None");
    let int = || python::name("int");

    let (returns, body) = match (&query.command, &query.rows) {
        // One `with` for both (ruff's SIM117); only psycopg copies, with no
        // setup.
        (Command::Exec, Some(_)) if driver.copies && query.copy_into.is_some() => {
            let names: Vec<&str> = query.params.iter().map(|p| p.name.as_str()).collect();
            let copy = py_stmts!(
                &t("
                    async with $cursor as cur, cur.copy($statement) as copy:
                        for r in rows:
                            await copy.write_row($row)
                    "),
                cursor = python::expr(api.closing),
                statement = &query.constant,
                row = tuple(&names, &of_row),
            );
            (none(), copy)
        }
        (Command::Exec, Some(_)) => {
            let many = py_stmts!(
                &t("await cur.executemany($statement, $rows)"),
                statement = &query.constant,
                rows = rows(),
            );
            (none(), on_cursor(many))
        }
        // A batch: every row's statement sent at once (psycopg pipelines
        // `executemany`), then each one's result read in turn.
        (Command::One(_) | Command::Many(_), Some(_)) => {
            let mut body = py_stmts!(
                &t("await cur.executemany($statement, $rows, returning=True)"),
                statement = &query.constant,
                rows = rows(),
            );
            let returns = match many(driver, query, "result", asynchronous) {
                Some((returns, item)) => {
                    body.extend(py_stmts!(
                        &t("return [$item async for result in cur.results()]"),
                        item = item,
                    ));
                    returns
                }
                None => {
                    let (returns, read) = read(driver, query, "result", asynchronous, &|value| {
                        py_stmt!("values.append($value)", value = value)
                    });
                    body.extend(py_stmts!(
                        &t("
                            values: list[$returns] = []
                            async for result in cur.results():
                                $read
                            return values
                            "),
                        returns = returns.clone(),
                        read = read,
                    ));
                    returns
                }
            };
            (
                py_expr!("list[$returns]", returns = returns),
                on_cursor(body),
            )
        }
        (Command::Exec, None) => (none(), on_cursor(vec![python::expression(execute)])),
        (Command::ExecRows, _) if driver.execute_counts => (
            int(),
            on_cursor(py_stmts!("return $execute", execute = execute)),
        ),
        (Command::ExecRows, _) => {
            let mut body = counted(driver, execute, asynchronous);
            body.extend(py_stmts!("return cur.rowcount"));
            (int(), on_cursor(body))
        }
        (Command::ExecLastId, _) => {
            let body = py_stmts!(
                "
                $execute
                return cur.lastrowid
                ",
                execute = execute,
            );
            (int(), on_cursor(body))
        }
        // The cursor is the result, so it stays open.
        (Command::ExecResult, _) => {
            let mut body = py_stmts!("cur = $cursor", cursor = python::expr(api.cursor));
            body.extend(api.setup.map(python::stmt));
            body.extend(counted(driver, execute, asynchronous));
            body.extend(py_stmts!("return cur"));
            (python::expr(api.cursor_type), body)
        }
        (Command::One(_) | Command::Many(_), None) => {
            let (returns, read) = read(driver, query, "cur", asynchronous, &|value| {
                py_stmt!("return $value", value = value)
            });
            let body = std::iter::once(python::expression(execute))
                .chain(read)
                .collect();
            (returns, on_cursor(body))
        }
    };
    (returns, body)
}

/// `execute`, run in a pipeline of its own if the driver counts a statement
/// of a pipeline only once it syncs.
fn counted(driver: &Driver, execute: ast::Expr, asynchronous: bool) -> Vec<ast::Stmt> {
    match driver.pipelines {
        true => py_stmts!(
            &python::coroutine(
                "
                async with conn.pipeline():
                    $execute
                ",
                asynchronous,
            ),
            execute = execute,
        ),
        false => vec![python::expression(execute)],
    }
}

/// A `:many`'s result, as one expression, from the result `cur` holds: what
/// it returns, and the expression.
fn many(
    driver: &Driver,
    query: &Query,
    cur: &str,
    asynchronous: bool,
) -> Option<(ast::Expr, ast::Expr)> {
    let Command::Many(shape) = &query.command else {
        return None;
    };
    let (item, each) = match shape {
        Shape::Scalar(ty) => (
            ty.annotation(Scope::Queries),
            ty.read(py_expr!("row[0]"), Scope::Queries, driver.text_arrays),
        ),
        shape => (
            class(shape),
            py_expr!("$class.from_row(row)", class = class(shape)),
        ),
    };
    let all = py_expr!(
        &python::coroutine("[$each for row in await $cur.fetchall()]", asynchronous),
        each = each,
        cur = cur,
    );
    Some((py_expr!("list[$item]", item = item), all))
}

/// What a `:one` or `:many` returns, and the statements that read it from the
/// result `cur` holds and hand it to `emit` (`return x`, `values.append(x)`).
fn read(
    driver: &Driver,
    query: &Query,
    cur: &str,
    asynchronous: bool,
    emit: &dyn Fn(ast::Expr) -> ast::Stmt,
) -> (ast::Expr, Vec<ast::Stmt>) {
    if let Some((returns, value)) = many(driver, query, cur, asynchronous) {
        return (returns, vec![emit(value)]);
    }
    let Command::One(shape) = &query.command else {
        unreachable!("only a :one or a :many reads rows")
    };
    // Without a row, `None`, or `NoRowError`.
    let (missing, optional) = match query.raises {
        false => (vec![emit(python::expr("None"))], true),
        true => (py_stmts!("raise NoRowError"), false),
    };
    let (returns, found) = match shape {
        // A row's value is `Any`: the annotated local states the type the
        // value would leak.
        Shape::Scalar(ty) if !ty.converts() => {
            let annotation = ty.annotation(Scope::Queries);
            let returns = FieldType {
                optional: ty.optional || optional,
                ..ty.clone()
            }
            .annotation(Scope::Queries);
            let mut found = py_stmts!(
                "value: $annotation = row[0]",
                annotation = annotation.clone(),
            );
            found.push(emit(python::name("value")));
            (returns, found)
        }
        Shape::Scalar(ty) => {
            let value = ty.read(py_expr!("row[0]"), Scope::Queries, driver.text_arrays);
            let returns = FieldType {
                optional: ty.optional || optional,
                ..ty.clone()
            };
            (returns.annotation(Scope::Queries), vec![emit(value)])
        }
        shape => {
            let value = py_expr!("$class.from_row(row)", class = class(shape));
            let returns = match optional {
                true => py_expr!("$class | None", class = class(shape)),
                false => class(shape),
            };
            (returns, vec![emit(value)])
        }
    };
    let matched = py_stmts!(
        &python::coroutine(
            "
            match await $cur.fetchmany(2):
                case [row]:
                    $found
                case []:
                    $missing
                case _:
                    raise MultipleRowsError
            ",
            asynchronous,
        ),
        cur = cur,
        found = found,
        missing = missing,
    );
    (returns, matched)
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
