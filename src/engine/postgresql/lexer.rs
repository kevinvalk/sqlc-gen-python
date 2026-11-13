//! PostgreSQL's lexer, as far as finding sqlc's placeholders needs it: what is
//! code and what is a string, a quoted identifier or a comment, as `scan.l`
//! reads it. `tests/py/scanner.py` holds it to libpg_query, the lexer sqlc
//! parses with.

use anyhow::{Result, bail};

use crate::engine::{Sql, cut};

#[cfg(test)]
#[path = "lexer_spec.rs"]
mod spec;

/// `text` cut at its placeholders `$N`, as [`cut`] names them. A named
/// parameter sqlc did not rewrite, as it does not in a statement it does not
/// analyse, is an error.
pub fn sql(text: &str, params: &[(i32, &str)]) -> Result<Sql> {
    let chars: Vec<char> = text.chars().collect();
    let tokens = tokens(&chars);
    if let Some(param) = unrewritten(&chars, &tokens) {
        bail!(
            "sqlc did not rewrite {param} into a placeholder, as it does not in a statement it \
             does not analyse, such as MERGE; write it as INSERT ... ON CONFLICT, or UPDATE ... FROM"
        );
    }
    let placeholders = (tokens.iter()).filter_map(|token| match token.kind {
        Kind::Placeholder(number) => Some((token.start..token.end, number)),
        _ => None,
    });
    cut(&chars, placeholders, params, |number| format!("${number}"))
}

/// A character PostgreSQL's lexer continues a dollar-quote tag with: an
/// ASCII letter or digit, `_`, or any character outside ASCII (`dolq_cont` in
/// its `scan.l`).
fn is_tag(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii()
}

/// A character PostgreSQL's lexer continues an identifier with: a tag's, or
/// `$` (`ident_cont`).
fn continues_identifier(c: char) -> bool {
    is_tag(c) || c == '$'
}

/// Whitespace to PostgreSQL's lexer (`space` in its `scan.l`).
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// Whether `sql` holds a `MERGE INTO`, as code.
pub fn merges(sql: &str) -> bool {
    let chars: Vec<char> = sql.chars().collect();
    (tokens(&chars).windows(2))
        .any(|pair| pair[0].is_word(&chars, "merge") && pair[1].is_word(&chars, "into"))
}

/// Whether `name` is one identifier to PostgreSQL's lexer, unquoted
/// (`identifier` in its `scan.l`).
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(starts_identifier) && chars.all(continues_identifier)
}

/// The first named parameter sqlc left as written in `sql`, rather than
/// rewriting it into a placeholder: `@name`, a lone `@` operator directly
/// before what starts an identifier, or `sqlc.arg`, `sqlc.narg`,
/// `sqlc.slice` or `sqlc.embed`. sqlc rewrites each of them in every
/// statement it analyses, so PostgreSQL would read what is left as something
/// else: `@amount` is the absolute value of a column `amount`.
fn unrewritten(sql: &[char], tokens: &[Token]) -> Option<String> {
    let text = |token: &Token| token.text(sql);
    (0..tokens.len()).find_map(|i| match &tokens[i..] {
        [at, next, ..]
            if at.kind == Kind::Operator
                && text(at) == "@"
                && (sql.get(at.end)).is_some_and(|&c| c == '"' || starts_identifier(c)) =>
        {
            Some(format!("@{}", text(next)))
        }
        [sqlc, dot, function, ..]
            if sqlc.is_word(sql, "sqlc")
                && text(dot) == "."
                && ["arg", "narg", "slice", "embed"]
                    .iter()
                    .any(|name| function.is_word(sql, name)) =>
        {
            Some(format!("{}.{}", text(sqlc), text(function)))
        }
        _ => None,
    })
}

/// A token of what PostgreSQL's lexer reads as code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Token {
    start: usize,
    end: usize,
    kind: Kind,
}

impl Token {
    fn text(&self, sql: &[char]) -> String {
        sql[self.start..self.end].iter().collect()
    }

    /// Whether this is the unquoted word `word`, in any case.
    fn is_word(&self, sql: &[char], word: &str) -> bool {
        self.kind == Kind::Word && self.text(sql).eq_ignore_ascii_case(word)
    }
}

/// What a token is, as far as the rewriting needs to tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `$N`, with its number.
    Placeholder(i32),
    /// An identifier or a keyword, quoted or not.
    Word,
    /// A run of operator characters, such as `@` or `<@`.
    Operator,
    /// Anything else: a literal, a number, a punctuation mark.
    Other,
}

/// The tokens of `sql`, scanned as PostgreSQL's lexer (`scan.l`) does,
/// leaving out whitespace and comments: `--` to the end of the line (`\n` or
/// `\r`), and nested `/* ... */`. A string is one token: `'...'` (with `''`
/// and, in an `E` string, `\'`), `$tag$ ... $tag$`, and the quoted identifier
/// `"..."`. An identifier continues with `$` (`a$1`), and a number is read
/// whole (`0x1F` of `0x1F$1`). An operator ends where a comment starts.
fn tokens(sql: &[char]) -> Vec<Token> {
    let run = |from: usize, continues: fn(char) -> bool| {
        from + sql[from..].iter().take_while(|&&c| continues(c)).count()
    };
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < sql.len() {
        let (end, kind) = match sql[i..] {
            ['-', '-', ..] => {
                let end = (i..sql.len()).find(|&j| matches!(sql[j], '\n' | '\r'));
                (end.map_or(sql.len(), |j| j + 1), None)
            }
            ['/', '*', ..] => {
                let (mut depth, mut j) = (1, i + 2);
                while depth > 0 && j < sql.len() {
                    match sql[j..] {
                        ['/', '*', ..] => (depth, j) = (depth + 1, j + 2),
                        ['*', '/', ..] => (depth, j) = (depth - 1, j + 2),
                        _ => j += 1,
                    }
                }
                (j, None)
            }
            [c, ..] if is_space(c) => (i + 1, None),
            ['E' | 'e', '\'', ..] => (string_end(sql, i + 1, true), Some(Kind::Other)),
            ['\'', ..] => (string_end(sql, i, false), Some(Kind::Other)),
            ['"', ..] => (find_end(sql, i + 1, &['"']), Some(Kind::Word)),
            ['$', c, ..] if c.is_ascii_digit() => {
                let end = run(i + 1, |c| c.is_ascii_digit());
                let number: String = sql[i + 1..end].iter().collect();
                // sqlc numbers parameters from 1, far below `i32::MAX`.
                let number = number.parse().unwrap_or(i32::MAX);
                (end, Some(Kind::Placeholder(number)))
            }
            ['$', ..] => match run(i + 1, is_tag) {
                tag if sql.get(tag) == Some(&'$') => {
                    (find_end(sql, tag + 1, &sql[i..=tag]), Some(Kind::Other))
                }
                _ => (i + 1, Some(Kind::Other)),
            },
            [c, ..] if starts_identifier(c) => (run(i, continues_identifier), Some(Kind::Word)),
            [c, ..] if is_tag(c) => (run(i, is_tag), Some(Kind::Other)),
            [c, ..] if is_operator(c) => {
                let mut end = i + 1;
                while end < sql.len()
                    && is_operator(sql[end])
                    && !matches!(sql[end..], ['-', '-', ..] | ['/', '*', ..])
                {
                    end += 1;
                }
                (end, Some(Kind::Operator))
            }
            _ => (i + 1, Some(Kind::Other)),
        };
        if let Some(kind) = kind {
            tokens.push(Token {
                start: i,
                end,
                kind,
            });
        }
        i = end;
    }
    tokens
}

/// A character PostgreSQL's lexer starts an identifier with (`ident_start`).
fn starts_identifier(c: char) -> bool {
    is_tag(c) && !c.is_ascii_digit()
}

/// A character PostgreSQL's lexer reads into an operator (`op_chars` in its
/// `scan.l`).
fn is_operator(c: char) -> bool {
    "~!@#^&|`?+-*/%<>=".contains(c)
}

/// Just past the first `end` in `sql` from `from`, or the end of `sql`.
fn find_end(sql: &[char], from: usize, end: &[char]) -> usize {
    (from..=sql.len().saturating_sub(end.len()))
        .find(|&j| sql[j..].starts_with(end))
        .map_or(sql.len(), |j| j + end.len())
}

/// Just past the string literal whose quote is at `i`, where `\` escapes a
/// character in an `E` string. A quote directly after its closing quote
/// (`''`) continues it, and so does one after whitespace holding a newline
/// (`quotecontinue` in `scan.l`): `E'a'` `\n` `'\'b'` is one string.
/// PostgreSQL also takes a `--` comment for that whitespace, but libpg_query,
/// which sqlc parses and numbers placeholders with, does not, so neither do we.
fn string_end(sql: &[char], mut i: usize, escapes: bool) -> usize {
    loop {
        let mut j = i + 1;
        while j < sql.len() && sql[j] != '\'' {
            j += if escapes && sql[j] == '\\' { 2 } else { 1 };
        }
        let end = (j + 1).min(sql.len());
        let space = sql[end..].iter().take_while(|&&c| is_space(c)).count();
        let newline = sql[end..end + space]
            .iter()
            .any(|c| matches!(c, '\n' | '\r'));
        match sql.get(end + space) {
            Some('\'') if space == 0 || newline => i = end + space,
            _ => return end,
        }
    }
}
