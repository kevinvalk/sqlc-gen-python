//! A DB-API driver (PEP 249), which takes parameters by position: psycopg's
//! raw cursors, which take sqlc's SQL as written, `$N` and all; and `sqlite3`
//! and PyMySQL, which take one placeholder per value, in the text's order.

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use crate::api::models::{Enum, Field};
use crate::api::queries::{Command, CopyInto, Query, Shape};
use crate::engine::Piece;
use crate::python::vocabulary::{
    CONN, COPY, CUR, LEN, MULTIPLE_ROWS_ERROR, NO_ROW_ERROR, R, RESULT, ROW, ROWS, Symbol, VALUE,
    VALUES,
};
use crate::python::{
    Each, Expression, Scope, ann_assign, assign, each, expression, for_, int, keyword, list,
    list_of, match_, name, none, pattern, raise, return_, starred, stmts, string, true_, tuple,
    with,
};

#[cfg(test)]
#[path = "dbapi_spec.rs"]
mod spec;

/// How a driver is written: its blocking functions, its coroutines if it has
/// them, and what it does with a statement.
pub struct Driver {
    pub sync: Flavor,
    pub asynchronous: Option<Flavor>,
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

/// A driver's blocking functions, or its coroutines: how each part of a
/// query's function is written with them.
pub struct Flavor {
    /// The annotation of the connection.
    pub connection: fn() -> ast::Expr,
    /// The cursor a statement runs on, from `conn`, as `with` enters it,
    /// which closes it after; and as `:execresult` returns it, open, and its
    /// type. Either is then set up with `setup`, if any.
    pub closing: fn() -> ast::Expr,
    pub cursor: fn() -> ast::Expr,
    pub cursor_type: fn() -> ast::Expr,
    pub setup: Option<fn() -> ast::Stmt>,
}

impl Driver {
    /// How blocking functions are written, or coroutines.
    pub fn flavor(&self, asynchronous: bool) -> &Flavor {
        match asynchronous {
            false => &self.sync,
            true => self
                .asynchronous
                .as_ref()
                .expect("checked by Backend::async_problem"),
        }
    }

    /// The parameter of each value a statement of `query` binds, in order:
    /// one per placeholder in the text's order for a driver that takes its
    /// own, or one per `$N`, by `N`.
    fn binds<'q>(&self, query: &'q Query) -> Vec<&'q Field> {
        let mut binds: Vec<(i32, &Field)> = Vec::new();
        for (at, piece) in query.sql.0.iter().enumerate() {
            let (Piece::Param { number, name, .. } | Piece::Slice { number, name, .. }) = piece
            else {
                continue;
            };
            let number = match self.placeholder {
                Some(_) => at as i32,
                None => *number,
            };
            if !binds.iter().any(|&(n, _)| n == number) {
                let param = query.params.iter().find(|param| param.name == *name);
                binds.push((number, param.expect("a placeholder is a parameter's")));
            }
        }
        binds.sort_by_key(|&(number, _)| number);
        binds.into_iter().map(|(_, param)| param).collect()
    }
}

/// sqlc's statement, as the driver takes it: as written, or with its
/// placeholders the driver's, and each `%` doubled if it formats with `%`.
/// Without parameters, a driver formats nothing, so `%` stays as written.
pub fn statement(driver: &Driver, query: &Query) -> ast::Expr {
    if driver.copies
        && let Some(into) = &query.copy_into
    {
        return string(&copy(into));
    }
    let Some(own) = driver.placeholder else {
        return string(&query.sql.text());
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
    string(&text)
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

/// The body of a query's function: one execution of its statement on a
/// cursor of its own, closed after, the result shaped by its command
/// (`docs/python-output.md` §6, "Functions"). A coroutine awaits what the
/// driver's coroutines return, and enters its cursor with `async with`.
pub fn body(driver: &Driver, query: &Query, asynchronous: bool) -> Vec<ast::Stmt> {
    let body = Body {
        driver,
        flavor: driver.flavor(asynchronous),
        query,
        asynchronous,
    };
    let returning = || [keyword("returning", true_())];
    let on_cursor = match (&query.command, query.takes.rows()) {
        (Command::Exec, true) if driver.copies && query.copy_into.is_some() => return body.copy(),
        // The cursor is the result, so it stays open.
        (Command::ExecResult, _) => {
            return stmts![
                assign(CUR, (body.flavor.cursor)()),
                body.setup(),
                body.counted(),
                return_(CUR),
            ];
        }
        (Command::Exec, true) => vec![expression(body.execute_many([]))],
        // A batch: every row's statement sent at once (psycopg pipelines
        // `executemany`), then each one's result read in turn.
        (Command::One(shape), true) => stmts![
            expression(body.execute_many(returning())),
            ann_assign(
                VALUES,
                super::returns(query, body.flavor.cursor_type),
                list(vec![]),
            ),
            for_(
                body.results(),
                vec![body.one(shape, RESULT, |value| {
                    expression(VALUES.attr("append").call([value]))
                })],
            ),
            return_(VALUES),
        ],
        (Command::Many(shape), true) => stmts![
            expression(body.execute_many(returning())),
            return_(list_of(body.many(shape, RESULT), body.results())),
        ],
        (Command::Exec, false) => vec![expression(body.execute())],
        (Command::ExecRows, _) if driver.execute_counts => vec![return_(body.execute())],
        (Command::ExecRows, _) => stmts![body.counted(), return_(CUR.attr("rowcount"))],
        (Command::ExecLastId, _) => {
            stmts![expression(body.execute()), return_(CUR.attr("lastrowid")),]
        }
        (Command::One(shape), false) => {
            stmts![expression(body.execute()), body.one(shape, CUR, return_),]
        }
        (Command::Many(shape), false) => {
            stmts![expression(body.execute()), return_(body.many(shape, CUR)),]
        }
    };
    let cursor = (body.flavor.closing)().entered_as(CUR);
    vec![with(
        asynchronous,
        vec![cursor],
        stmts![body.setup(), on_cursor],
    )]
}

/// What the body of one query's function is made of, on its driver.
struct Body<'a> {
    driver: &'a Driver,
    flavor: &'a Flavor,
    query: &'a Query,
    asynchronous: bool,
}

impl Body<'_> {
    /// What sets a new cursor up, if the driver needs it.
    fn setup(&self) -> Option<ast::Stmt> {
        self.flavor.setup.map(|setup| setup())
    }

    /// `cur.execute(statement, values)`: the statement, run once.
    fn execute(&self) -> ast::Expr {
        let binds = self.driver.binds(self.query);
        let values = (!binds.is_empty()).then(|| self.values(&binds));
        (CUR.attr("execute"))
            .call(std::iter::once(self.statement()).chain(values))
            .awaited(self.asynchronous)
    }

    /// `cur.executemany(statement, [values for r in rows])`: the statement,
    /// run for each row.
    fn execute_many(&self, keywords: impl IntoIterator<Item = ast::Keyword>) -> ast::Expr {
        let binds = self.driver.binds(self.query);
        let rows = list_of(self.values(&binds), each(R, ROWS));
        (CUR.attr("executemany"))
            .call_with([name(&self.query.constant), rows], keywords)
            .awaited(self.asynchronous)
    }

    /// [`Self::execute`], in a pipeline of its own if the driver counts a
    /// statement of a pipeline only once it syncs.
    fn counted(&self) -> ast::Stmt {
        let execute = expression(self.execute());
        match self.driver.pipelines {
            true => {
                let pipeline = CONN.attr("pipeline").call([]).entered();
                with(self.asynchronous, vec![pipeline], vec![execute])
            }
            false => execute,
        }
    }

    /// PostgreSQL's `COPY` of the rows: its cursor and the copy entered in
    /// one `with` (ruff's SIM117). Only psycopg copies, with no setup.
    fn copy(&self) -> Vec<ast::Stmt> {
        let columns: Vec<&Field> = self.query.params.iter().collect();
        let write = (COPY.attr("write_row"))
            .call([self.values(&columns)])
            .awaited(self.asynchronous);
        let copy = CUR.attr("copy").call([name(&self.query.constant)]);
        vec![with(
            self.asynchronous,
            vec![
                (self.flavor.closing)().entered_as(CUR),
                copy.entered_as(COPY),
            ],
            vec![for_(each(R, ROWS), vec![expression(write)])],
        )]
    }

    /// The statement, each slice's placeholder made one per item:
    /// `.replace(marker, ", ".join([placeholder] * len(items)) or no_rows)`.
    fn statement(&self) -> ast::Expr {
        let placeholder = self.driver.placeholder.unwrap_or("?");
        let mut sliced: Vec<&str> = Vec::new();
        let mut statement = name(&self.query.constant);
        for piece in &self.query.sql.0 {
            if let Piece::Slice { marker, name, .. } = piece
                && !sliced.contains(&name.as_str())
            {
                sliced.push(name);
                let count = LEN.call([super::value(self.query, name)]);
                let places = (string(", ").attr("join"))
                    .call([list(vec![string(placeholder)]).times(count)])
                    .or(string(self.driver.no_rows));
                let marker = string(&format!("{marker}{placeholder}"));
                statement = statement.attr("replace").call([marker, places]);
            }
        }
        statement
    }

    /// The values of `params`, as the tuple a statement binds, each read as
    /// the function takes it: a slice's items each a value of its own.
    fn values(&self, params: &[&Field]) -> ast::Expr {
        let values = (params.iter())
            .map(|param| {
                let value = super::value(self.query, &param.name);
                match self.query.sql.is_slice(&param.name) {
                    true => starred(value),
                    false => (self.driver.bind)(param, value),
                }
            })
            .collect();
        tuple(values)
    }

    /// `for result in cur.results()`: each row's result of a batch.
    fn results(&self) -> Each {
        each(RESULT, CUR.attr("results").call([])).asynchronous(self.asynchronous)
    }

    /// Every row of the result `cur` holds, as a list.
    fn many(&self, shape: &Shape, cur: Symbol) -> ast::Expr {
        let fetched = (cur.attr("fetchall").call([])).awaited(self.asynchronous);
        list_of(shape.read(ROW, self.driver.text_arrays), each(ROW, fetched))
    }

    /// The only row of the result `cur` holds, handed to `emit` (`return x`,
    /// `values.append(x)`): the first two rows tell one row from none, which
    /// is `None` or raises, and from several, which raise.
    fn one(&self, shape: &Shape, cur: Symbol, emit: fn(ast::Expr) -> ast::Stmt) -> ast::Stmt {
        let found = match shape {
            // A row's value is `Any`: the annotated local states the type the
            // value would leak.
            Shape::Scalar(ty) if !ty.converts() => vec![
                ann_assign(VALUE, ty.annotation(Scope::Queries), ROW.index(int(0))),
                emit(VALUE.into()),
            ],
            shape => vec![emit(shape.read(ROW, self.driver.text_arrays))],
        };
        let missing = match self.query.raises {
            true => raise(NO_ROW_ERROR),
            false => emit(none()),
        };
        let fetched = (cur.attr("fetchmany").call([int(2)])).awaited(self.asynchronous);
        match_(
            fetched,
            vec![
                (pattern::sequence(vec![pattern::capture(ROW)]), found),
                (pattern::sequence(vec![]), vec![missing]),
                (pattern::wildcard(), vec![raise(MULTIPLE_ROWS_ERROR)]),
            ],
        )
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
