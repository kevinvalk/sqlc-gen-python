//! A DB-API driver (PEP 249) takes parameters by position. psycopg's raw cursors take sqlc's SQL as written, `$N` and
//! all, while `sqlite3` and PyMySQL take one placeholder per value in the text's order.

use anyhow::{Result, bail};
use ruff_python_ast as ast;

use crate::api::models::{Enum, Field};
use crate::api::queries::{Command, CopyInto, Query, Shape};
use crate::engine::Piece;
use crate::python::vocabulary::{
    ANY, ASYNC_ITERATOR, CONNECTION, CONNECTION_TYPE, COPY, CUR, CURSOR, INT, LEN, MODELS, MULTIPLE_ROWS_ERROR,
    NO_ROW_ERROR, PROTOCOL, R, RESULT, ROW, ROWS, SELF, SEQUENCE, STR, Symbol, TYPE_CHECKING, TYPED, TYPED_CURSOR,
    VALUE, VALUES,
};
use crate::python::{
    Each, Expression, Function, Scope, ann_assign, assign, class, each, ellipsis, expression, for_, generator_of,
    if_else, int, keyword, list, list_of, match_, name, none, parameter, pattern, raise, receiver, return_, starred,
    stmts, string, true_, tuple, with, yield_,
};

#[cfg(test)]
#[path = "dbapi_spec.rs"]
mod spec;

pub struct Driver {
    pub sync: Flavor,
    pub asynchronous: Flavor,
    /// The driver's own placeholders, if it does not take sqlc's numbered ones.
    pub placeholders: Option<Placeholders>,
    /// Whether `execute` returns the count of rows it affected, which we prefer as the cursor's `rowcount` is not typed
    /// as one.
    pub execute_counts: bool,
    /// Whether a count must be read in a pipeline of its own, as psycopg only counts a statement in a pipeline once the
    /// pipeline syncs.
    pub pipelines: bool,
    /// A parameter's value, from the expression holding it.
    pub bind: fn(&Field, ast::Expr) -> ast::Expr,
    /// Whether an array of enums comes back as the array's text.
    pub text_arrays: bool,
    /// Whether a `:copyfrom` runs PostgreSQL's `COPY` through the cursor's `copy()` rather than one `INSERT` per row.
    pub copies: bool,
    /// What makes the cursor's rows tuples, for a driver where making the cursor does not.
    pub plain_rows: Option<fn() -> ast::Stmt>,
}

/// One placeholder per value, in the text's order.
pub struct Placeholders {
    pub text: &'static str,
    /// Whether the driver formats a statement that has parameters with `%`, so every other `%` must be doubled.
    pub formats: bool,
    /// A subquery of no rows for a `sqlc.slice` of none, so `x IN (...)` is false and `x NOT IN (...)` true, as of an
    /// empty list.
    pub no_rows: &'static str,
}

/// A driver's blocking functions, or its coroutines.
pub struct Flavor {
    pub connection: fn() -> ast::Expr,
    /// Whether the connection's class is only generic in the library's stubs, over the class of its cursors (PyMySQL).
    /// Bare, a type checker takes it for a connection of the default cursor only, and subscripted it raises at runtime
    /// once imported or once its annotation is read. So generated code names it for both ([`prelude`]).
    pub generic: bool,
    /// The cursor a statement runs on, as `with` enters it to close it after, as an `:execresult` returns it (open),
    /// and its type.
    pub closing: fn() -> ast::Expr,
    pub cursor: fn() -> ast::Expr,
    pub cursor_type: fn() -> ast::Expr,
    /// The cursor a stream iterates, as `with` enters it, of the name it is given: one that reads each row from the
    /// connection as it is iterated rather than all of them on `execute`.
    pub streaming: fn(&str) -> ast::Expr,
    /// Whether the library's stubs leave `execute`, `executemany` and `fetchmany` untyped, so strict pyright rejects a
    /// call of them and generated code calls them through a protocol of its own ([`prelude`]). asyncmy's do, see
    /// "Workarounds" in AGENTS.md.
    pub untyped: bool,
}

impl Flavor {
    pub fn connection_type(&self) -> ast::Expr {
        match self.generic {
            true => CONNECTION_TYPE.into(),
            false => (self.connection)(),
        }
    }
}

impl Driver {
    pub fn flavor(&self, asynchronous: bool) -> &Flavor {
        match asynchronous {
            false => &self.sync,
            true => &self.asynchronous,
        }
    }

    /// The parameter of each value a statement binds, in order: one per placeholder in the text's order for a driver
    /// with its own placeholders, or else one per `$N`, by `N`.
    fn binds<'q>(&self, query: &'q Query) -> Vec<&'q Field> {
        let mut binds: Vec<(i32, &Field)> = Vec::new();
        for (index, piece) in query.sql.0.iter().enumerate() {
            let (Piece::Param { number, name, .. } | Piece::Slice { number, name, .. }) = piece else {
                continue;
            };
            let number = match self.placeholders {
                Some(_) => index as i32,
                None => *number,
            };
            if !binds.iter().any(|&(bound, _)| bound == number) {
                let parameter = query.params.iter().find(|parameter| parameter.name == *name);
                binds.push((number, parameter.expect("a placeholder is a parameter's")));
            }
        }
        binds.sort_by_key(|&(number, _)| number);
        binds.into_iter().map(|(_, parameter)| parameter).collect()
    }
}

/// sqlc's statement as written, or with the driver's own placeholders and each `%` doubled if it formats with `%`.
pub fn statement(driver: &Driver, query: &Query) -> ast::Expr {
    if driver.copies
        && let Some(into) = &query.copy_into
    {
        return string(&copy(into));
    }
    let Some(placeholders) = &driver.placeholders else {
        return string(&query.sql.text());
    };
    let formats = placeholders.formats && !query.params.is_empty(); // Without parameters a driver formats nothing
    let text: String = (query.sql.0.iter())
        .map(|piece| match piece {
            Piece::Text(text) if formats => text.replace('%', "%%"),
            Piece::Text(text) => text.clone(),
            Piece::Param { .. } => placeholders.text.into(),
            // A slice keeps its marker, which the function replaces
            Piece::Slice { marker, .. } => format!("{marker}{}", placeholders.text),
        })
        .collect();
    string(&text)
}

/// We quote every name, as sqlc reports the name PostgreSQL stores.
fn copy(into: &CopyInto) -> String {
    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    let table = match into.schema.as_str() {
        "" => quote(&into.table),
        schema => format!("{}.{}", quote(schema), quote(&into.table)),
    };
    let columns: Vec<String> = into.columns.iter().map(|column| quote(column)).collect();
    format!("COPY {table} ({}) FROM STDIN", columns.join(", "))
}

/// What a query module defines before its queries, for a library whose stubs need it. Where the connection's class is
/// [`Flavor::generic`]:
///
/// ```python
/// if typing.TYPE_CHECKING:
///     _Connection = pymysql.Connection[Any]
/// else:
///     _Connection = pymysql.Connection
/// ```
///
/// Where the cursor's methods are [`Flavor::untyped`]:
///
/// ```python
/// class _Cursor(typing.Protocol):
///     async def execute(self, query: str, args: Sequence[Any] | None = None) -> int: ...
///     async def executemany(self, query: str, args: Sequence[Sequence[Any]]) -> int | None: ...
///     async def fetchmany(self, size: int) -> Sequence[Sequence[Any]]: ...
///     def __aiter__(self) -> AsyncIterator[Sequence[Any]]: ...
///
/// def _typed(cursor: asyncmy.cursors.Cursor) -> _Cursor:
///     return cursor
/// ```
pub fn prelude(driver: &Driver, asynchronous: bool) -> Vec<ast::Stmt> {
    let flavor = driver.flavor(asynchronous);
    let connection = flavor.generic.then(|| {
        let class = (flavor.connection)();
        if_else(
            TYPE_CHECKING,
            vec![assign(CONNECTION_TYPE, class.clone().index(ANY))],
            vec![assign(CONNECTION_TYPE, class)],
        )
    });
    stmts![connection, (flavor.untyped).then(|| bridge(flavor, asynchronous)),]
}

fn bridge(flavor: &Flavor, asynchronous: bool) -> Vec<ast::Stmt> {
    let method = |name: &str, parameters: Vec<ast::ParameterWithDefault>, returns: ast::Expr| {
        Function {
            parameters: std::iter::once(receiver(SELF)).chain(parameters).collect(),
            body: vec![expression(ellipsis())],
            is_async: asynchronous,
            ..Function::new(name, returns)
        }
        .into()
    };
    let values = || SEQUENCE.index(ANY);
    // Without parameters a statement runs as written, so `args` is optional
    let mut args = parameter("args", values().or_none());
    args.default = Some(Box::new(none()));
    let iterate = Function {
        parameters: vec![receiver(SELF)],
        body: vec![expression(ellipsis())],
        ..Function::new("__aiter__", ASYNC_ITERATOR.index(values()))
    };
    let methods: [ast::Stmt; 4] = [
        method("execute", vec![parameter("query", STR), args], INT.into()),
        method(
            "executemany",
            vec![parameter("query", STR), parameter("args", SEQUENCE.index(values()))],
            INT.or_none(),
        ),
        method("fetchmany", vec![parameter("size", INT)], SEQUENCE.index(values())),
        iterate.into(),
    ];
    let typed = Function {
        parameters: vec![parameter(CURSOR, (flavor.cursor_type)())],
        body: vec![return_(CURSOR)],
        ..Function::new(TYPED, TYPED_CURSOR)
    };
    vec![class(TYPED_CURSOR, [], [PROTOCOL.into()], methods), typed.into()]
}

/// One execution of the statement on a cursor of its own, which is closed after, with the result shaped by the command.
pub fn body(driver: &Driver, query: &Query, asynchronous: bool) -> Vec<ast::Stmt> {
    let body = Body::new(driver, query, asynchronous);
    let returning = || [keyword("returning", true_())];
    let on_cursor = match (&query.command, query.takes.rows()) {
        (Command::CopyFrom { .. }, _) if driver.copies && query.copy_into.is_some() => return body.copy(),
        // A MySQL driver counts what `executemany` wrote, but returns `None` for no rows
        (Command::CopyFrom { .. }, _) if driver.execute_counts => vec![return_(body.execute_many([]).or(int(0)))],
        (Command::CopyFrom { .. }, _) => stmts![body.counted(body.execute_many([])), return_(CUR.attr("rowcount"))],
        // The cursor is the result, so it stays open
        (Command::ExecResult, _) => {
            return stmts![
                assign(CUR, (body.flavor.cursor)()),
                body.plain_rows(),
                body.counted(body.execute()),
                return_(CUR),
            ];
        }
        (Command::Exec, true) => vec![expression(body.execute_many([]))],
        // We send every row's statement at once (psycopg pipelines `executemany`), then read each one's result in turn
        (Command::One(shape), true) => stmts![
            expression(body.execute_many(returning())),
            ann_assign(VALUES, super::returns(query, body.flavor.cursor_type), list(vec![]),),
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
        (Command::ExecRows, _) => stmts![body.counted(body.execute()), return_(CUR.attr("rowcount"))],
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
    vec![with(asynchronous, vec![cursor], stmts![body.plain_rows(), on_cursor])]
}

/// The rows of a `:many`, read from a cursor of its own as the caller iterates them, which is closed after:
///
/// ```python
/// with contextlib.closing(connection.cursor()) as cur:
///     cur.execute(_LIST_AUTHORS, (name,))
///     yield (models.Author.from_row(row) for row in cur)
/// ```
pub fn stream(driver: &Driver, query: &Query, (name, shape): (&str, &Shape), asynchronous: bool) -> Vec<ast::Stmt> {
    let body = Body::new(driver, query, asynchronous);
    let rows = generator_of(shape.read(ROW), each(ROW, body.typed(CUR)).asynchronous(asynchronous));
    let cursor = (body.flavor.streaming)(name).entered_as(CUR);
    vec![with(
        asynchronous,
        vec![cursor],
        stmts![body.plain_rows(), expression(body.execute()), yield_(rows)],
    )]
}

struct Body<'a> {
    driver: &'a Driver,
    flavor: &'a Flavor,
    query: &'a Query,
    asynchronous: bool,
}

impl<'a> Body<'a> {
    fn new(driver: &'a Driver, query: &'a Query, asynchronous: bool) -> Self {
        Self {
            driver,
            flavor: driver.flavor(asynchronous),
            query,
            asynchronous,
        }
    }

    /// Only when the driver needs it and the rows are read, by the function or from the cursor it returns.
    fn plain_rows(&self) -> Option<ast::Stmt> {
        let read = matches!(
            self.query.command,
            Command::One(_) | Command::Many(_) | Command::ExecResult
        );
        self.driver.plain_rows.filter(|_| read).map(|rows| rows())
    }

    /// The cursor viewed as typed, where the library's stubs are not.
    fn typed(&self, cur: Symbol) -> ast::Expr {
        match self.flavor.untyped {
            true => TYPED.call([cur.into()]),
            false => cur.into(),
        }
    }

    /// `cur.execute(statement, values)`
    fn execute(&self) -> ast::Expr {
        let binds = self.driver.binds(self.query);
        let values = (!binds.is_empty()).then(|| self.values(&binds));
        (self.typed(CUR).attr("execute"))
            .call(std::iter::once(self.statement()).chain(values))
            .awaited(self.asynchronous)
    }

    /// `cur.executemany(statement, [values for r in rows])`
    fn execute_many(&self, keywords: impl IntoIterator<Item = ast::Keyword>) -> ast::Expr {
        let binds = self.driver.binds(self.query);
        let rows = list_of(self.values(&binds), each(R, ROWS));
        (self.typed(CUR).attr("executemany"))
            .call_with([name(&self.query.constant), rows], keywords)
            .awaited(self.asynchronous)
    }

    /// `execute`, in a pipeline of its own if the driver only counts a statement of a pipeline once it syncs.
    fn counted(&self, execute: ast::Expr) -> ast::Stmt {
        let execute = expression(execute);
        match self.driver.pipelines {
            true => {
                let pipeline = CONNECTION.attr("pipeline").call([]).entered();
                with(self.asynchronous, vec![pipeline], vec![execute])
            }
            false => execute,
        }
    }

    /// We read the count `COPY` reports while the cursor is open, as closing it forgets it.
    fn copy(&self) -> Vec<ast::Stmt> {
        let columns: Vec<&Field> = self.query.params.iter().collect();
        let write = (COPY.attr("write_row"))
            .call([self.values(&columns)])
            .awaited(self.asynchronous);
        let copy = CUR.attr("copy").call([name(&self.query.constant)]);
        let copied = with(
            self.asynchronous,
            vec![copy.entered_as(COPY)],
            vec![for_(each(R, ROWS), vec![expression(write)])],
        );
        vec![with(
            self.asynchronous,
            vec![(self.flavor.closing)().entered_as(CUR)],
            vec![copied, return_(CUR.attr("rowcount"))],
        )]
    }

    /// The statement with each slice's placeholder made one per item:
    /// `.replace(marker, ", ".join([placeholder] * len(items)) or no_rows)`
    fn statement(&self) -> ast::Expr {
        let mut statement = name(&self.query.constant);
        // Only an engine whose driver has its own placeholders has slices
        let Some(placeholders) = &self.driver.placeholders else {
            return statement;
        };
        let mut sliced: Vec<&str> = Vec::new();
        for piece in &self.query.sql.0 {
            if let Piece::Slice { marker, name, .. } = piece
                && !sliced.contains(&name.as_str())
            {
                sliced.push(name);
                let count = LEN.call([super::value(self.query, name)]);
                let places = (string(", ").attr("join"))
                    .call([list(vec![string(placeholders.text)]).times(count)])
                    .or(string(placeholders.no_rows));
                let marker = string(&format!("{marker}{}", placeholders.text));
                statement = statement.attr("replace").call([marker, places]);
            }
        }
        statement
    }

    /// The tuple a statement binds, where a slice's items are each a value of their own.
    fn values(&self, parameters: &[&Field]) -> ast::Expr {
        let values = (parameters.iter())
            .map(|parameter| {
                let value = super::value(self.query, &parameter.name);
                match self.query.sql.is_slice(&parameter.name) {
                    true => starred(value),
                    false => (self.driver.bind)(parameter, value),
                }
            })
            .collect();
        tuple(values)
    }

    /// `for result in cur.results()`
    fn results(&self) -> Each {
        each(RESULT, CUR.attr("results").call([])).asynchronous(self.asynchronous)
    }

    fn many(&self, shape: &Shape, cur: Symbol) -> ast::Expr {
        let fetched = (cur.attr("fetchall").call([])).awaited(self.asynchronous);
        list_of(shape.read(ROW), each(ROW, fetched))
    }

    /// The only row, handed to `emit` (`return x`, `values.append(x)`). We fetch two rows to tell one from none, which
    /// is `None` or raises, and from several, which always raise.
    fn one(&self, shape: &Shape, cur: Symbol, emit: fn(ast::Expr) -> ast::Stmt) -> ast::Stmt {
        let found = match shape {
            // A row's value is `Any`, so we annotate a local to state the type instead of leaking `Any`
            Shape::Scalar(ty) if !ty.converts() => vec![
                ann_assign(VALUE, ty.annotation(Scope::Queries), ROW.index(int(0))),
                emit(VALUE.into()),
            ],
            shape => vec![emit(shape.read(ROW))],
        };
        let missing = match self.query.raises {
            true => raise(MODELS.attr(NO_ROW_ERROR)),
            false => emit(none()),
        };
        let fetched = (self.typed(cur).attr("fetchmany"))
            .call([int(2)])
            .awaited(self.asynchronous);
        match_(
            fetched,
            vec![
                (pattern::sequence(vec![pattern::capture(ROW)]), found),
                (pattern::sequence(vec![]), vec![missing]),
                (pattern::wildcard(), vec![raise(MODELS.attr(MULTIPLE_ROWS_ERROR))]),
            ],
        )
    }
}

/// `from_array` splits an array's text at its commas, so for a driver of [`Driver::text_arrays`] we refuse a label that
/// PostgreSQL quotes there: an empty one, `NULL`, or one holding whitespace, a quote, a backslash, a brace or a comma.
pub fn check_text_array(sql_enum: &Enum) -> Result<()> {
    let quoted = |label: &str| {
        label.is_empty()
            || label.eq_ignore_ascii_case("null")
            || (label.chars()).any(|c| "{},\"\\ \t\n\r\x0b\x0c".contains(c))
    };
    if let Some(member) = sql_enum.members.iter().find(|member| quoted(&member.value)) {
        bail!(
            "psycopg returns an array of this enum as its text, in which PostgreSQL quotes the \
             label {:?}; rename the label, or use the sqlalchemy backend",
            member.value
        );
    }
    Ok(())
}
