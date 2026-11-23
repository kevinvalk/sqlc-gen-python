use super::*;
use crate::engine::{Piece, binds};

/// The numbers of the placeholders `sql` holds as code, in order.
fn found(sql: &str) -> Vec<i32> {
    let chars: Vec<char> = sql.chars().collect();
    (tokens(&chars).iter())
        .filter_map(|token| match token.kind {
            Kind::Placeholder(number) => Some(number),
            _ => None,
        })
        .collect()
}

fn error(sql: &str, params: &[(i32, &str)]) -> String {
    format!("{:#}", super::sql(sql, &binds(params)).unwrap_err())
}

#[test]
fn a_placeholder_is_only_code_never_text_postgresql_would_not_read_as_code() {
    for sql in [
        "SELECT '$1'",
        "SELECT 'it''s $1'",
        r"SELECT E'it\'s $1'",
        r#"SELECT "col$1""#,
        "SELECT 1 -- $1\n",
        "SELECT /* $1 /* nested */ $1 */ 1",
        "SELECT $$ $1 $$",
        "SELECT $fn$ $1 $fn$",
        "SELECT a$1 FROM t",
        // PostgreSQL's lexer takes any non-ASCII character into an identifier or a dollar quote's tag, whatever
        // Unicode calls it
        "SELECT $€$ $1 $€$",
        "SELECT a€$1 FROM t",
    ] {
        assert_eq!(found(sql), [0; 0], "{sql}");
    }
    assert_eq!(found("SELECT 'a' || $1 -- x\n"), [1]);
}

#[test]
fn a_statement_is_cut_at_its_placeholders_each_named_and_back_to_sqlcs_text() {
    let text = "SELECT 'é', $12 WHERE $12 = $1";
    let cut = sql(text, &binds(&[(12, "p"), (1, "q")])).unwrap();
    let param = |number: i32, argument: usize, name: &str| Piece::Param {
        argument,
        placeholder: format!("${number}"),
        number,
        name: name.into(),
    };
    assert_eq!(
        cut.0,
        [
            Piece::Text("SELECT 'é', ".into()),
            param(12, 0, "p"),
            Piece::Text(" WHERE ".into()),
            param(12, 0, "p"),
            Piece::Text(" = ".into()),
            param(1, 1, "q"),
        ]
    );
    assert_eq!(cut.text(), text);
}

#[test]
fn a_line_comment_ends_at_a_carriage_return_as_in_postgresql() {
    // `scan.l`: a comment is `--` and then `[^\n\r]*`
    assert_eq!(found("WHERE a = $2 -- note\rAND b = $1"), [2, 1]);
}

#[test]
fn a_doubled_quote_keeps_an_escape_strings_escapes() {
    // `''` is a quote inside the same literal, so a later `\'` is still escaped and does not end it
    assert_eq!(found(r"WHERE a = $2 AND E'it''s\'' <> '' AND b = $1"), [2, 1]);
    assert_eq!(found("SELECT 'a''$1'"), [0; 0]);
}

#[test]
fn a_dollar_sign_belongs_to_an_identifier_only_inside_one() {
    // `scan.l`: `$` continues an identifier, but not a number, a placeholder or a dollar quote, so a placeholder may
    // follow each directly
    for sql in ["SELECT $$x$$$1", r#"SELECT "a"$1"#, "SELECT 1$1", "SELECT 0x1F$1"] {
        assert_eq!(found(sql), [1], "{sql}");
    }
    assert_eq!(found("SELECT $1$2"), [1, 2]);
}

#[test]
fn an_escape_string_goes_on_after_whitespace_holding_a_newline() {
    // `scan.l`'s `quotecontinue`: the second quote continues the `E` string, so its `\'` is a quote, not the end, and
    // `$1` is text
    for sql in ["SELECT E'a'\n'\\' $1 '", "SELECT E'a' \r\n\t'\\' $1 '"] {
        assert_eq!(found(sql), [0; 0], "{sql:?}");
    }
    // Not across a comment in libpg_query, which sqlc parses with, nor without a newline
    assert_eq!(found("SELECT E'a' -- c\n'\\' $1 ''"), [1]);
    assert_eq!(found("SELECT E'a' '\\' $1 ''"), [1]);
}

#[test]
fn a_named_parameter_sqlc_left_as_written_is_an_error() {
    // sqlc does not analyse MERGE, so `@amount` reaches PostgreSQL, which reads it as the absolute value of the column
    // `amount`
    for (sql, param) in [
        ("UPDATE SET amount = b.amount - @amount", "@amount"),
        (r#"SELECT @"Amount""#, r#"@"Amount""#),
        ("SELECT /* c */@a", "@a"),
    ] {
        let error = error(sql, &[]);
        assert!(
            error.contains(&format!("sqlc did not rewrite {param} into a placeholder")),
            "{sql}: {error}"
        );
    }
    // PostgreSQL reads `=@` as one operator, so sqlc saw no parameter and left the name as written
    let glued = error("SELECT id FROM t WHERE id=@id", &[]);
    assert!(
        glued.contains("as the operator =@") && glued.contains("write a space before @id"),
        "{glued}"
    );
    // Operators holding `@`, and what is not code
    for sql in [
        "SELECT tags @> '{a}', tags <@ tags, tags <@tags, a @@ b, @-x, @ x, @1",
        "SELECT '@a', \"@a\", $$@a$$ -- @a\n",
        "SELECT x@--a\n",
    ] {
        assert_eq!(found(sql), [0; 0], "{sql}");
    }
}

#[test]
fn an_identifier_is_one_word_to_postgresql() {
    for name in ["day", "_x", "größe", "a$1"] {
        assert!(is_identifier(name), "{name}");
    }
    for name in ["", "1st", "+x", "Europe/Amsterdam", "a b"] {
        assert!(!is_identifier(name), "{name}");
    }
}
