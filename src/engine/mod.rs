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
    /// [`Bind`] numbers `number` and Python names `name`.
    Param {
        placeholder: String,
        number: i32,
        name: String,
    },
    /// A `sqlc.slice`'s placeholder, after the marker sqlc writes before it
    /// (`/*SLICE:ids*/`): it stands for one placeholder per item of the list.
    Slice {
        marker: String,
        placeholder: String,
        number: i32,
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

    /// Whether the type of `column`, a query's `parameter` or a column of a
    /// result or a table, is sqlc's guess rather than a type it read: on
    /// MySQL and SQLite, `any` for an expression it could not type, or
    /// nothing.
    ///
    /// On PostgreSQL, a bare `integer` is a guess too, like every standard
    /// name ([`postgresql::is_guess`]): sqlc spells a column's own type
    /// `pg_catalog.int4`, so this is the first overload of a function
    /// (`age(ts)`, an interval, read through a CTE is the `integer` of
    /// `age(xid)`). But for a parameter with no column behind it, which sqlc
    /// types so itself: a `LIMIT`'s or an `OFFSET`'s, or a function's
    /// argument.
    pub fn is_guess(self, column: &plugin::Column, parameter: bool) -> bool {
        let data_type = column.data_type();
        match self {
            Self::PostgreSql => {
                let read = parameter && column.table.is_none();
                postgresql::is_guess(&data_type) || (data_type == "integer" && !read)
            }
            Self::Sqlite | Self::MySql => ["", "any"].contains(&data_type.as_str()),
        }
    }

    /// Whether a result `column` is an expression sqlc typed by its form, not
    /// its operands: on MySQL and SQLite, arithmetic of any operands is `int`
    /// (`price * 1.1` too), and MySQL's `1.0`, a `DECIMAL`, is `float`. A
    /// cast is spelt as declared (`INTEGER`, `SIGNED`, `REAL`, `DOUBLE`).
    pub fn is_untyped_expression(self, column: &plugin::Column) -> bool {
        match self {
            Self::PostgreSql => false,
            Self::Sqlite | Self::MySql => {
                column.table.is_none() && ["int", "float"].contains(&column.data_type().as_str())
            }
        }
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
            Self::PostgreSql | Self::MySql => "",
        }
    }

    /// What else may make sqlc report a type that reads as a guess, for an
    /// error to say: on PostgreSQL, analysing the queries on a database,
    /// which names each type as static analysis names only its guesses.
    pub fn analysis(self) -> &'static str {
        match self {
            Self::PostgreSql => {
                "; with a database block in sqlc.yaml, also set analyzer: {database: false}, \
                 as only sqlc's static analysis can be told from a guess"
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

    /// What each of the `columns` items of `text`'s result list is, by
    /// position, if it is read ([`select::items`]).
    pub fn items(self, text: &str, columns: usize) -> Option<Vec<select::Item>> {
        select::items(&self.words(text), columns, &self.aggregates())
    }

    /// Whether `text` returns exactly one row, whatever the data
    /// ([`select::single_row`]).
    pub fn single_row(self, text: &str) -> bool {
        select::single_row(&self.words(text), &self.aggregates())
    }

    /// The tables every row of `text`'s result has a row of
    /// ([`select::present`]).
    pub fn present(self, text: &str) -> Vec<String> {
        select::present(&self.words(text), &self.aggregates())
    }

    /// The engine's aggregate functions, as its manual lists them. One that
    /// is not here, as any the schema defines, is read as any other function.
    fn aggregates(self) -> select::Aggregates {
        match self {
            Self::PostgreSql => select::Aggregates {
                nullable: &[
                    "any_value",
                    "array_agg",
                    "avg",
                    "bit_and",
                    "bit_or",
                    "bit_xor",
                    "bool_and",
                    "bool_or",
                    "corr",
                    "covar_pop",
                    "covar_samp",
                    "every",
                    "json_agg",
                    "json_agg_strict",
                    "json_arrayagg",
                    "json_object_agg",
                    "json_object_agg_strict",
                    "json_object_agg_unique",
                    "json_object_agg_unique_strict",
                    "json_objectagg",
                    "jsonb_agg",
                    "jsonb_agg_strict",
                    "jsonb_object_agg",
                    "jsonb_object_agg_strict",
                    "jsonb_object_agg_unique",
                    "jsonb_object_agg_unique_strict",
                    "max",
                    "min",
                    "mode",
                    "percentile_cont",
                    "percentile_disc",
                    "range_agg",
                    "range_intersect_agg",
                    "regr_avgx",
                    "regr_avgy",
                    "regr_intercept",
                    "regr_r2",
                    "regr_slope",
                    "regr_sxx",
                    "regr_sxy",
                    "regr_syy",
                    "stddev",
                    "stddev_pop",
                    "stddev_samp",
                    "string_agg",
                    "sum",
                    "var_pop",
                    "var_samp",
                    "variance",
                    "xmlagg",
                ],
                never_null: &["count", "regr_count"],
                scalar_of_several: &[],
                bare_columns: false,
            },
            Self::Sqlite => select::Aggregates {
                nullable: &["avg", "group_concat", "max", "min", "string_agg", "sum"],
                // Of no rows, `total` is `0.0`, and a JSON group its empty
                // array or object.
                never_null: &[
                    "count",
                    "json_group_array",
                    "json_group_object",
                    "jsonb_group_array",
                    "jsonb_group_object",
                    "total",
                ],
                scalar_of_several: &["max", "min"],
                bare_columns: true,
            },
            Self::MySql => select::Aggregates {
                nullable: &[
                    "avg",
                    "group_concat",
                    "json_arrayagg",
                    "json_objectagg",
                    "max",
                    "min",
                    "std",
                    "stddev",
                    "stddev_pop",
                    "stddev_samp",
                    "sum",
                    "var_pop",
                    "var_samp",
                    "variance",
                ],
                // Of no rows, a bitwise aggregate is its neutral value.
                never_null: &["bit_and", "bit_or", "bit_xor", "count"],
                scalar_of_several: &[],
                bare_columns: true,
            },
        }
    }

    /// The text of each token of `text`'s code, comments left out. None of a
    /// MySQL statement with a comment that is code, which MySQL runs and its
    /// lexer here leaves out (`/*! LEFT */ JOIN`): it is then not read.
    fn words(self, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        match self {
            Self::PostgreSql => postgresql::words(&chars),
            Self::Sqlite => sqlite::words(&chars),
            Self::MySql if text.contains("/*!") || text.contains("/*M!") => Vec::new(),
            Self::MySql => mysql::words(&chars),
        }
    }

    /// Why a statement's placeholders may not be the parameters sqlc reported,
    /// for an error to say, if the engine has a reason.
    fn misnumbered(self) -> &'static str {
        match self {
            Self::PostgreSql | Self::MySql => "",
            // sqlc numbers a `sqlc.arg` among `?`s, and a `sqlc.slice` used
            // again, as SQLite does not.
            Self::Sqlite => {
                "; on SQLite, name every parameter of the statement with sqlc.arg rather than \
                 mix them with ?, and use a sqlc.slice once"
            }
        }
    }

    /// The engine's own name of the type sqlc reports as `data_type`, for a
    /// backend to know it by: PostgreSQL's, whatever spelling sqlc read.
    pub fn type_name(self, data_type: &str) -> String {
        match self {
            Self::PostgreSql => postgresql::type_name(data_type),
            Self::Sqlite | Self::MySql => data_type.to_string(),
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
            "sqlc reported parameter {} (\"{}\"), but the SQL has no placeholder for it outside \
             strings and comments{}",
            engine.placeholder(bind.number),
            bind.name,
            engine.misnumbered()
        );
    }
    let text = |range: Range<usize>| chars[range].iter().collect::<String>();
    let mut pieces = Vec::new();
    let mut from = 0;
    for (range, number) in placeholders {
        let placeholder = text(range.clone());
        let Some(bind) = params.iter().find(|bind| bind.number == number) else {
            bail!(
                "the SQL uses {placeholder}, which sqlc reported no parameter for{}",
                engine.misnumbered()
            );
        };
        let mut before = text(from..range.start);
        from = range.end;
        let name = bind.name.to_string();
        let piece = match bind.slice {
            None => Piece::Param {
                placeholder,
                number,
                name,
            },
            Some(slice) => {
                let marker = format!("/*SLICE:{slice}*/");
                let Some(kept) = before.strip_suffix(&marker) else {
                    bail!("sqlc.slice({slice}) has no marker {marker} before its placeholder");
                };
                before = kept.to_string();
                Piece::Slice {
                    marker,
                    placeholder,
                    number,
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

    // Generated code expands a slice by replacing its marker in the whole
    // statement, where a string or a comment that spells it would expand too.
    let spelt = |marker: &str| {
        (pieces.iter()).any(|piece| matches!(piece, Piece::Text(text) if text.contains(marker)))
    };
    if let Some(marker) = (pieces.iter()).find_map(|piece| match piece {
        Piece::Slice { marker, .. } if spelt(marker) => Some(marker),
        _ => None,
    }) {
        bail!("the SQL spells {marker} where no sqlc.slice is, in a string or a comment");
    }
    Ok(Sql(pieces))
}
