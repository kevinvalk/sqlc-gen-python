//! The Python libraries generated code runs its SQL through. A backend says
//! which engines it runs, how a statement is written and executed, and what a
//! connection is; what a query takes and returns is sqlc's and the engine's to
//! say.
//!
//! SQLAlchemy rewrites sqlc's SQL for its `text()`. Every other backend is a
//! driver that takes it as written ([`dbapi`]), and is only a [`Driver`]:
//! how its connection, its cursor and a parameter are spelt.
//!
//! Each method is an exhaustive `match`, so a new backend is a new variant
//! and the compiler lists everything it has to answer.

mod dbapi;
mod psycopg;
mod pymysql;
pub mod sqlalchemy;
mod sqlite3;

#[cfg(test)]
mod spec;

use anyhow::Result;
use ruff_python_ast as ast;
use serde::Deserialize;

use crate::api::models::Enum;
use crate::api::queries::{Query, Shape};
use crate::engine::Engine;
use crate::python::Type;
use dbapi::Driver;

/// A library generated code runs its SQL through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// SQLAlchemy Core, on any driver it has a dialect for.
    SqlAlchemy,
    /// psycopg 3, on PostgreSQL.
    Psycopg,
    /// `sqlite3`, and `aiosqlite` for coroutines, on SQLite.
    Sqlite3,
    /// PyMySQL, on MySQL, without coroutines.
    Pymysql,
}

impl Backend {
    /// The backend's name, as the `backend` option spells it.
    pub fn name(self) -> &'static str {
        match self {
            Self::SqlAlchemy => "sqlalchemy",
            Self::Psycopg => "psycopg",
            Self::Sqlite3 => "sqlite3",
            Self::Pymysql => "pymysql",
        }
    }

    /// Whether the backend runs SQL on `engine`.
    pub fn runs(self, engine: Engine) -> bool {
        match self {
            Self::SqlAlchemy => true,
            Self::Psycopg => engine == Engine::PostgreSql,
            Self::Sqlite3 => engine == Engine::Sqlite,
            Self::Pymysql => engine == Engine::MySql,
        }
    }

    /// Why the backend has no coroutines, if it has none.
    pub fn async_problem(self) -> Option<&'static str> {
        match self {
            Self::SqlAlchemy | Self::Psycopg | Self::Sqlite3 => None,
            Self::Pymysql => Some(
                "no async MySQL driver is both typed and right, so the pymysql backend has no \
                 coroutines; use the sqlalchemy backend, on mysql+asyncmy",
            ),
        }
    }

    /// The Python type of a column of `data_type` that only the backend's
    /// driver decides: a class of its own, or text where drivers disagree.
    pub fn scalar_type(self, engine: Engine, data_type: &str) -> Option<Type> {
        match (self, engine) {
            (Self::SqlAlchemy, Engine::PostgreSql) => sqlalchemy::scalar_type(data_type),
            (Self::Psycopg, _) => psycopg::scalar_type(data_type),
            _ => None,
        }
    }

    /// The driver the backend is, unless it is SQLAlchemy.
    fn driver(self) -> Option<&'static Driver> {
        match self {
            Self::SqlAlchemy => None,
            Self::Psycopg => Some(&psycopg::DRIVER),
            Self::Sqlite3 => Some(&sqlite3::DRIVER),
            Self::Pymysql => Some(&pymysql::DRIVER),
        }
    }

    /// An error for what the backend cannot represent or run faithfully,
    /// asked while the API is decided, so that generation fails.
    pub fn check(self, engine: Engine, query: &Query) -> Result<()> {
        match self.driver() {
            None => sqlalchemy::check(engine, query),
            Some(_) => Ok(()),
        }
    }

    /// An error if the backend cannot read an array of `sql_enum`.
    pub fn check_array_enum(self, sql_enum: &Enum) -> Result<()> {
        match self.text_arrays() {
            true => dbapi::check_text_array(sql_enum),
            false => Ok(()),
        }
    }

    /// Whether the library returns an array of enums as the array's text,
    /// which the enum's class then parses (`from_array`).
    pub fn text_arrays(self) -> bool {
        self.driver().is_some_and(|driver| driver.text_arrays)
    }

    /// Whether a `:one` raises the package's own `NoRowError` and
    /// `MultipleRowsError`: a driver has no equivalent.
    pub fn defines_errors(self) -> bool {
        self.driver().is_some()
    }

    /// The value of a query's statement constant.
    pub fn statement(self, query: &Query) -> ast::Expr {
        match self.driver() {
            None => sqlalchemy::statement(query),
            Some(driver) => dbapi::statement(driver, query),
        }
    }

    /// The annotation of the connection a function takes.
    pub fn connection(self, asynchronous: bool) -> &'static str {
        match self.driver() {
            None => sqlalchemy::connection(asynchronous),
            Some(driver) => driver.api(asynchronous).connection,
        }
    }

    /// What a query's function returns, and its body.
    pub fn body(self, query: &Query, asynchronous: bool) -> (String, String) {
        match self.driver() {
            None => sqlalchemy::body(query, asynchronous),
            Some(driver) => dbapi::body(driver, query, asynchronous),
        }
    }
}

/// PostgreSQL's range types, each with the type of its bounds and
/// SQLAlchemy's class for it, which turns each driver's range into
/// SQLAlchemy's `Range`; psycopg returns its own `Range`.
const RANGES: &[(&str, &str, &str)] = &[
    ("int4range", "int", "postgresql.INT4RANGE"),
    ("int8range", "int", "postgresql.INT8RANGE"),
    ("numrange", "Decimal", "postgresql.NUMRANGE"),
    ("daterange", "dt.date", "postgresql.DATERANGE"),
    ("tsrange", "dt.datetime", "postgresql.TSRANGE"),
    ("tstzrange", "dt.datetime", "postgresql.TSTZRANGE"),
];

/// The range type `name`'s bounds' type and SQLAlchemy class, if it is one.
fn range(name: &str) -> Option<(&'static str, &'static str)> {
    let (_, bound, class) = RANGES.iter().find(|(range, ..)| *range == name)?;
    Some((bound, class))
}

/// `header`, then `body` indented under it.
fn block(header: &str, body: Vec<String>) -> Vec<String> {
    std::iter::once(header.to_string())
        .chain(body.into_iter().map(|line| format!("    {line}")))
        .collect()
}

/// The class a row of `shape` becomes, as a query module writes it.
fn class(shape: &Shape) -> String {
    match shape {
        Shape::Model(model) => format!("models.{model}"),
        Shape::Row(class, _) => class.clone(),
        Shape::Scalar(_) => unreachable!("a scalar is returned bare"),
    }
}
