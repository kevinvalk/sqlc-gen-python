//! Specification for the SQLAlchemy backend.

use super::*;

fn postgresql(sql: &str) -> String {
    sql_for(sql, Engine::PostgreSql)
}

fn sql_for(sql: &str, engine: Engine) -> String {
    super::sql(sql, engine)
}

#[test]
fn postgresql_placeholders_keep_their_own_numbering() {
    assert_eq!(
        postgresql("SELECT * FROM authors WHERE id = $1 AND name = $2"),
        "SELECT * FROM authors WHERE id = :p1 AND name = :p2"
    );
}

#[test]
fn a_placeholder_past_nine_keeps_all_its_digits() {
    assert_eq!(
        postgresql("VALUES ($9, $10, $11)"),
        "VALUES (:p9, :p10, :p11)"
    );
}

#[test]
fn placeholders_may_repeat_and_appear_out_of_order() {
    // sqlc reuses a number when one argument feeds two positions.
    assert_eq!(
        postgresql("$2 = $1 OR $2 IS NULL"),
        ":p2 = :p1 OR :p2 IS NULL"
    );
}

#[test]
fn a_dollar_that_starts_no_number_is_left_alone() {
    // Dollar-quoted bodies are not placeholders and must survive verbatim.
    assert_eq!(postgresql("$$ SELECT 1 $$"), "$$ SELECT 1 $$");
    assert_eq!(postgresql("$tag$ body $tag$"), "$tag$ body $tag$");
}

#[test]
fn positional_engines_number_placeholders_by_order() {
    for engine in [Engine::MySql, Engine::Sqlite] {
        assert_eq!(
            sql_for("SELECT a FROM foo WHERE b = ? LIMIT ?", engine),
            "SELECT a FROM foo WHERE b = :p1 LIMIT :p2"
        );
    }
}

#[test]
fn a_colon_already_in_the_query_is_escaped() {
    // `text()` would otherwise read `::text` as a bind parameter named `:text`.
    assert_eq!(postgresql("SELECT $1::text"), r"SELECT :p1\:\:text");
}

#[test]
fn escaping_never_touches_the_placeholders_it_introduces() {
    let rewritten = postgresql("WHERE id = $1");
    assert!(rewritten.contains(":p1"), "{rewritten}");
    assert!(!rewritten.contains(r"\:p1"), "{rewritten}");
}

#[test]
fn the_connection_type_follows_the_async_setting() {
    assert_eq!(
        connection(false),
        Type::Qualified {
            module: "sqlalchemy.engine",
            attribute: "Connection"
        }
    );
    assert_eq!(
        connection(true),
        Type::Qualified {
            module: "sqlalchemy.ext.asyncio",
            attribute: "AsyncConnection"
        }
    );
}
