//! `text()` reads `:name` as a bind where the colon follows none of `:`, a word character and `\`, and the name is
//! followed by neither `:` nor a word character. When it renders the statement, it turns each `\:` that starts a run of
//! word characters not followed by `:` back into `:`.
//!
//! So we write each placeholder as `:name` and escape every other colon as `\:`. An escaped colon is never a bind and
//! is always turned back, as the run after it ends at a character that is not `:` (every other colon is escaped too)
//! and a bind never directly follows text ([`check`]). That way sqlc's text comes back byte for byte on every Python,
//! whatever its Unicode says a word character is.

use anyhow::{Result, bail};

use crate::engine::{Piece, Sql};
use crate::problem::Problem;

#[cfg(test)]
#[path = "text_spec.rs"]
mod spec;

/// SQLAlchemy expands a `sqlc.slice`'s bind into its items in parentheses of its own, so it replaces sqlc's: `IN :ids`
pub fn text(sql: &Sql) -> String {
    let mut text = String::new();
    let mut slice = false;
    for piece in &sql.0 {
        match piece {
            Piece::Text(part) => {
                let part = match slice {
                    true => part.strip_prefix(')').unwrap_or(part),
                    false => part,
                };
                text += &part.replace(':', r"\:");
                slice = false;
            }
            Piece::Param { name, .. } => text += &format!(":{name}"),
            Piece::Slice { name, .. } => {
                slice = true;
                text.pop();
                text += &format!(":{name}");
            }
        }
    }
    text
}

/// Refuses a placeholder `text()` would not bind as written: one directly after `:`, `\` or a word character, which
/// `text()` takes for no bind, or directly before a word character, which it takes for part of the name.
pub fn check(sql: &Sql) -> Result<()> {
    // A piece's first and last character, of a placeholder as sqlc wrote it
    let ends = |piece: &Piece| match piece {
        Piece::Text(text) => (text.chars().next(), text.chars().last()),
        Piece::Param { placeholder, .. } => (placeholder.chars().next(), placeholder.chars().last()),
        Piece::Slice {
            marker, placeholder, ..
        } => (marker.chars().next(), placeholder.chars().last()),
    };
    for (i, piece) in sql.0.iter().enumerate() {
        let placeholder = match piece {
            Piece::Text(_) => continue,
            Piece::Param { placeholder, .. } => placeholder,
            Piece::Slice { .. } => {
                let before = i.checked_sub(1).and_then(|j| ends(&sql.0[j]).1);
                let after = sql.0.get(i + 1).and_then(|next| ends(next).0);
                if (before, after) != (Some('('), Some(')')) {
                    bail!(
                        "SQLAlchemy expands a sqlc.slice into parentheses of its own, so it must be \
                     written directly inside them: IN (sqlc.slice(name))"
                    );
                }
                continue;
            }
        };
        let before = (i.checked_sub(1).and_then(|j| ends(&sql.0[j]).1))
            .filter(|&c| matches!(c, ':' | '\\') || is_word(c))
            .map(|c| ("after", c));
        let after = (sql.0.get(i + 1).and_then(|next| ends(next).0))
            .filter(|&c| is_word(c))
            .map(|c| ("before", c));
        if let Some((side, c)) = before.or(after) {
            bail!(Problem::new(
                format!("SQLAlchemy text() cannot bind parameter {placeholder} directly {side} {c:?}"),
                "add a space"
            ));
        }
    }
    Ok(())
}

/// Whether `text()`'s `\w` may match on any supported Python. We count every character outside ASCII, as Pythons
/// disagree on their Unicode categories. PostgreSQL's lexer takes such a character into a word too, so we refuse no
/// placeholder it reads because of it.
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii()
}
