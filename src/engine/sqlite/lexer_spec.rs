//! Specification for finding sqlc's placeholders as SQLite's tokenizer does.

use super::*;
use crate::engine::{Piece, binds};

/// What `sql` binds where, with `$N` named `pN`.
fn cut(sql: &str, params: &[i32]) -> Result<Vec<Piece>> {
    let names: Vec<String> = params.iter().map(|n| format!("p{n}")).collect();
    let params: Vec<(i32, &str)> = (params.iter().copied())
        .zip(names.iter().map(String::as_str))
        .collect();
    super::sql(sql, &binds(&params)).map(|sql| sql.0)
}

/// The names of the parameters `sql` binds, in order.
fn bound(sql: &str, params: &[i32]) -> Vec<String> {
    (cut(sql, params).unwrap().into_iter())
        .filter_map(|piece| match piece {
            Piece::Param { name, .. } => Some(name),
            _ => None,
        })
        .collect()
}

#[test]
fn a_bare_placeholder_takes_the_number_after_the_largest_before_it() {
    assert_eq!(bound("SELECT ?, ?", &[1, 2]), ["p1", "p2"]);
    assert_eq!(bound("SELECT ?2, ?1, ?", &[1, 2, 3]), ["p2", "p1", "p3"]);
    assert_eq!(bound("WHERE a = ?1 OR b = ?1", &[1]), ["p1", "p1"]);
}

#[test]
fn a_statement_is_cut_back_to_sqlcs_text() {
    let text = "SELECT 'é', ?2 WHERE x = ?1";
    let sql = super::sql(text, &binds(&[(1, "a"), (2, "b")])).unwrap();
    assert_eq!(sql.text(), text);
}

#[test]
fn a_placeholder_is_only_code() {
    for sql in [
        "SELECT '?'",
        "SELECT 'it''s ?'",
        r#"SELECT "a?""b""#,
        "SELECT `a?`",
        "SELECT [a?]",
        "SELECT 1 -- ?\n",
        "SELECT /* ? /* */ 1",
    ] {
        assert_eq!(bound(sql, &[]), [""; 0], "{sql}");
    }
    assert_eq!(bound("SELECT /* ? */ ? -- ?\n", &[1]), ["p1"]);
}

#[test]
fn a_mismatch_with_sqlcs_parameters_is_an_error() {
    let error = |sql: &str, params: &[i32]| format!("{:#}", cut(sql, params).unwrap_err());
    assert!(error("SELECT ?", &[]).contains("uses ?"));
    assert!(error("SELECT '?'", &[1]).contains(r#"parameter ?1 ("p1")"#));
}

#[test]
fn a_named_parameter_sqlc_left_as_written_is_an_error() {
    for (sql, param) in [
        ("SELECT :a", ":a"),
        ("SELECT @b", "@b"),
        ("SELECT $c", "$c"),
    ] {
        let error = format!("{:#}", cut(sql, &[]).unwrap_err());
        assert!(
            error.contains(&format!("sqlc did not rewrite {param} into")),
            "{sql}: {error}"
        );
    }
    assert_eq!(bound("SELECT ':a', 'x:y', a$b", &[]), [""; 0]);
}
