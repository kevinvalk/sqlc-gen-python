//! The database engines sqlc analyses queries for, as its `engine:` names
//! them. An engine says what its catalog's types are in Python and how its
//! SQL reads; it knows nothing of the library that will run the SQL.
//!
//! Each method is an exhaustive `match`, so a new engine is a new variant
//! and the compiler lists everything it has to answer.

mod postgresql;
mod sqlite;

#[cfg(test)]
mod spec;

use std::ops::Range;

use anyhow::{Result, bail};

use crate::plugin;
use crate::python::Type;

/// A database engine this plugin generates for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    PostgreSql,
    Sqlite,
}

/// A statement as sqlc wrote it, cut at its placeholders: what an engine reads
/// in it, for a backend to write in its own placeholder style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sql(pub Vec<Piece>);

/// A part of a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Text, as sqlc wrote it.
    Text(String),
    /// A placeholder as sqlc wrote it (`$1`, `?`, `?2`), of the parameter
    /// Python names `name`.
    Param { placeholder: String, name: String },
}

impl Sql {
    /// The statement as sqlc wrote it.
    pub fn text(&self) -> String {
        (self.0.iter())
            .map(|piece| match piece {
                Piece::Text(text)
                | Piece::Param {
                    placeholder: text, ..
                } => text.as_str(),
            })
            .collect()
    }
}

impl Engine {
    /// The engine the request's queries were analysed for.
    pub fn of(request: &plugin::GenerateRequest) -> Result<Self> {
        match request.settings.as_ref().map_or("", |s| s.engine.as_str()) {
            "postgresql" => Ok(Self::PostgreSql),
            "sqlite" => Ok(Self::Sqlite),
            engine => {
                bail!("engine \"{engine}\" is not supported yet; only postgresql and sqlite are")
            }
        }
    }

    /// The engine's name, as sqlc's `engine:` spells it.
    pub fn name(self) -> &'static str {
        match self {
            Self::PostgreSql => "postgresql",
            Self::Sqlite => "sqlite",
        }
    }

    /// `expr` cast to a type, as the engine writes it, for an error to suggest.
    pub fn cast(self, expr: &str) -> String {
        match self {
            Self::PostgreSql => format!("{expr}::<type>"),
            Self::Sqlite => format!("CAST({expr} AS <type>)"),
        }
    }

    /// The Python type of a column type the engine defines, with driver
    /// evidence, or `None`.
    pub fn scalar_type(self, data_type: &str) -> Option<Type> {
        match self {
            Self::PostgreSql => postgresql::scalar_type(data_type),
            Self::Sqlite => sqlite::scalar_type(data_type),
        }
    }

    /// Whether `data_type` is sqlc's guess rather than a type it read.
    pub fn is_guess(self, data_type: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::is_guess(data_type),
            Self::Sqlite => sqlite::is_guess(data_type),
        }
    }

    /// Why an array of `data_type` cannot be a list, if it cannot.
    pub fn array_problem(self, data_type: &str) -> Option<&'static str> {
        match self {
            Self::PostgreSql => postgresql::array_problem(data_type),
            Self::Sqlite => sqlite::array_problem(data_type),
        }
    }

    /// sqlc's `text`, cut at its placeholders, each named as `params` name
    /// it; they must be exactly the parameters sqlc reported.
    pub fn sql(self, text: &str, params: &[(i32, &str)]) -> Result<Sql> {
        match self {
            Self::PostgreSql => postgresql::sql(text, params),
            Self::Sqlite => sqlite::sql(text, params),
        }
    }

    /// Whether `sql` is a `MERGE`, whose row count not every driver reports.
    pub fn merges(self, sql: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::merges(sql),
            Self::Sqlite => sqlite::merges(sql),
        }
    }

    /// Whether `name` is one unquoted identifier to the engine.
    pub fn is_identifier(self, name: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::is_identifier(name),
            Self::Sqlite => sqlite::is_identifier(name),
        }
    }
}

/// `chars` cut at its `placeholders`, each its range and the number of the
/// parameter it binds, which `params` names: they must be exactly the
/// parameters sqlc reported. `spell` writes a number as the engine's
/// placeholder.
///
/// Either mismatch is an error: had a lexer taken a placeholder for text, the
/// driver would bind its own placeholder there, which is silently another
/// parameter's value.
fn cut(
    chars: &[char],
    placeholders: impl Iterator<Item = (Range<usize>, i32)>,
    params: &[(i32, &str)],
    spell: fn(i32) -> String,
) -> Result<Sql> {
    let placeholders: Vec<(Range<usize>, i32)> = placeholders.collect();
    if let Some((number, name)) =
        (params.iter()).find(|(number, _)| !placeholders.iter().any(|(_, found)| found == number))
    {
        bail!(
            "sqlc reported parameter {} (\"{name}\"), but the SQL has no placeholder for it outside strings and comments",
            spell(*number)
        );
    }
    let text = |range: Range<usize>| chars[range].iter().collect::<String>();
    let mut pieces = Vec::new();
    let mut from = 0;
    for (range, number) in placeholders {
        let placeholder = text(range.clone());
        let Some(&(_, name)) = params.iter().find(|(n, _)| *n == number) else {
            bail!("the SQL uses {placeholder}, which sqlc reported no parameter for");
        };
        if from < range.start {
            pieces.push(Piece::Text(text(from..range.start)));
        }
        from = range.end;
        pieces.push(Piece::Param {
            placeholder,
            name: name.into(),
        });
    }
    if from < chars.len() {
        pieces.push(Piece::Text(text(from..chars.len())));
    }
    Ok(Sql(pieces))
}
