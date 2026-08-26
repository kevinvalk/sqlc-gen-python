//! SQLAlchemy Core.
//!
//! Queries are executed through `sqlalchemy.text`, which recognises exactly one
//! placeholder style — `:name` — whatever the database underneath uses. So the
//! SQL sqlc produced has to be rewritten, and every colon already in it has to
//! be escaped out of the way first.

use crate::engine::Engine;
use crate::python::Type;

#[cfg(test)]
#[path = "sqlalchemy_spec.rs"]
mod spec;

/// Rewrites sqlc's placeholders into the named form `sqlalchemy.text` expects.
///
/// Parameters keep their original position in the name — `$3` becomes `:p3` —
/// so the mapping stays legible against the query the user wrote.
pub fn sql(sql: &str, engine: Engine) -> String {
    // Escape first: the colons introduced below are ours, and must survive.
    let escaped = sql.replace(':', r"\:");

    match engine {
        // PostgreSQL numbers its own placeholders, so keep that numbering.
        Engine::PostgreSql => rewrite(&escaped, |c| c == '$', Numbering::FromQuery),
        // MySQL and SQLite are positional, so the number comes from the order.
        Engine::MySql | Engine::Sqlite => rewrite(&escaped, |c| c == '?', Numbering::ByPosition),
    }
}

/// The type of the connection a querier holds.
pub fn connection(asynchronous: bool) -> Type {
    match asynchronous {
        true => Type::Qualified {
            module: "sqlalchemy.ext.asyncio",
            attribute: "AsyncConnection",
        },
        false => Type::Qualified {
            module: "sqlalchemy.engine",
            attribute: "Connection",
        },
    }
}

/// Where a rewritten placeholder's number comes from.
enum Numbering {
    /// The digits the query already carries, as in `$3`.
    FromQuery,
    /// Its position among the placeholders, as in the third `?`.
    ByPosition,
}

fn rewrite(sql: &str, is_marker: impl Fn(char) -> bool, numbering: Numbering) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut position = 0;

    while let Some(character) = chars.next() {
        if !is_marker(character) {
            out.push(character);
            continue;
        }
        match numbering {
            Numbering::ByPosition => {
                position += 1;
                out.push_str(&format!(":p{position}"));
            }
            Numbering::FromQuery => {
                // A `$` not introducing a number is not a placeholder — it may
                // be a dollar-quoted string, which must survive untouched.
                let Some(digit) = chars.peek().filter(|c| c.is_ascii_digit()).copied() else {
                    out.push(character);
                    continue;
                };
                out.push_str(":p");
                out.push(digit);
                chars.next();
                while let Some(digit) = chars.peek().filter(|c| c.is_ascii_digit()).copied() {
                    out.push(digit);
                    chars.next();
                }
            }
        }
    }
    out
}
