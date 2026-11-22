use super::*;

#[test]
fn a_mysql_statement_with_a_comment_that_is_code_is_not_read() {
    // MySQL runs what `/*! ... */` holds while our lexer leaves it out: this is a `LEFT JOIN`, so `b` may be missing
    let sql = "SELECT a.id, b.id FROM a /*! LEFT */ JOIN b ON b.a_id = a.id";
    assert_eq!(
        select::result_items(&Engine::MySql.words(sql), Engine::MySql, &[""; 2], &Default::default()),
        None
    );
    assert!(select::present_tables(&Engine::MySql.words(sql), Engine::MySql).is_empty());
    assert!(!select::single_row(
        Engine::MySql,
        &Engine::MySql.words("SELECT count(*) FROM a /*! GROUP BY id */")
    ));
    // MariaDB, which sqlc's mysql engine and the drivers serve too, runs `/*M! ... */`
    assert!(!select::single_row(
        Engine::MySql,
        &Engine::MySql.words("SELECT count(*) FROM a /*M! GROUP BY id */")
    ));
    // To PostgreSQL it is a comment
    assert_eq!(
        select::present_tables(&Engine::PostgreSql.words(sql), Engine::PostgreSql),
        ["a", "b"]
    );
}

#[test]
fn a_coalesce_a_nullif_or_a_case_is_found_as_code() {
    for (engine, sql) in [
        (Engine::Sqlite, "SELECT name, Coalesce(n, 0), offset FROM t"),
        (Engine::Sqlite, "SELECT CASE WHEN n THEN 1 END, offset FROM t"),
        // What an executable comment holds runs, so there we look in the text whole
        (Engine::MySql, "SELECT name, /*! nullif(n, 0) */ offset FROM t"),
    ] {
        assert!(holds_operand_typed(&engine.words(sql), sql), "{sql}");
    }
    for sql in [
        "SELECT name, offset FROM t WHERE name <> 'case'",
        "SELECT name, /* just in case */ offset FROM t",
        r#"SELECT "nullif", offset FROM t -- coalesce"#,
    ] {
        assert!(!holds_operand_typed(&Engine::Sqlite.words(sql), sql), "{sql}");
    }
}

#[test]
fn a_statement_reads_unless_it_or_a_statement_it_runs_first_writes() {
    for engine in [Engine::PostgreSql, Engine::MySql, Engine::Sqlite] {
        for sql in [
            "SELECT id FROM authors",
            "(SELECT 1) UNION (SELECT 2)",
            "VALUES (1), (2)",
            "WITH a AS (SELECT 1) SELECT * FROM a",
            "SELECT id FROM authors FOR UPDATE",
            "WITH a AS (SELECT id FROM authors FOR NO KEY UPDATE) SELECT * FROM a",
            "SELECT replace(name, 'a', 'b') FROM authors",
            "WITH a AS (SELECT name FROM authors) SELECT replace(name, 'a', 'b') FROM a",
            // A quoted name is no statement
            r#"WITH a AS (SELECT "delete" FROM t) SELECT * FROM a"#,
        ] {
            assert!(reads(&engine.words(sql)), "{engine:?}: {sql}");
        }
        for sql in [
            "DELETE FROM authors RETURNING id",
            "INSERT INTO authors (name) VALUES ('a') RETURNING id",
            "UPDATE authors SET name = 'a' RETURNING id",
            "WITH gone AS (DELETE FROM authors RETURNING id) SELECT * FROM gone",
            "REPLACE INTO authors (id) VALUES (1)",
            "WITH n AS (SELECT 1) REPLACE INTO authors (id) VALUES (1) RETURNING id",
        ] {
            assert!(!reads(&engine.words(sql)), "{engine:?}: {sql}");
        }
    }
}
