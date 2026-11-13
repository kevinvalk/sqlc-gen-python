//! A statement as SQLAlchemy's `text()` must see it (`docs/python-output.md`
//! §6, "On SQLAlchemy").
//!
//! `text()` reads `:name` as a bind where the colon follows none of `:`, a
//! word character and `\`, and the name is followed by neither `:` nor a word
//! character. When it renders the statement, it turns each `\:` that starts a
//! run of word characters not followed by `:` back into `:`.
//!
//! So each placeholder becomes `:name`, and every colon of the statement's own
//! is written `\:`. An escaped colon is never a bind, and is always turned
//! back: the run after it ends at a character that is not `:`, since every
//! other colon is escaped too, and a bind never directly follows text (see
//! [`check`]). sqlc's text comes back byte for byte, on every Python, whose
//! Unicode decides what a word character is.

use anyhow::{Result, bail};

use crate::engine::{Piece, Sql};

#[cfg(test)]
#[path = "text_spec.rs"]
mod spec;

/// `sql` as `text()` takes it: each placeholder `:name`, every other colon
/// escaped. A `sqlc.slice` is a bind SQLAlchemy expands into its items in
/// parentheses of its own, so it takes the place of sqlc's: `IN :ids`.
pub fn text(sql: &Sql) -> String {
    let mut text = String::new();
    let mut slice = false;
    for piece in &sql.0 {
        match piece {
            Piece::Text(part) => {
                let part = if slice { &part[1..] } else { part };
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

/// An error if `text()` would not bind a placeholder of `sql` as that
/// placeholder: one directly after `:`, `\` or a word character, which `text()`
/// takes for no bind, or directly before a word character, which it takes for
/// part of the name.
pub fn check(sql: &Sql) -> Result<()> {
    // A piece's first and last character, a placeholder's as sqlc wrote it.
    let ends = |piece: &Piece| match piece {
        Piece::Text(text) => (text.chars().next(), text.chars().last()),
        Piece::Param { placeholder, .. } => {
            (placeholder.chars().next(), placeholder.chars().last())
        }
        Piece::Slice {
            marker,
            placeholder,
            ..
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
            bail!(
                "SQLAlchemy text() cannot bind parameter {placeholder} directly {side} {c:?}; add a space"
            );
        }
    }
    Ok(())
}

/// Whether `c` may be a word character to `text()`, whose `\w` is Python's on
/// any supported version: an ASCII letter, digit or `_`, or any character
/// outside ASCII, whose Unicode category Pythons disagree on. PostgreSQL's
/// lexer takes such a character into a word too, so no placeholder it reads
/// is refused for it.
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii()
}
