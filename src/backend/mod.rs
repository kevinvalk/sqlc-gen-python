//! A backend only decides how a statement is written and executed and what a connection is, as what a query takes and
//! returns is sqlc's and the engine's to say. SQLAlchemy rewrites sqlc's SQL for its `text()`, while every other
//! backend is a [`Driver`] that takes it as written ([`dbapi`]).
//!
//! Every method is an exhaustive `match`, so for a new backend the compiler lists everything it has to answer.

mod dbapi;
mod psycopg;
mod pymysql;
pub mod sqlalchemy;
mod sqlite3;

#[cfg(test)]
mod spec;

use anyhow::{Result, bail};
use ruff_python_ast as ast;
use serde::Deserialize;

use crate::api::queries::{Command, Query, Shape, Takes};
use crate::engine::Engine;
use crate::python::vocabulary::{DATE, DATETIME, DECIMAL, INT, LIST, PARAMS, R, Symbol};
use crate::python::{Expression, Type, name, none};
use dbapi::Driver;
pub use dbapi::check_text_array;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// SQLAlchemy Core, on any driver it has a dialect for.
    SqlAlchemy,
    /// psycopg 3, on PostgreSQL.
    Psycopg,
    /// `sqlite3`, and `aiosqlite` for coroutines, on SQLite.
    Sqlite3,
    /// PyMySQL, and asyncmy for coroutines, on MySQL.
    Pymysql,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::SqlAlchemy => "sqlalchemy",
            Self::Psycopg => "psycopg",
            Self::Sqlite3 => "sqlite3",
            Self::Pymysql => "pymysql",
        }
    }

    pub fn runs(self, engine: Engine) -> bool {
        match self {
            Self::SqlAlchemy => true,
            Self::Psycopg => engine == Engine::PostgreSql,
            Self::Sqlite3 => engine == Engine::Sqlite,
            Self::Pymysql => engine == Engine::MySql,
        }
    }

    /// The Python type only the backend's driver decides for a type ([`Engine::type_name`]): a class of its own, or
    /// text where drivers disagree.
    pub fn scalar_type(self, engine: Engine, type_name: &str) -> Option<Type> {
        match (self, engine) {
            (Self::SqlAlchemy, Engine::PostgreSql) => sqlalchemy::scalar_type(type_name),
            (Self::Psycopg, Engine::PostgreSql) => psycopg::scalar_type(type_name),
            (Self::SqlAlchemy | Self::Psycopg | Self::Sqlite3 | Self::Pymysql, _) => None,
        }
    }

    fn driver(self) -> Option<&'static Driver> {
        match self {
            Self::SqlAlchemy => None,
            Self::Psycopg => Some(&psycopg::DRIVER),
            Self::Sqlite3 => Some(&sqlite3::DRIVER),
            Self::Pymysql => Some(&pymysql::DRIVER),
        }
    }

    /// Fails generation for what the backend cannot represent or run faithfully.
    pub fn check(self, engine: Engine, query: &Query) -> Result<()> {
        // A MySQL driver's `executemany` rewrites an `INSERT` into one statement of many rows, and only some versions
        // format what follows its `VALUES` list (PyMySQL from 1.2.4, not mysqlclient). So a `%` there, doubled for the
        // driver, could reach MySQL doubled, in a string too!
        if engine == Engine::MySql && query.takes.rows() && query.sql.text().contains('%') {
            bail!(
                ":copyfrom has a % in its statement, which a MySQL driver's executemany doubles \
                 or not by its version; write it without one, as MOD(a, b) for a % b"
            )
        }
        match self.driver() {
            None => sqlalchemy::check(engine, query),
            Some(_) => Ok(()),
        }
    }

    /// Whether the library returns an array of enums as the array's text, which the enum's class then parses
    /// (`from_array`).
    pub fn text_arrays(self) -> bool {
        self.driver().is_some_and(|driver| driver.text_arrays)
    }

    /// A driver has no errors for a `:one` to raise, so the package defines its own. We always define both, so what a
    /// package exports does not change with its queries.
    pub fn defines_errors(self) -> bool {
        self.driver().is_some()
    }

    pub fn prelude(self, asynchronous: bool) -> Vec<ast::Stmt> {
        match self.driver() {
            None => Vec::new(),
            Some(driver) => dbapi::prelude(driver, asynchronous),
        }
    }

    pub fn statement(self, query: &Query) -> ast::Expr {
        match self.driver() {
            None => sqlalchemy::statement(query),
            Some(driver) => dbapi::statement(driver, query),
        }
    }

    /// The annotation of the connection a function takes.
    pub fn connection(self, asynchronous: bool) -> ast::Expr {
        match self.driver() {
            None => sqlalchemy::connection(asynchronous),
            Some(driver) => driver.flavor(asynchronous).connection_type(),
        }
    }

    pub fn returns(self, query: &Query, asynchronous: bool) -> ast::Expr {
        match self.driver() {
            None => returns(query, sqlalchemy::result),
            Some(driver) => returns(query, driver.flavor(asynchronous).cursor_type),
        }
    }

    pub fn body(self, query: &Query, asynchronous: bool) -> Vec<ast::Stmt> {
        match self.driver() {
            None => sqlalchemy::body(query, asynchronous),
            Some(driver) => dbapi::body(driver, query, asynchronous),
        }
    }

    /// The body of a `:many`'s [stream](Query::stream) `name`, which yields an iterator of its rows of `shape`.
    pub fn stream(self, query: &Query, (name, shape): (&str, &Shape), asynchronous: bool) -> Vec<ast::Stmt> {
        match self.driver() {
            None => sqlalchemy::stream(query, shape, asynchronous),
            Some(driver) => dbapi::stream(driver, query, (name, shape), asynchronous),
        }
    }
}

/// What one run of the query's command returns, or a list of that for a batch of rows. sqlc decides it, except for an
/// `:execresult`, which returns the library's own `result`.
fn returns(query: &Query, result: fn() -> ast::Expr) -> ast::Expr {
    let run = match &query.command {
        Command::Exec => return none(),
        Command::CopyFrom { .. } => return INT.into(),
        Command::ExecRows | Command::ExecLastId => INT.into(),
        Command::ExecResult => result(),
        Command::One(shape) => shape.annotation(!query.raises),
        // Eager, as a lazy generator would leave an `UPDATE ... RETURNING` unrun until someone iterates it
        Command::Many(shape) => LIST.index(shape.annotation(false)),
    };
    match query.takes.rows() {
        true => LIST.index(run),
        false => run,
    }
}

/// PostgreSQL's range types with the type of their bounds and SQLAlchemy's PostgreSQL type, which turns each driver's
/// range into SQLAlchemy's `Range`. psycopg returns its own `Range`.
const RANGES: &[(&str, Symbol, &str)] = &[
    ("int4range", INT, "INT4RANGE"),
    ("int8range", INT, "INT8RANGE"),
    ("numrange", DECIMAL, "NUMRANGE"),
    ("daterange", DATE, "DATERANGE"),
    ("tsrange", DATETIME, "TSRANGE"),
    ("tstzrange", DATETIME, "TSTZRANGE"),
];

fn range(name: &str) -> Option<(Symbol, &'static str)> {
    let (_, bound, sql_type) = RANGES.iter().find(|(range, ..)| *range == name)?;
    Some((*bound, sql_type))
}

/// A parameter as the function reads it: itself, a field of the one record, or of the row.
fn value(query: &Query, parameter: &str) -> ast::Expr {
    match query.takes {
        Takes::Keywords => name(parameter),
        Takes::Record(_) => PARAMS.attr(parameter),
        Takes::Rows(_) => R.attr(parameter),
    }
}
