use anyhow::{Result, bail};

use crate::engine::{Bind, Engine, Sql, cut};

#[cfg(test)]
#[path = "lexer_spec.rs"]
mod spec;

/// sqlc writes a parameter as `?`, or as `?N` where one is used again, and SQLite numbers a `?` one past the largest
/// number before it. A named parameter (`:name`, `@name`, `$name`) is an error, as sqlc rewrites each one it reads and
/// a driver binds positions.
pub fn sql(text: &str, params: &[Bind]) -> Result<Sql> {
    let chars: Vec<char> = text.chars().collect();
    let mut placeholders = Vec::new();
    let mut largest = 0i32;
    for (start, end) in tokens(&chars) {
        match chars[start..end] {
            ['?'] => {
                largest = largest.saturating_add(1);
                placeholders.push((start..end, largest));
            }
            ['?', ref digits @ ..] => {
                let digits: String = digits.iter().collect();
                // sqlc numbers parameters from 1, so an overflow is no parameter of its and `cut` reports it
                let number = digits.parse().unwrap_or(i32::MAX);
                largest = largest.max(number);
                placeholders.push((start..end, number));
            }
            [':' | '@' | '$', c, ..] if continues_identifier(c) => {
                let name: String = chars[start..end].iter().collect();
                bail!(
                    "sqlc did not rewrite {name} into a placeholder{}",
                    Engine::Sqlite.unrewritten()
                );
            }
            _ => {}
        }
    }
    cut(&chars, placeholders.into_iter(), params, Engine::Sqlite)
}

fn starts_identifier(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn continues_identifier(c: char) -> bool {
    starts_identifier(c) || c.is_ascii_digit() || c == '$'
}

/// A name SQLite's tokenizer (`tokenize.c`) reads as one word.
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(starts_identifier) && chars.all(continues_identifier)
}

pub fn words(sql: &[char]) -> Vec<String> {
    (tokens(sql).into_iter())
        .map(|(start, end)| sql[start..end].iter().collect())
        .collect()
}

/// The start and end of each token of `sql` as SQLite's tokenizer (`tokenize.c`) scans them, without whitespace and
/// comments. We only need enough of it to tell code from strings, quoted identifiers and comments.
///
/// A comment is `--` to the end of the line, or a `/* ... */` that does not nest. A string (`'...'`) and a quoted
/// identifier (`"..."`, `` `...` ``, `[...]`) are one token, a quote doubled inside continuing one. A word or a number
/// is read whole, and so is a placeholder or a variable: `?` and its digits, or `:`, `@` or `$` and an identifier's
/// characters.
fn tokens(sql: &[char]) -> Vec<(usize, usize)> {
    let run =
        |from: usize, continues: fn(char) -> bool| from + sql[from..].iter().take_while(|&&c| continues(c)).count();
    // Just past the quote that closes the one at `i`, a doubled one continuing (but never a doubled `]`)
    let quoted = |i: usize, close: char| {
        let mut j = i + 1;
        loop {
            match sql[j.min(sql.len())..] {
                [] => return sql.len(),
                [c, d, ..] if c == close && d == close && close != ']' => j += 2,
                [c, ..] if c == close => return j + 1,
                _ => j += 1,
            }
        }
    };
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < sql.len() {
        let (end, token) = match sql[i..] {
            ['-', '-', ..] => {
                let end = (i..sql.len()).find(|&j| sql[j] == '\n');
                (end.map_or(sql.len(), |j| j + 1), false)
            }
            ['/', '*', ..] => {
                let end = (i + 2..sql.len()).find(|&j| sql[j..].starts_with(&['*', '/']));
                (end.map_or(sql.len(), |j| j + 2), false)
            }
            [c, ..] if c.is_whitespace() => (i + 1, false),
            [quote @ ('\'' | '"' | '`'), ..] => (quoted(i, quote), true),
            ['[', ..] => (quoted(i, ']'), true),
            ['?', ..] => (run(i + 1, |c| c.is_ascii_digit()), true),
            [':' | '@' | '$', c, ..] if continues_identifier(c) => (run(i + 1, continues_identifier), true),
            [c, ..] if continues_identifier(c) => (run(i, continues_identifier), true),
            _ => (i + 1, true),
        };
        if token {
            tokens.push((i, end));
        }
        i = end;
    }
    tokens
}
