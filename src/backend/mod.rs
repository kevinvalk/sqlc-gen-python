//! A backend only decides how a statement is written and executed and what a connection is, as what a query takes and
//! returns is sqlc's and the engine's to say. SQLAlchemy rewrites sqlc's SQL for its `text()`, while every other
//! backend is a [`Driver`] that takes it as written ([`dbapi`]).

mod dbapi;
mod psycopg;
mod pymysql;
pub mod sqlalchemy;
mod sqlite3;

#[cfg(test)]
mod spec;

use std::collections::BTreeSet;

use anyhow::{Result, bail};
use ruff_python_ast as ast;
use serde::Deserialize;

use crate::api::queries::{Command, Query, Shape, Takes};
use crate::engine::Engine;
use crate::python::vocabulary::{
    CONNECTION, DATE, DATETIME, DECIMAL, INT, LIST, NAMES, PARAMS, R, REGISTER_TYPES, STR, Symbol,
};
use crate::python::{Expression, Function, Type, expression, name, none, parameter, string};
use dbapi::Driver;

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
        // driver, could reach MySQL doubled, in a string too! And none binds a parameter there: each raises a TypeError
        // for the values the list does not take.
        if engine == Engine::MySql && query.takes.rows() {
            let text = query.sql.text();
            if text.contains('%') {
                bail!(
                    ":copyfrom has a % in its statement, which a MySQL driver's executemany doubles \
                     or not by its version; write it without one, as MOD(a, b) for a % b"
                )
            }
            if engine.binds_on_duplicate(&text) {
                bail!(
                    ":copyfrom binds a parameter after ON DUPLICATE KEY UPDATE, where a MySQL driver's \
                     executemany formats none and raises; write the row's own value as VALUES(column), \
                     or use :exec, once per row"
                )
            }
        }
        match self.driver() {
            None => sqlalchemy::check(engine, query),
            Some(_) => Ok(()),
        }
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
        let body = match self.driver() {
            None => sqlalchemy::body(query, asynchronous),
            Some(driver) => dbapi::body(driver, query, asynchronous),
        };
        registering(query, asynchronous, body)
    }

    /// The body of a `:many`'s [stream](Query::stream) `name`, which yields an iterator of its rows of `shape`.
    pub fn stream(self, query: &Query, (name, shape): (&str, &Shape), asynchronous: bool) -> Vec<ast::Stmt> {
        let body = match self.driver() {
            None => sqlalchemy::stream(query, shape, asynchronous),
            Some(driver) => dbapi::stream(driver, query, (name, shape), asynchronous),
        };
        registering(query, asynchronous, body)
    }

    /// `_register_types`, which registers each type it is given on the connection unless it already is, so that psycopg
    /// loads an array of it as a list. Only a query module that reads such an array defines it.
    pub fn register_types(self, asynchronous: bool) -> ast::Stmt {
        let body = match self.driver() {
            None => sqlalchemy::register_types(asynchronous),
            Some(_) => psycopg::register_types(CONNECTION.into(), asynchronous),
        };
        Function {
            parameters: vec![parameter(CONNECTION, self.connection(asynchronous))],
            vararg: Some(parameter(NAMES, STR)),
            body: vec![body],
            is_async: asynchronous,
            ..Function::new(REGISTER_TYPES, none())
        }
        .into()
    }
}

/// `body`, after it registers the types whose arrays the query reads. That has to come first, as a cursor keeps the
/// types its connection had when it was made.
fn registering(query: &Query, asynchronous: bool, body: Vec<ast::Stmt>) -> Vec<ast::Stmt> {
    let types: BTreeSet<&str> = (query.columns.iter())
        .filter_map(|column| column.ty.registered_type.as_deref())
        .collect();
    if types.is_empty() {
        return body;
    }
    let arguments = std::iter::once(CONNECTION.into()).chain(types.into_iter().map(string));
    let register = expression(REGISTER_TYPES.call(arguments).awaited(asynchronous));
    std::iter::once(register).chain(body).collect()
}

/// What one run of the query's command returns, or a list of that for a batch of rows. sqlc decides it, except for an
/// `:execresult`, which returns the library's own `result`.
fn returns(query: &Query, result: fn() -> ast::Expr) -> ast::Expr {
    let run = match &query.command {
        Command::Exec => return none(),
        Command::CopyFrom => return INT.into(),
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
