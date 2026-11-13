//! Specification for finding sqlc's placeholders as MySQL's lexer does.

use crate::engine::{Piece, binds};

/// The names of the parameters `sql` binds, in order, with `?` number `n`
/// named `pn`.
fn bound(sql: &str, count: i32) -> Vec<String> {
    let names: Vec<String> = (1..=count).map(|n| format!("p{n}")).collect();
    let params: Vec<(i32, &str)> = (1..=count).zip(names.iter().map(String::as_str)).collect();
    (super::sql(sql, &binds(&params)).unwrap().0.into_iter())
        .filter_map(|piece| match piece {
            Piece::Param { name, .. } => Some(name),
            _ => None,
        })
        .collect()
}

#[test]
fn each_placeholder_is_the_next_parameter() {
    assert_eq!(bound("SELECT ? FROM t WHERE a = ?", 2), ["p1", "p2"]);
    let text = "SELECT 'é', ? WHERE x = ?";
    let sql = super::sql(text, &binds(&[(1, "a"), (2, "b")])).unwrap();
    assert_eq!(sql.text(), text);
}

#[test]
fn a_placeholder_is_only_code() {
    for sql in [
        "SELECT '?'",
        "SELECT 'it''s ?'",
        r"SELECT 'it\'s ?'",
        r#"SELECT "a?""#,
        r#"SELECT "it\"s ?""#,
        "SELECT `a?`",
        "SELECT 1 # ?\n",
        "SELECT 1 -- ?\n",
        "SELECT /* ? */ 1",
    ] {
        assert_eq!(bound(sql, 0), [""; 0], "{sql}");
    }
    // `--` without a space is two minus signs, not a comment.
    assert_eq!(bound("SELECT 1--?", 1), ["p1"]);
    // A backslash escapes the quote in a string, never in an identifier.
    assert_eq!(bound(r"SELECT `a\`, ?", 1), ["p1"]);
}

#[test]
fn a_mismatch_with_sqlcs_parameters_is_an_error() {
    let error = format!(
        "{:#}",
        super::sql("SELECT '?'", &binds(&[(1, "a")])).unwrap_err()
    );
    assert!(error.contains(r#"parameter ? #1 ("a")"#), "{error}");
    let error = format!("{:#}", super::sql("SELECT ?", &[]).unwrap_err());
    assert!(error.contains("uses ?"), "{error}");
}
