//! An engine knows what its catalog's types are in Python and how its SQL reads, but nothing of the library that runs
//! the SQL (that is the backend's). Where engines differ a method is an exhaustive `match`, so a new engine makes the
//! compiler list everything it has to answer.

mod mysql;
mod postgresql;
mod sqlite;

#[cfg(test)]
mod spec;

use std::ops::Range;

use anyhow::{Result, bail};

use crate::plugin;
use crate::problem::Problem;
use crate::python::Type;
use crate::sqlc_typing::bindings;

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
        argument: usize,
        name: String,
    },
    /// The placeholder of a `sqlc.slice`, which sqlc writes after a marker (`/*SLICE:ids*/`) and which stands for one
    /// placeholder per item of the list.
    Slice {
        marker: String,
        placeholder: String,
        number: i32,
        argument: usize,
        name: String,
    },
}

/// A parameter sqlc reported, as [`Engine::sql`] binds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind<'a> {
    pub number: i32,
    /// Index in the generated function's public parameters.
    pub argument: usize,
    /// The Python name.
    pub name: &'a str,
    /// sqlc's own name of a `sqlc.slice`, which its marker spells.
    pub slice: Option<&'a str>,
}

#[cfg(test)]
pub fn binds<'a>(params: &[(i32, &'a str)]) -> Vec<Bind<'a>> {
    (params.iter())
        .enumerate()
        .map(|(argument, &(number, name))| Bind {
            number,
            argument,
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

    /// Whether we support `:execlastid`. PostgreSQL and SQLite have `RETURNING` instead, and SQLite's last id is the
    /// connection's, so an `INSERT` that inserted nothing would report the previous one's.
    pub fn has_last_insert_id(self) -> bool {
        match self {
            Self::PostgreSql | Self::Sqlite => false,
            Self::MySql => true,
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
    ///
    /// sqlc rewrites every `sqlc.arg`, `sqlc.narg`, `sqlc.slice` and `sqlc.embed` it reads, so one still in the
    /// statement is an error: the function would lack the parameter, and the database be sent the call as written!
    /// Without its `(` it is a column of a table named `sqlc`, which sqlc sends as written.
    pub fn sql(self, text: &str, params: &[Bind]) -> Result<Sql> {
        bindings::check_macros(self, &self.code(text))?;
        match self {
            Self::PostgreSql => postgresql::sql(text, params),
            Self::Sqlite => sqlite::sql(text, params),
            Self::MySql => mysql::sql(text, params),
        }
    }

    /// Whether `sql` is a `MERGE`, as not every driver reports the row count of one.
    pub fn merges(self, sql: &str) -> bool {
        match self {
            Self::PostgreSql => Self::after(self.words(sql), &["merge", "into"]).is_some(),
            // Neither has `MERGE`.
            Self::Sqlite | Self::MySql => false,
        }
    }

    /// Whether an `INSERT` takes the sequence's value for an identity column and discards the one it is given. A
    /// `COPY` is no such statement: it "will always write the column values provided in the input data, like the
    /// INSERT option OVERRIDING SYSTEM VALUE" (https://www.postgresql.org/docs/current/sql-copy.html).
    pub fn overrides_user_value(self, sql: &str) -> bool {
        match self {
            Self::PostgreSql => Self::after(self.words(sql), &["overriding", "user", "value"]).is_some(),
            // Neither has the clause.
            Self::Sqlite | Self::MySql => false,
        }
    }

    /// Whether a driver reads the row count of `sql` before the engine has it. SQLite counts the rows a statement
    /// writes as it returns them, so the count is 0 until every row of a `RETURNING` is read.
    pub fn counts_as_it_returns(self, sql: &str) -> bool {
        match self {
            Self::Sqlite => Self::after(self.words(sql), &["returning"]).is_some(),
            // Both count the rows before they return the first.
            Self::PostgreSql | Self::MySql => false,
        }
    }

    /// Whether a result `column` is an expression cast to a type that is none of SQLite's own. SQLite casts to one of
    /// five affinities, and such a type (`DATETIME`, `DECIMAL`) has the NUMERIC one: the cast makes a number of text
    /// that starts as one (`'2024-06-07'` is `2024`), which no column of that type holds.
    pub fn casts_to_number(self, column: &plugin::Column) -> bool {
        match self {
            Self::Sqlite => {
                column.table.is_none() && !column.data_type().is_empty() && self.scalar_type(column).is_none()
            }
            // Both cast to the type they name.
            Self::PostgreSql | Self::MySql => false,
        }
    }

    /// Whether `sql`, a MySQL statement, binds a parameter after its `ON DUPLICATE KEY UPDATE`.
    pub fn binds_on_duplicate(self, sql: &str) -> bool {
        Self::after(self.code(sql), &["on", "duplicate"]).is_some_and(|words| words.iter().any(|word| word == "?"))
    }

    /// What follows the first `phrase` among `words`, ignoring case.
    fn after(mut words: Vec<String>, phrase: &[&str]) -> Option<Vec<String>> {
        let is_phrase =
            |found: &[String]| std::iter::zip(found, phrase).all(|(word, part)| word.eq_ignore_ascii_case(part));
        let start = words.windows(phrase.len()).position(is_phrase)?;
        Some(words.split_off(start + phrase.len()))
    }

    /// The words of `text` whose meaning we may rely on.
    pub(crate) fn words(self, text: &str) -> Vec<String> {
        match self {
            // Executable comments can change a join or projection. Only trust the words when the lexer found none.
            Self::MySql => mysql::words(&text.chars().collect::<Vec<_>>(), false),
            Self::PostgreSql => {
                // PostgreSQL folds unquoted names before it looks them up in the catalog.
                let mut words = self.code(text);
                for word in &mut words {
                    if postgresql::is_identifier(word) {
                        word.make_ascii_lowercase();
                    }
                }
                words
            }
            Self::Sqlite => self.code(text),
        }
    }

    /// Every word of `text` outside its comments, executable ones too: what we look in for something to refuse, as
    /// what stands outside a comment is code whatever an executable comment adds to it.
    fn code(self, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        match self {
            Self::PostgreSql => postgresql::words(&chars),
            Self::Sqlite => sqlite::words(&chars),
            Self::MySql => mysql::words(&chars, true),
        }
    }

    /// Whether `name`, bare, is a type of the engine's own that we map, so never another schema's type of that name.
    pub fn is_own_type(self, name: &str) -> bool {
        match self {
            Self::PostgreSql => postgresql::is_own(&name.to_ascii_lowercase()),
            // Neither has schemas of types
            Self::Sqlite | Self::MySql => false,
        }
    }

    /// The engine's own name for `data_type`, whichever spelling sqlc reported. Only PostgreSQL has several.
    pub fn type_name(self, data_type: &str) -> String {
        match self {
            Self::PostgreSql => postgresql::type_name(data_type),
            Self::Sqlite | Self::MySql => data_type.to_string(),
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
        bail!(Problem::new(
            format!(
                "sqlc reported parameter {} (\"{}\"), but the SQL has no placeholder for it outside \
             strings and comments",
                engine.placeholder(bind.number),
                bind.name
            ),
            bindings::misnumbered(engine)
        ));
    }

    let text = |range: Range<usize>| chars[range].iter().collect::<String>();
    let mut pieces = Vec::new();
    let mut from = 0;
    for (range, number) in placeholders {
        let placeholder = text(range.clone());
        let Some(bind) = params.iter().find(|bind| bind.number == number) else {
            bail!(Problem::new(
                format!("the SQL uses {placeholder}, which sqlc reported no parameter for"),
                bindings::misnumbered(engine)
            ));
        };
        let mut before = text(from..range.start);
        from = range.end;
        let name = bind.name.to_string();
        let piece = match bind.slice {
            None => Piece::Param {
                placeholder,
                number,
                argument: bind.argument,
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
                    argument: bind.argument,
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

    // Generated code writes a slice as one placeholder per item, and as a subquery of no rows for an empty list, which
    // is no item of a list: `IN (SELECT ..., ?)` is a syntax error.
    let alone = |at: usize| {
        let opens = |piece: &Piece| matches!(piece, Piece::Text(text) if text.trim_end().ends_with('('));
        let closes = |piece: &Piece| matches!(piece, Piece::Text(text) if text.trim_start().starts_with(')'));
        at.checked_sub(1).is_some_and(|before| opens(&pieces[before])) && pieces.get(at + 1).is_some_and(closes)
    };
    if let Some(name) = pieces.iter().enumerate().find_map(|(at, piece)| match piece {
        Piece::Slice { name, .. } if !alone(at) => Some(name),
        _ => None,
    }) {
        bail!(Problem::new(
            format!("sqlc.slice({name}) shares its parentheses, where an empty list leaves a subquery of no rows"),
            "write it alone, as IN (sqlc.slice(name)), with anything else in an OR beside it"
        ));
    }

    // Generated code expands a slice by replacing its marker anywhere in the statement, so a string or a comment that
    // spells it would expand too. And so would the marker of a placeholder sqlc reported as another parameter.
    let spelt = |marker: &str| (pieces.iter()).any(|piece| matches!(piece, Piece::Text(text) if text.contains(marker)));
    if let Some(marker) = (pieces.iter()).find_map(|piece| match piece {
        Piece::Slice { marker, .. } if spelt(marker) => Some(marker),
        _ => None,
    }) {
        bail!(Problem::new(
            format!("the SQL spells {marker} where sqlc reported no sqlc.slice, as in a string or a comment"),
            bindings::misnumbered(engine)
        ));
    }
    Ok(Sql(pieces))
}
