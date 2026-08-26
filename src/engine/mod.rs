//! Mapping database column types to Python types.
//!
//! An engine is exactly one lookup: the type name sqlc reports, to a
//! [`python::Type`]. Nullability and arrays are not its concern — in Python
//! those are `Optional[T]` and `List[T]` whatever the engine — so they are
//! applied once, in [`crate::schema`].
//!
use anyhow::{Result, bail};

use crate::python::Type;

mod mysql;
mod postgresql;
mod sqlite;

#[cfg(test)]
mod spec;

/// A database engine sqlc can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    PostgreSql,
    MySql,
    Sqlite,
}

impl Engine {
    /// Parses the `engine:` value from a sqlc config block.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "postgresql" => Ok(Self::PostgreSql),
            "mysql" => Ok(Self::MySql),
            "sqlite" => Ok(Self::Sqlite),
            other => bail!("unsupported engine `{other}`"),
        }
    }

    /// The Python type for a column type, or `None` if this engine does not
    /// know the name — in which case the schema declared it, and
    /// [`crate::schema`] resolves it against the catalog instead.
    pub fn scalar_type(self, data_type: &str) -> Option<Type> {
        match self {
            Self::PostgreSql => postgresql::scalar_type(data_type),
            Self::MySql => mysql::scalar_type(data_type),
            Self::Sqlite => sqlite::scalar_type(data_type),
        }
    }
}
