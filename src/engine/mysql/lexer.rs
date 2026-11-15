use anyhow::Result;

use crate::engine::{Bind, Engine, Sql, cut};

#[cfg(test)]
#[path = "lexer_spec.rs"]
mod spec;

/// sqlc writes every parameter as `?`, numbered by its position, so a parameter used twice is two.
pub fn sql(text: &str, params: &[Bind]) -> Result<Sql> {
    let chars: Vec<char> = text.chars().collect();
    let placeholders = (tokens(&chars).into_iter())
        .filter(|&(start, end)| chars[start..end] == ['?'])
        .enumerate()
        .map(|(i, (start, end))| (start..end, i as i32 + 1));
    cut(&chars, placeholders, params, Engine::MySql)
}

pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let first = chars.next();
    first.is_some_and(|c| continues_identifier(c) && !c.is_ascii_digit()) && chars.all(continues_identifier)
}

fn continues_identifier(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii()
}

pub fn words(sql: &[char]) -> Vec<String> {
    (tokens(sql).into_iter())
        .map(|(start, end)| sql[start..end].iter().collect())
        .collect()
}

/// The start and end of each token of `sql` as MySQL's lexer (`sql_lex.cc`) scans them under its default SQL mode,
/// without whitespace and comments. We only need enough of it to tell code from strings, quoted identifiers and
/// comments.
///
/// A comment is `#` or `-- ` (two dashes, then whitespace) to the end of the line, or a `/* ... */` that does not
/// nest. A string (`'...'` or `"..."`) and a quoted identifier (`` `...` ``) are one token: a quote doubled inside
/// continues one, and so does a quote after a backslash in a string. A word or a number is read whole.
fn tokens(sql: &[char]) -> Vec<(usize, usize)> {
    // Just past the quote that closes the one at `i`.
    let quoted = |i: usize, close: char, escapes: bool| {
        let mut j = i + 1;
        loop {
            match sql[j.min(sql.len())..] {
                [] => return sql.len(),
                ['\\', _, ..] if escapes => j += 2,
                [c, d, ..] if c == close && d == close => j += 2,
                [c, ..] if c == close => return j + 1,
                _ => j += 1,
            }
        }
    };
    let line_end = |i: usize| (i..sql.len()).find(|&j| sql[j] == '\n').map_or(sql.len(), |j| j + 1);
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < sql.len() {
        let (end, token) = match sql[i..] {
            ['#', ..] => (line_end(i), false),
            ['-', '-', c, ..] if c.is_whitespace() || c.is_control() => (line_end(i), false),
            ['-', '-'] => (sql.len(), false),
            ['/', '*', ..] => {
                let end = (i + 2..sql.len()).find(|&j| sql[j..].starts_with(&['*', '/']));
                (end.map_or(sql.len(), |j| j + 2), false)
            }
            [c, ..] if c.is_whitespace() => (i + 1, false),
            [quote @ ('\'' | '"'), ..] => (quoted(i, quote, true), true),
            ['`', ..] => (quoted(i, '`', false), true),
            [c, ..] if continues_identifier(c) => {
                let run = sql[i..].iter().take_while(|&&c| continues_identifier(c));
                (i + run.count(), true)
            }
            _ => (i + 1, true),
        };
        if token {
            tokens.push((i, end));
        }
        i = end;
    }
    tokens
}
