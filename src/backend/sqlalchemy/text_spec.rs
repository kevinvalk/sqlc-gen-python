//! Specification for writing a statement as SQLAlchemy's `text()` takes it.

use super::*;
use crate::engine::Engine;

/// `sql`, whose parameters `$1`, `$2`, `$3` are named `id`, `ids`, `name`, cut
/// by PostgreSQL's lexer, then checked and written for `text()`.
fn written(sql: &str) -> Result<String> {
    let params: Vec<(i32, &str)> = (1..=3)
        .filter(|n| sql.contains(&format!("${n}")))
        .map(|n| (n, ["id", "ids", "name"][n as usize - 1]))
        .collect();
    let sql = Engine::PostgreSql.sql(sql, &params)?;
    check(&sql)?;
    Ok(text(&sql))
}

#[test]
fn a_placeholder_becomes_its_parameters_name_every_time_it_is_used() {
    assert_eq!(
        written("SELECT $1, $2 WHERE a = $1").unwrap(),
        "SELECT :id, :ids WHERE a = :id"
    );
}

#[test]
fn every_colon_of_the_statements_own_is_escaped() {
    // An escaped colon is never a bind, and `text()` always turns it back,
    // so one rule serves a cast after a placeholder, a cast after anything
    // else, a literal that would bind, and a backslash already there.
    for (sql, expected) in [
        ("SELECT $1::uuid[]", r"SELECT :id\:\:uuid[]"),
        ("SELECT x::numeric(12, 2)", r"SELECT x\:\:numeric(12, 2)"),
        (r#"SELECT '{"vip":true}'"#, r#"SELECT '{"vip"\:true}'"#),
        ("SELECT arr[1:2], 'a:b:c'", r"SELECT arr[1\:2], 'a\:b\:c'"),
        (r"SELECT '\:x'", r"SELECT '\\:x'"),
        ("SELECT 'é:x'", r"SELECT 'é\:x'"),
    ] {
        assert_eq!(written(sql).unwrap(), expected, "{sql}");
    }
}

#[test]
fn a_placeholder_touching_what_text_would_read_as_part_of_its_bind_is_an_error() {
    // SQLAlchemy binds `:name` only after none of `:`, `\` and a word
    // character, and its name runs on through every word character after:
    // PostgreSQL scans `$1a` as `$1` and then `a`, an alias, but `:ida` is
    // another bind, and nothing in `text()` can end a name. A digit before is
    // part of a word to `text()` too, though PostgreSQL reads a number.
    for (sql, problem) in [
        ("SELECT arr[1:$1]", "$1 directly after ':'"),
        (r"SELECT \$1", r"$1 directly after '\\'"),
        ("SELECT 1$1", "$1 directly after '1'"),
        ("SELECT 0x1F$1", "$1 directly after 'F'"),
        ("SELECT $1$2", "$2 directly after '1'"),
        ("SELECT $1a", "$1 directly before 'a'"),
        ("SELECT $1_0", "$1 directly before '_'"),
        ("SELECT $1N''", "$1 directly before 'N'"),
        ("SELECT $1é", "$1 directly before 'é'"),
    ] {
        let error = format!("{:#}", written(sql).unwrap_err());
        assert!(
            error.contains(&format!("cannot bind parameter {problem}")),
            "{sql}: {error}"
        );
    }
    // A dollar quote's `$` ends it, and is no word character.
    assert_eq!(written("SELECT $$x$$$1").unwrap(), "SELECT $$x$$:id");
}
