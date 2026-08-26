//! The database access layer generated code is written against.
//!
//! This is the axis that varies *structurally*. An [`Engine`] only changes which
//! Python type a column maps to; a backend changes the shape of the generated
//! code — how a statement is executed, what a connection is, and what a
//! placeholder looks like. Query modules are built from these answers.
//!
//! Backends are a closed set, so selection is an enum that dispatches to a
//! module, exactly as [`Engine`] does.

mod sqlalchemy;

use serde::Deserialize;

use crate::engine::Engine;
use crate::python::Type;

#[cfg(test)]
mod spec;

/// A database access layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// SQLAlchemy Core, against `Connection` or `AsyncConnection`.
    ///
    /// Spelled out, because `snake_case` would otherwise ask users to write
    /// `sql_alchemy`, which is not what the library is called.
    #[default]
    #[serde(rename = "sqlalchemy")]
    SqlAlchemy,
}

impl Backend {
    /// The query text as this backend's driver must see it.
    ///
    /// sqlc emits the placeholder style of the *database*; a driver often wants
    /// a different one.
    pub fn sql(self, sql: &str, engine: Engine) -> String {
        match self {
            Self::SqlAlchemy => sqlalchemy::sql(sql, engine),
        }
    }

    /// The type of the connection a querier holds.
    pub fn connection(self, asynchronous: bool) -> Type {
        match self {
            Self::SqlAlchemy => sqlalchemy::connection(asynchronous),
        }
    }
}
