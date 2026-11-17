use super::*;

#[test]
fn postgresql_mysql_and_sqlite_are_supported() {
    let request = |engine: &str| plugin::GenerateRequest {
        settings: Some(plugin::Settings {
            engine: engine.into(),
            ..plugin::Settings::default()
        }),
        ..plugin::GenerateRequest::default()
    };
    for engine in [Engine::PostgreSql, Engine::Sqlite, Engine::MySql] {
        assert_eq!(Engine::of(&request(engine.name())).unwrap(), engine);
    }
    for engine in ["oracle", ""] {
        assert_eq!(
            Engine::of(&request(engine)).unwrap_err().to_string(),
            format!(r#"engine "{engine}" is not supported; only postgresql, mysql and sqlite are"#)
        );
    }
}

#[test]
fn a_statements_placeholders_must_be_exactly_the_parameters_sqlc_reported() {
    let cut = |engine: Engine, text: &str, params: &[Bind]| engine.sql(text, params).map_err(|error| error.to_string());
    let ids = binds(&[(1, "id")]);
    assert!(cut(Engine::PostgreSql, "SELECT $1", &ids).is_ok());
    // A parameter whose only placeholder is in a string
    let missing = cut(Engine::PostgreSql, "SELECT '$1'", &ids).unwrap_err();
    assert!(missing.contains("has no placeholder for it"), "{missing}");
    // A placeholder without a parameter would bind another's value
    let extra = cut(Engine::PostgreSql, "SELECT $1, $2", &ids).unwrap_err();
    assert!(extra.contains("$2, which sqlc reported no parameter for"), "{extra}");
    // On SQLite, where sqlc numbers a `sqlc.arg` among `?`s differently, the error says what to do
    let misnumbered = cut(Engine::Sqlite, "SELECT ?3, ?", &binds(&[(1, "a"), (2, "b")]));
    assert!(misnumbered.unwrap_err().contains("with sqlc.arg"));
    // A slice's placeholder follows the marker sqlc writes before it
    let slice = [Bind {
        number: 1,
        name: "ids",
        slice: Some("ids"),
    }];
    assert!(cut(Engine::Sqlite, "SELECT 1 WHERE id IN (/*SLICE:ids*/?)", &slice).is_ok());
    let unmarked = cut(Engine::Sqlite, "SELECT 1 WHERE id IN (?)", &slice).unwrap_err();
    assert!(unmarked.contains("has no marker /*SLICE:ids*/"), "{unmarked}");
    // Expanding the slice would rewrite the string too
    let spelt = cut(
        Engine::Sqlite,
        "SELECT '/*SLICE:ids*/' WHERE id IN (/*SLICE:ids*/?)",
        &slice,
    );
    assert!(
        spelt.unwrap_err().contains("where no sqlc.slice is"),
        "a marker in a string"
    );
}

#[test]
fn a_mysql_statement_with_a_comment_that_is_code_is_not_read() {
    // MySQL runs what `/*! ... */` holds while our lexer leaves it out: this is a `LEFT JOIN`, so `b` may be missing
    let sql = "SELECT a.id, b.id FROM a /*! LEFT */ JOIN b ON b.a_id = a.id";
    assert_eq!(Engine::MySql.items(sql, &[""; 2]), None);
    assert!(Engine::MySql.present(sql).is_empty());
    assert!(!Engine::MySql.single_row("SELECT count(*) FROM a /*! GROUP BY id */"));
    // To PostgreSQL it is a comment
    assert_eq!(Engine::PostgreSql.present(sql), ["a", "b"]);
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
            // A quoted name is no statement
            r#"WITH a AS (SELECT "delete" FROM t) SELECT * FROM a"#,
        ] {
            assert!(engine.reads(sql), "{engine:?}: {sql}");
        }
        for sql in [
            "DELETE FROM authors RETURNING id",
            "INSERT INTO authors (name) VALUES ('a') RETURNING id",
            "UPDATE authors SET name = 'a' RETURNING id",
            "WITH gone AS (DELETE FROM authors RETURNING id) SELECT * FROM gone",
            "REPLACE INTO authors (id) VALUES (1)",
        ] {
            assert!(!engine.reads(sql), "{engine:?}: {sql}");
        }
    }
}

#[test]
fn a_merge_is_found_as_code() {
    for sql in [
        "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
        "WITH s AS (SELECT 1) merge /* c */ into t USING s ON true WHEN MATCHED THEN DELETE",
    ] {
        assert!(Engine::PostgreSql.merges(sql), "{sql}");
    }
    for sql in [
        "SELECT 'MERGE INTO'",
        "SELECT merge FROM t -- merge into\n",
        r#"SELECT "merge" into"#,
    ] {
        assert!(!Engine::PostgreSql.merges(sql), "{sql}");
    }
}
