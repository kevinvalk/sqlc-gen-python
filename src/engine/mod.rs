//! The database engines sqlc analyses queries for, as its `engine:` names
//! them. An engine says what its catalog's types are in Python and how its
//! SQL reads; it knows nothing of the library that will run the SQL.
//!
//! Each method is an exhaustive `match`, so a new engine is a new variant
//! and the compiler lists everything it has to answer.

mod mysql;
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
    MySql,
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
    /// Python names `name`. A `sqlc.slice` is `/*SLICE:name*/?`, whose `?`
    /// stands for as many as the list has items.
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
            "mysql" => Ok(Self::MySql),
            engine => {
                bail!("engine \"{engine}\" is not supported; only postgresql, mysql and sqlite are")
            }
        }
    }

    /// The engine's name, as sqlc's `engine:` spells it.
    pub fn name(self) -> &'static str {
        match self {
            Self::PostgreSql => "postgresql",
            Self::Sqlite => "sqlite",
            Self::MySql => "mysql",
        }
    }

    /// Placeholder `number`, as an error names it.
    pub fn placeholder(self, number: i32) -> String {
        match self {
            Self::PostgreSql => format!("${number}"),
            Self::Sqlite => format!("?{number}"),
            Self::MySql => format!("? #{number}"),
        }
    }

    /// `expr` cast to a type, as the engine writes it, for an error to suggest.
    pub fn cast(self, expr: &str) -> String {
        match self {
            Self::PostgreSql => format!("{expr}::<type>"),
            Self::Sqlite | Self::MySql => format!("CAST({expr} AS <type>)"),
        }
    }

    /// The Python type of `column`'s type, if the engine defines it and has
    /// driver evidence for it.
    pub fn scalar_type(self, column: &plugin::Column) -> Option<Type> {
        match self {
            Self::PostgreSql => postgresql::scalar_type(&column.data_type()),
            Self::Sqlite => sqlite::scalar_type(&column.data_type()),
            Self::MySql => mysql::scalar_type(column),
        }
    }

    /// Why `column`, of an enum of `labels`, is no enum, if it is not.
    pub fn enum_problem(self, column: &plugin::Column, labels: &[String]) -> Option<&'static str> {
        match self {
            Self::PostgreSql | Self::Sqlite => None,
            Self::MySql => mysql::enum_problem(column, labels),
        }
    }

    /// Whether the engine reports the id an `INSERT` generated
    /// (`:execlastid`). PostgreSQL and SQLite have `RETURNING` instead, and
    /// SQLite's last id is the connection's, which an `INSERT` that inserted
    /// nothing leaves at the previous one's.
    pub fn has_last_insert_id(self) -> bool {
        match self {
            Self::PostgreSql | Self::Sqlite => false,
            Self::MySql => true,
        }
    }

    /// Whether `data_type` is sqlc's guess rather than a type it read, if the
    /// queries were analysed on a `database` or else statically.
    pub fn is_guess(self, data_type: &str, database: bool) -> bool {
        match self {
            Self::PostgreSql => postgresql::is_guess(data_type, database),
            Self::Sqlite => sqlite::is_guess(data_type, database),
            Self::MySql => mysql::is_guess(data_type, database),
        }
    }

    /// Why an array of `data_type` cannot be a list, if it cannot.
    pub fn array_problem(self, data_type: &str) -> Option<&'static str> {
        match self {
            Self::PostgreSql => postgresql::array_problem(data_type),
            Self::Sqlite => sqlite::array_problem(data_type),
            Self::MySql => mysql::array_problem(data_type),
        }
    }

    /// sqlc's `text`, cut at its placeholders, each named as `params` name
    /// it; they must be exactly the parameters sqlc reported.
    pub fn sql(self, text: &str, params: &[(i32, &str)]) -> Result<Sql> {
        match self {
            Self::PostgreSql => postgresql::sql(text, params),
            Self::Sqlite => sqlite::sql(text, params),
            Self::MySql => mysql::sql(text, params),
        }
    }

    /// Whether `sql` is a `MERGE`, whose row count not every driver reports.
    pub fn merges(self, sql: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::merges(sql),
            Self::Sqlite => sqlite::merges(sql),
            Self::MySql => mysql::merges(sql),
        }
    }

    /// Whether `name` is one unquoted identifier to the engine.
    pub fn is_identifier(self, name: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::is_identifier(name),
            Self::Sqlite => sqlite::is_identifier(name),
            Self::MySql => mysql::is_identifier(name),
        }
    }
}

/// `chars` cut at its `placeholders`, each its range and the number of the
/// parameter it binds, which `params` names: they must be exactly the
/// parameters sqlc reported. Errors name a placeholder as `engine` does.
///
/// Either mismatch is an error: had a lexer taken a placeholder for text, the
/// driver would bind its own placeholder there, which is silently another
/// parameter's value.
fn cut(
    chars: &[char],
    placeholders: impl Iterator<Item = (Range<usize>, i32)>,
    params: &[(i32, &str)],
    engine: Engine,
) -> Result<Sql> {
    let placeholders: Vec<(Range<usize>, i32)> = placeholders.collect();
    if let Some((number, name)) =
        (params.iter()).find(|(number, _)| !placeholders.iter().any(|(_, found)| found == number))
    {
        bail!(
            "sqlc reported parameter {} (\"{name}\"), but the SQL has no placeholder for it outside strings and comments",
            engine.placeholder(*number)
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
        // A `sqlc.slice` is its marker comment and its placeholder.
        let before = text(from..range.start);
        let marker = (before.rfind("/*SLICE:")).filter(|&at| {
            before[at..].ends_with("*/") && !before[at + 2..before.len() - 2].contains("*/")
        });
        let (before, placeholder) = match marker {
            Some(at) => (
                before[..at].to_string(),
                before[at..].to_string() + &placeholder,
            ),
            None => (before, placeholder),
        };
        if !before.is_empty() {
            pieces.push(Piece::Text(before));
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
