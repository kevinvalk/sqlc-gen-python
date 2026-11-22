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
    // sqlc reports no parameter for a `?` in a SQLite HAVING, where a sqlc.arg would not be one either
    let unread = cut(Engine::Sqlite, "SELECT 1 GROUP BY 1 HAVING count(*) > ?", &[]).unwrap_err();
    assert!(
        unread.ends_with("ON CONFLICT ... DO UPDATE, sqlc reads no parameter at all"),
        "{unread}"
    );
    // Nor on MySQL for a slice of an expression, and what an executable comment holds is no code to us
    let expression = cut(Engine::MySql, "SELECT 1 WHERE id + 1 IN (/*SLICE:ids*/?)", &[]).unwrap_err();
    assert!(
        expression.contains("a sqlc.slice without a plain column on its left"),
        "{expression}"
    );
    let executable = cut(
        Engine::MySql,
        "SELECT ? /*! AND a = ? */",
        &binds(&[(1, "a"), (2, "b")]),
    )
    .unwrap_err();
    assert!(
        executable.contains("we read no placeholder inside /*! ... */"),
        "{executable}"
    );
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
        spelt
            .unwrap_err()
            .contains("where sqlc reported no sqlc.slice, as in a string or a comment"),
        "a marker in a string"
    );
    // SQLite numbers the second use of a slice 3, which sqlc reports as `name`: no string or comment is involved
    let reused = cut(
        Engine::Sqlite,
        "SELECT 1 WHERE a IN (/*SLICE:ids*/?) AND b = ?2 AND (c IN (/*SLICE:ids*/?) OR d = ?3)",
        &[slice[0], binds(&[(2, "qty")])[0], binds(&[(3, "name")])[0]],
    );
    assert!(
        reused.unwrap_err().contains("use a sqlc.slice once"),
        "a slice used twice"
    );
}

#[test]
fn a_sqlc_call_sqlc_left_in_a_postgresql_statement_is_an_error() {
    // sqlc does not analyse MERGE
    for (sql, call) in [
        (
            "MERGE INTO a USING b ON true WHEN MATCHED THEN UPDATE SET n = sqlc.arg(n)",
            "sqlc.arg",
        ),
        ("SELECT SQLC . narg ('x')", "SQLC.narg"),
        ("SELECT id FROM a WHERE id = ANY(sqlc.slice(ids))", "sqlc.slice"),
        ("SELECT sqlc.embed(a) FROM a", "sqlc.embed"),
    ] {
        let error = Engine::PostgreSql.sql(sql, &[]).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!(
                "sqlc did not rewrite {call}, as it rewrites none in a statement"
            )),
            "{sql}: {error}"
        );
    }
    // A column of a table named sqlc, another function of the schema, and what is not code
    for sql in [
        "SELECT sqlc.arg, sqlc.slice FROM sqlc",
        "SELECT sqlc.argument, sqlc_arg(1)",
        r#"SELECT 'sqlc.arg(a)', "sqlc".arg(1) -- sqlc.arg(a)"#,
    ] {
        assert!(Engine::PostgreSql.sql(sql, &[]).is_ok(), "{sql}");
    }
}

#[test]
fn a_sqlc_call_sqlc_left_in_a_sqlite_statement_is_an_error() {
    // What sqlc 1.31.1 sends for each: SQLite would fail on the call at `near "("`
    for (sql, params, call) in [
        (
            "SELECT grp, count(*) AS c FROM t GROUP BY grp HAVING count(*) >= sqlc.arg(at_least)",
            vec![],
            "sqlc.arg",
        ),
        (
            "SELECT id FROM t ORDER BY CASE WHEN sqlc.narg(by_name) THEN name ELSE grp END",
            vec![],
            "sqlc.narg",
        ),
        (
            "INSERT INTO t (id, name) VALUES (?1, ?2) ON CONFLICT (id) DO UPDATE SET name = sqlc.arg(new_name)",
            binds(&[(1, "id"), (2, "name")]),
            "sqlc.arg",
        ),
    ] {
        let error = Engine::Sqlite.sql(sql, &params).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!(
                "sqlc did not rewrite {call}, as on SQLite it reads no parameter in some clauses, such as HAVING"
            )),
            "{sql}: {error}"
        );
    }
    for sql in [
        "SELECT sqlc.arg, sqlc.slice FROM sqlc",
        "SELECT 'sqlc.arg(a)', [sqlc].arg -- sqlc.arg(a)",
    ] {
        assert!(Engine::Sqlite.sql(sql, &[]).is_ok(), "{sql}");
    }
}

#[test]
fn a_sqlc_call_sqlc_left_in_a_mysql_statement_is_an_error() {
    for (sql, call) in [
        (
            "SELECT id FROM users WHERE id > ANY (SELECT u.id FROM users u WHERE u.id = sqlc.arg(id))",
            "sqlc.arg",
        ),
        (
            "SELECT id FROM users WHERE id > ALL (SELECT u.id FROM users u WHERE u.id IN (sqlc.slice(ids)))",
            "sqlc.slice",
        ),
        // An executable comment hides no call outside it
        (
            "SELECT /*! STRAIGHT_JOIN */ id FROM users WHERE id > ANY (SELECT u.id FROM users u WHERE u.id = sqlc.arg(id))",
            "sqlc.arg",
        ),
    ] {
        let error = Engine::MySql.sql(sql, &[]).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!("sqlc did not rewrite {call}, as on MySQL")),
            "{sql}: {error}"
        );
    }
    for sql in [
        "SELECT sqlc.arg, sqlc.slice FROM sqlc",
        "SELECT 'sqlc.arg(a)', `sqlc`.arg # sqlc.arg(a)",
    ] {
        assert!(Engine::MySql.sql(sql, &[]).is_ok(), "{sql}");
    }
}

#[test]
fn a_mysql_statement_with_a_comment_that_is_code_is_not_read() {
    // MySQL runs what `/*! ... */` holds while our lexer leaves it out: this is a `LEFT JOIN`, so `b` may be missing
    let sql = "SELECT a.id, b.id FROM a /*! LEFT */ JOIN b ON b.a_id = a.id";
    assert_eq!(Engine::MySql.items(sql, &[""; 2], &Default::default()), None);
    assert!(Engine::MySql.present(sql).is_empty());
    assert!(!Engine::MySql.single_row("SELECT count(*) FROM a /*! GROUP BY id */"));
    // MariaDB, which sqlc's mysql engine and the drivers serve too, runs `/*M! ... */`
    assert!(!Engine::MySql.single_row("SELECT count(*) FROM a /*M! GROUP BY id */"));
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
fn a_coalesce_a_nullif_or_a_case_is_found_as_code() {
    for (engine, sql) in [
        (Engine::Sqlite, "SELECT name, Coalesce(n, 0), offset FROM t"),
        (Engine::Sqlite, "SELECT CASE WHEN n THEN 1 END, offset FROM t"),
        // What an executable comment holds runs, so there we look in the text whole
        (Engine::MySql, "SELECT name, /*! nullif(n, 0) */ offset FROM t"),
    ] {
        assert!(engine.holds_operand_typed(sql), "{sql}");
    }
    for sql in [
        "SELECT name, offset FROM t WHERE name <> 'case'",
        "SELECT name, /* just in case */ offset FROM t",
        r#"SELECT "nullif", offset FROM t -- coalesce"#,
    ] {
        assert!(!Engine::Sqlite.holds_operand_typed(sql), "{sql}");
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
