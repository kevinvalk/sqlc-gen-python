//! The database engines sqlc analyses queries for, as its `engine:` names
//! them. An engine says what its catalog's types are in Python and how its
//! SQL reads; it knows nothing of the library that will run the SQL.
//!
//! Each method is an exhaustive `match`, so a new engine is a new variant
//! and the compiler lists everything it has to answer.

mod mysql;
mod postgresql;
pub mod select;
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
    /// Python names `name`.
    Param { placeholder: String, name: String },
    /// A `sqlc.slice`'s placeholder, after the marker sqlc writes before it
    /// (`/*SLICE:ids*/`): it stands for one placeholder per item of the list.
    Slice {
        marker: String,
        placeholder: String,
        name: String,
    },
}

/// A parameter sqlc reported, as [`Engine::sql`] binds it: its number, its
/// Python name, and, for a `sqlc.slice`, sqlc's name of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind<'a> {
    pub number: i32,
    pub name: &'a str,
    pub slice: Option<&'a str>,
}

/// Plain parameters, numbered and named, as a spec writes them.
#[cfg(test)]
pub fn binds<'a>(params: &[(i32, &'a str)]) -> Vec<Bind<'a>> {
    (params.iter())
        .map(|&(number, name)| Bind {
            number,
            name,
            slice: None,
        })
        .collect()
}

impl Sql {
    /// The statement as sqlc wrote it.
    pub fn text(&self) -> String {
        (self.0.iter())
            .map(|piece| match piece {
                Piece::Text(text)
                | Piece::Param {
                    placeholder: text, ..
                } => text.clone(),
                Piece::Slice {
                    marker,
                    placeholder,
                    ..
                } => format!("{marker}{placeholder}"),
            })
            .collect()
    }

    /// Whether the parameter Python names `name` is a `sqlc.slice`.
    pub fn is_slice(&self, name: &str) -> bool {
        (self.0.iter())
            .any(|piece| matches!(piece, Piece::Slice { name: slice, .. } if slice == name))
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
    /// queries were analysed on a `database` or else statically: on MySQL and
    /// SQLite, `any` for an expression it could not type, or nothing.
    pub fn is_guess(self, data_type: &str, database: bool) -> bool {
        match self {
            Self::PostgreSql => postgresql::is_guess(data_type, database),
            Self::Sqlite | Self::MySql => ["", "any"].contains(&data_type),
        }
    }

    /// Whether a result `column` is an expression sqlc typed by its form, not
    /// its operands: on MySQL and SQLite, arithmetic of any operands is `int`
    /// (`price * 1.1` too), and MySQL's `1.0`, a `DECIMAL`, is `float`. A
    /// cast is spelt as declared (`INTEGER`, `SIGNED`, `REAL`, `DOUBLE`).
    pub fn is_untyped_expression(self, column: &plugin::Column) -> bool {
        self != Self::PostgreSql
            && column.table.is_none()
            && ["int", "float"].contains(&column.data_type().as_str())
    }

    /// What to do about a column of `data_type` that has no Python type, for
    /// an error to suggest, if anything.
    pub fn unmapped(self, data_type: &str) -> &'static str {
        match self {
            // sqlc sends a domain's name, never its base type.
            Self::PostgreSql if !data_type.starts_with("pg_catalog.") => {
                ", or, for a domain, declare the column with its base type and a CHECK constraint"
            }
            Self::Sqlite => {
                "; but SQLite stores a value of its NUMERIC affinity as whichever of an integer, \
                 a real and text it looks like, so better declare it INTEGER, REAL, TEXT or BLOB"
            }
            _ => "",
        }
    }

    /// How else to have sqlc type a function's result, for an error to
    /// suggest, if there is a way.
    pub fn analyses(self) -> &'static str {
        match self {
            Self::PostgreSql => {
                "; or have sqlc analyse the queries on a database (database_analysis)"
            }
            Self::Sqlite | Self::MySql => "",
        }
    }

    /// Why an array of `data_type` cannot be a list, if it cannot.
    pub fn array_problem(self, data_type: &str) -> Option<&'static str> {
        match self {
            Self::PostgreSql => postgresql::array_problem(data_type),
            Self::Sqlite => Some("SQLite does not have"),
            Self::MySql => Some("MySQL does not have"),
        }
    }

    /// sqlc's `text`, cut at its placeholders, each named as `params` name
    /// it; they must be exactly the parameters sqlc reported.
    pub fn sql(self, text: &str, params: &[Bind]) -> Result<Sql> {
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
            // Neither has `MERGE`.
            Self::Sqlite | Self::MySql => false,
        }
    }

    /// What each of `columns` result columns of `text` is, by position,
    /// where certain ([`select::items`]).
    pub fn items(self, text: &str, columns: usize) -> Option<Vec<select::Item>> {
        select::items(&self.words(text), columns)
    }

    /// The tables `text`'s outer joins may leave without a row, lowercase;
    /// `None` for any ([`select::outer_joined`]).
    pub fn outer_joined(self, text: &str) -> Option<Vec<String>> {
        select::outer_joined(&self.words(text))
    }

    /// The text of each token of `text`'s code, comments left out.
    fn words(self, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        match self {
            Self::PostgreSql => postgresql::words(&chars),
            Self::Sqlite => sqlite::words(&chars),
            Self::MySql => mysql::words(&chars),
        }
    }

    /// The type of `count(...)`, whatever it counts, as sqlc names it on a
    /// type it read: PostgreSQL's `bigint`, MySQL's `BIGINT`, SQLite's
    /// integer.
    pub fn count_type(self) -> &'static str {
        match self {
            Self::PostgreSql => "pg_catalog.int8",
            Self::Sqlite => "integer",
            Self::MySql => "bigint",
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
/// parameters sqlc reported. A slice's placeholder must follow its marker.
/// Errors name a placeholder as `engine` does.
///
/// Either mismatch is an error: had a lexer taken a placeholder for text, the
/// driver would bind its own placeholder there, which is silently another
/// parameter's value.
fn cut(
    chars: &[char],
    placeholders: impl Iterator<Item = (Range<usize>, i32)>,
    params: &[Bind],
    engine: Engine,
) -> Result<Sql> {
    let placeholders: Vec<(Range<usize>, i32)> = placeholders.collect();
    if let Some(bind) =
        (params.iter()).find(|bind| !placeholders.iter().any(|(_, found)| *found == bind.number))
    {
        bail!(
            "sqlc reported parameter {} (\"{}\"), but the SQL has no placeholder for it outside strings and comments",
            engine.placeholder(bind.number),
            bind.name
        );
    }
    let text = |range: Range<usize>| chars[range].iter().collect::<String>();
    let mut pieces = Vec::new();
    let mut from = 0;
    for (range, number) in placeholders {
        let placeholder = text(range.clone());
        let Some(bind) = params.iter().find(|bind| bind.number == number) else {
            bail!("the SQL uses {placeholder}, which sqlc reported no parameter for");
        };
        let mut before = text(from..range.start);
        from = range.end;
        let name = bind.name.to_string();
        let piece = match bind.slice {
            None => Piece::Param { placeholder, name },
            Some(slice) => {
                let marker = format!("/*SLICE:{slice}*/");
                let Some(kept) = before.strip_suffix(&marker) else {
                    bail!("sqlc.slice({slice}) has no marker {marker} before its placeholder");
                };
                before = kept.to_string();
                Piece::Slice {
                    marker,
                    placeholder,
                    name,
                }
            }
        };
        if !before.is_empty() {
            pieces.push(Piece::Text(before));
        }
        pieces.push(piece);
    }
    if from < chars.len() {
        pieces.push(Piece::Text(text(from..chars.len())));
    }
    Ok(Sql(pieces))
}
