//! An engine knows what its catalog's types are in Python and how its SQL reads, but nothing of the library that runs
//! the SQL (that is the backend's). Every method is an exhaustive `match`, so a new engine makes the compiler list
//! everything it has to answer.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    PostgreSql,
    Sqlite,
    MySql,
}

/// A statement as sqlc wrote it, cut at its placeholders so that a backend can write each in its own style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sql(pub Vec<Piece>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    /// `placeholder` is as sqlc wrote it: `$1`, `?` or `?2`.
    Param {
        placeholder: String,
        number: i32,
        name: String,
    },
    /// The placeholder of a `sqlc.slice`, which sqlc writes after a marker (`/*SLICE:ids*/`) and which stands for one
    /// placeholder per item of the list.
    Slice {
        marker: String,
        placeholder: String,
        number: i32,
        name: String,
    },
}

/// A parameter sqlc reported, as [`Engine::sql`] binds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind<'a> {
    pub number: i32,
    /// The Python name.
    pub name: &'a str,
    /// sqlc's own name of a `sqlc.slice`, which its marker spells.
    pub slice: Option<&'a str>,
}

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
    /// The statement exactly as sqlc wrote it.
    pub fn text(&self) -> String {
        (self.0.iter())
            .map(|piece| match piece {
                Piece::Text(text) | Piece::Param { placeholder: text, .. } => text.clone(),
                Piece::Slice {
                    marker, placeholder, ..
                } => format!("{marker}{placeholder}"),
            })
            .collect()
    }

    pub fn is_slice(&self, name: &str) -> bool {
        (self.0.iter()).any(|piece| matches!(piece, Piece::Slice { name: slice, .. } if slice == name))
    }
}

impl Engine {
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

    pub fn name(self) -> &'static str {
        match self {
            Self::PostgreSql => "postgresql",
            Self::Sqlite => "sqlite",
            Self::MySql => "mysql",
        }
    }

    /// How an error message names placeholder `number`. MySQL's are all `?`, so we add the number.
    pub fn placeholder(self, number: i32) -> String {
        match self {
            Self::PostgreSql => format!("${number}"),
            Self::Sqlite => format!("?{number}"),
            Self::MySql => format!("? #{number}"),
        }
    }

    /// A cast of `expression` in the engine's syntax, for an error message to suggest.
    pub fn cast(self, expression: &str) -> String {
        match self {
            Self::PostgreSql => format!("{expression}::<type>"),
            Self::Sqlite | Self::MySql => format!("CAST({expression} AS <type>)"),
        }
    }

    /// `None` when we have no driver evidence for the column's type.
    pub fn scalar_type(self, column: &plugin::Column) -> Option<Type> {
        match self {
            Self::PostgreSql => postgresql::scalar_type(&column.data_type()),
            Self::Sqlite => sqlite::scalar_type(&column.data_type()),
            Self::MySql => mysql::scalar_type(column),
        }
    }

    /// Why a column sqlc reports as an enum of `labels` cannot be generated as one, if it cannot.
    pub fn enum_problem(self, column: &plugin::Column, labels: &[String]) -> Option<&'static str> {
        match self {
            Self::PostgreSql | Self::Sqlite => None,
            Self::MySql => mysql::enum_problem(column, labels),
        }
    }

    /// Whether we support `:execlastid`. PostgreSQL and SQLite have `RETURNING` instead, and SQLite's last id is the
    /// connection's, so an `INSERT` that inserted nothing would report the previous one's.
    pub fn has_last_insert_id(self) -> bool {
        match self {
            Self::PostgreSql | Self::Sqlite => false,
            Self::MySql => true,
        }
    }

    /// Whether sqlc guessed the type of `column` (a query's parameter when `parameter`, else a result's or a table's
    /// column) instead of reading it. On MySQL and SQLite a guess is `any` or nothing, for an expression sqlc could
    /// not type.
    ///
    /// On PostgreSQL every standard name is a guess ([`postgresql::is_guess`]), and so is a bare `integer`: sqlc spells
    /// a column's own type `pg_catalog.int4`, so `integer` is a function's first overload (`age(ts)`, an interval,
    /// read through a CTE is the `integer` of `age(xid)`). Except for a parameter without a column behind it (of a
    /// `LIMIT`, an `OFFSET` or a function), which sqlc types as `integer` itself.
    pub fn is_guess(self, column: &plugin::Column, parameter: bool) -> bool {
        let data_type = column.data_type();
        match self {
            Self::PostgreSql => {
                let is_typed_by_sqlc = parameter && column.table.is_none();
                postgresql::is_guess(&data_type) || (data_type == "integer" && !is_typed_by_sqlc)
            }
            Self::Sqlite | Self::MySql => ["", "any"].contains(&data_type.as_str()),
        }
    }

    /// Whether a result `column` is an expression sqlc typed by its form instead of its operands. On MySQL and SQLite
    /// any arithmetic is `int` (`price * 1.1` too), MySQL's `1.0` (a `DECIMAL`) is `float` and its `BINARY x` (bytes)
    /// is `char`. Neither a cast nor a column lands here, as a cast keeps its declared spelling (`INTEGER`, `SIGNED`,
    /// `REAL`, `DOUBLE`) and a column has its table.
    pub fn is_untyped_expression(self, column: &plugin::Column) -> bool {
        let forms: &[&str] = match self {
            Self::PostgreSql => &[],
            Self::Sqlite => &["int", "float"],
            Self::MySql => &["int", "float", "char"],
        };
        column.table.is_none() && forms.contains(&column.data_type().as_str())
    }

    /// A hint for the error about a column of `data_type` that has no Python type, or nothing.
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
            Self::MySql if data_type == "var_string" => {
                "; it is how sqlc reports a CAST AS CHAR, a str, and a CAST AS BINARY, bytes, alike"
            }
            Self::PostgreSql | Self::MySql => "",
        }
    }

    /// A hint for the error about a guessed type. On PostgreSQL, sqlc's database analyzer names every type the way its
    /// static analysis names only its guesses, so we cannot tell them apart.
    pub fn analysis(self) -> &'static str {
        match self {
            Self::PostgreSql => {
                "; with a database block in sqlc.yaml, also set analyzer: {database: false}, \
                 as only sqlc's static analysis can be told from a guess"
            }
            Self::Sqlite | Self::MySql => "",
        }
    }

    pub fn array_problem(self, data_type: &str, is_parameter: bool) -> Option<&'static str> {
        match self {
            Self::PostgreSql => postgresql::array_problem(data_type, is_parameter),
            Self::Sqlite => Some("SQLite does not have"),
            Self::MySql => Some("MySQL does not have"),
        }
    }

    /// sqlc's `text` cut at its placeholders, which must be exactly the parameters sqlc reported in `params`.
    pub fn sql(self, text: &str, params: &[Bind]) -> Result<Sql> {
        match self {
            Self::PostgreSql => postgresql::sql(text, params),
            Self::Sqlite => sqlite::sql(text, params),
            Self::MySql => mysql::sql(text, params),
        }
    }

    /// Whether `sql` is a `MERGE`, as not every driver reports the row count of one.
    pub fn merges(self, sql: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::merges(sql),
            // Neither has `MERGE`.
            Self::Sqlite | Self::MySql => false,
        }
    }

    pub fn items(self, text: &str, tables: &[&str]) -> Option<Vec<select::Item>> {
        select::items(&self.words(text), tables, &self.aggregates())
    }

    pub fn single_row(self, text: &str) -> bool {
        select::single_row(&self.words(text), &self.aggregates())
    }

    pub fn present(self, text: &str) -> Vec<String> {
        select::present(&self.words(text), &self.aggregates())
    }

    /// The aggregate functions as each manual lists them.
    ///
    /// NOTE: An aggregate the schema defines is not here, so we read it as any other function.
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
                // Of no rows `total` is `0.0`, and a JSON group is its empty array or object
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
                // Of no rows a bitwise aggregate is its neutral value
                never_null: &["bit_and", "bit_or", "bit_xor", "count"],
                scalar_of_several: &[],
                bare_columns: true,
            },
        }
    }

    fn words(self, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        match self {
            Self::PostgreSql => postgresql::words(&chars),
            Self::Sqlite => sqlite::words(&chars),
            // MySQL runs an executable comment (`/*! LEFT */ JOIN`) that our lexer leaves out, so we rather read no
            // tokens at all, which reads as nothing known of the statement.
            Self::MySql if text.contains("/*!") || text.contains("/*M!") => Vec::new(),
            Self::MySql => mysql::words(&chars),
        }
    }

    /// A hint for the error when a statement's placeholders are not the parameters sqlc reported.
    fn misnumbered(self) -> &'static str {
        match self {
            Self::PostgreSql | Self::MySql => "",
            // sqlc numbers a `sqlc.arg` among `?`s, and a `sqlc.slice` used twice, differently than SQLite does
            Self::Sqlite => {
                "; on SQLite, name every parameter of the statement with sqlc.arg rather than \
                 mix them with ?, and use a sqlc.slice once"
            }
        }
    }

    /// The engine's own name for `data_type`, whichever spelling sqlc reported. Only PostgreSQL has several.
    /// Whether `name`, bare, is a type of the engine's own that we map, so never another schema's type of that name.
    pub fn is_own_type(self, name: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::is_own(&name.to_ascii_lowercase()),
            // Neither has schemas of types
            Self::Sqlite | Self::MySql => false,
        }
    }

    pub fn type_name(self, data_type: &str) -> String {
        match self {
            Self::PostgreSql => postgresql::type_name(data_type),
            Self::Sqlite | Self::MySql => data_type.to_string(),
        }
    }

    /// The type of `count(...)`, spelled as sqlc spells a type it read instead of guessed.
    pub fn count_type(self) -> &'static str {
        match self {
            Self::PostgreSql => "pg_catalog.int8",
            Self::Sqlite => "integer",
            Self::MySql => "bigint",
        }
    }

    pub fn is_identifier(self, name: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::is_identifier(name),
            Self::Sqlite => sqlite::is_identifier(name),
            Self::MySql => mysql::is_identifier(name),
        }
    }
}

/// `chars` cut at `placeholders` (each a range and the number of the parameter it binds), named after `params`.
///
/// We fail when the placeholders are not exactly the parameters sqlc reported, as had a lexer taken a placeholder for
/// text, the driver would still bind there: silently with another parameter's value!
fn cut(
    chars: &[char],
    placeholders: impl Iterator<Item = (Range<usize>, i32)>,
    params: &[Bind],
    engine: Engine,
) -> Result<Sql> {
    let placeholders: Vec<(Range<usize>, i32)> = placeholders.collect();

    if let Some(bind) = (params.iter()).find(|bind| !placeholders.iter().any(|(_, found)| *found == bind.number)) {
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

    // Generated code expands a slice by replacing its marker anywhere in the statement, so a string or a comment that
    // spells it would expand too.
    let spelt = |marker: &str| (pieces.iter()).any(|piece| matches!(piece, Piece::Text(text) if text.contains(marker)));
    if let Some(marker) = (pieces.iter()).find_map(|piece| match piece {
        Piece::Slice { marker, .. } if spelt(marker) => Some(marker),
        _ => None,
    }) {
        bail!("the SQL spells {marker} where no sqlc.slice is, in a string or a comment");
    }
    Ok(Sql(pieces))
}
