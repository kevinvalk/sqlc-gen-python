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
}

impl Backend {
    /// The backend's name, as the `backend` option spells it.
    pub fn name(self) -> &'static str {
        match self {
            Self::SqlAlchemy => "sqlalchemy",
            Self::Psycopg => "psycopg",
            Self::Sqlite3 => "sqlite3",
        }
    }

    /// Whether the backend runs SQL on `engine`.
    pub fn runs(self, engine: Engine) -> bool {
        match self {
            Self::SqlAlchemy => true,
            Self::Psycopg => engine == Engine::PostgreSql,
            Self::Sqlite3 => engine == Engine::Sqlite,
        }
    }

    /// The driver the backend is, unless it is SQLAlchemy.
    fn driver(self) -> Option<&'static Driver> {
        match self {
            Self::SqlAlchemy => None,
            Self::Psycopg => Some(&psycopg::DRIVER),
            Self::Sqlite3 => Some(&sqlite3::DRIVER),
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
            Some(_) => dbapi::statement(query),
        }
    }

    /// The annotation of the connection a function takes.
    pub fn connection(self, asynchronous: bool) -> &'static str {
        match self.driver() {
            None => sqlalchemy::connection(asynchronous),
            Some(driver) => driver.connection[usize::from(asynchronous)],
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

/// The class a row of `shape` becomes, as a query module writes it.
fn class(shape: &Shape) -> String {
    match shape {
        Shape::Model(model) => format!("models.{model}"),
        Shape::Row(class, _) => class.clone(),
        Shape::Scalar(_) => unreachable!("a scalar is returned bare"),
    }
}
