use super::*;
use crate::api::fixtures::authors::{resolve, *};
use crate::api::fixtures::*;
use crate::backend::Backend;

#[test]
fn each_query_file_is_a_module_named_after_its_stem_in_sqlc_order() {
    let file = |name: &str, filename: &str| plugin::Query {
        name: name.into(),
        filename: filename.into(),
        ..query(":exec", "SELECT 1")
    };
    let modules = try_resolve_all(vec![file("A", "b.sql"), file("B", "a.sql"), file("C", "b.sql")]).unwrap();
    let shape: Vec<(&str, Vec<&str>)> = (modules.iter())
        .map(|m| (m.name.as_str(), m.queries.iter().map(|q| q.function.as_str()).collect()))
        .collect();
    assert_eq!(shape, [("b", vec!["a", "c"]), ("a", vec!["b"])]);
}

#[test]
fn a_sqlite_query_from_the_first_non_ascii_text_of_its_file_on_is_an_error() {
    let query = |name: &str, filename: &str, comment: &str| plugin::Query {
        name: name.into(),
        filename: filename.into(),
        comments: vec![comment.into()],
        ..query(":exec", "SELECT 1")
    };
    let queries = vec![
        query("A", "a.sql", "plain"),
        query("B", "a.sql", "café"),
        query("C", "a.sql", "plain"),
        query("D", "b.sql", "plain"),
    ];
    let error = error_of(try_resolve_on("sqlite", queries.clone()));
    assert!(error.contains(r#"a.sql: query "B": sqlc misplaces"#), "{error}");
    assert!(!error.contains(r#"query "C""#), "one problem per file: {error}");

    let resolved = try_resolve_on("sqlite", vec![queries[0].clone(), queries[3].clone()]).unwrap();
    assert_eq!(resolved.len(), 2);
    assert!(try_resolve_on("postgresql", queries).is_ok());
}

#[test]
fn a_query_file_that_would_replace_a_generated_module_is_an_error() {
    // On a case-insensitive file system, `Models.py` is `models.py`.
    for (filename, stem) in [
        ("models.sql", "models"),
        ("Models.sql", "Models"),
        ("__init__.sql", "__init__"),
        ("__INIT__.sql", "__INIT__"),
    ] {
        let error = error(plugin::Query {
            filename: filename.into(),
            ..query(":exec", "SELECT 1")
        });
        let message = format!(r#"would overwrite the generated module "{stem}.py""#);
        assert!(error.contains(&message), "{error}");
    }
}

#[test]
fn a_query_file_whose_stem_is_no_lowercase_module_name_is_an_error() {
    // ruff N999; and `Invoices.py` beside `invoices.py` would be one file on a
    // case-insensitive file system.
    for stem in ["query.v2", "Invoices", "lineItems", "1st"] {
        let error = error(plugin::Query {
            filename: format!("{stem}.sql"),
            ..query(":exec", "SELECT 1")
        });
        let message = format!(r#""{stem}" is not a lowercase Python module name"#);
        assert!(error.contains(&message), "{error}");
    }
}

#[test]
fn a_hyphen_in_a_query_files_name_is_an_underscore_unless_two_files_meet() {
    let file = |filename: &str| plugin::Query {
        filename: filename.into(),
        ..query(":exec", "SELECT 1")
    };
    let modules = try_resolve_all(vec![file("ws-requests.sql")]).unwrap();
    assert_eq!(modules[0].name, "ws_requests");
    let error = format!(
        "{:#}",
        try_resolve_all(vec![file("a-b.sql"), file("a_b.sql")]).unwrap_err()
    );
    assert!(error.contains(r#"both become the module "a_b.py""#), "{error}");
}

#[test]
fn every_problem_is_reported_at_once() {
    let guessed = |name: &str| plugin::Query {
        name: name.into(),
        columns: vec![column("v", "int", true)],
        ..query(":one", "SELECT price * 1.5 AS total FROM sales")
    };
    let error = format!("{:#}", try_resolve_all(vec![guessed("A"), guessed("B")]).unwrap_err());
    assert!(error.starts_with("2 problems:"), "{error}");
    assert!(
        error.contains(r#"query "A""#) && error.contains(r#"query "B""#),
        "{error}"
    );
}

#[test]
fn what_to_do_about_several_problems_is_said_once_under_them() {
    let column = |name: &str, data_type: &str| plugin::Query {
        name: name.into(),
        columns: vec![column("value; net", data_type, true)],
        ..query(":one", "SELECT v FROM t")
    };
    let queries = vec![column("A", "geometry"), column("B", "integer"), column("C", "geometry")];
    let error = format!("{:#}", try_resolve_all(queries).unwrap_err());
    assert_eq!(
        error,
        r#"3 problems:
authors.sql: query "A": column "value; net" has type "geometry", which has no verified Python mapping
authors.sql: query "C": column "value; net" has type "geometry", which has no verified Python mapping
  name its Python type with an override (db_type "geometry"), or, for a domain, declare the column with its base type and a CHECK constraint; or set unknown_type: Any to leave it unchecked
authors.sql: query "B": column "value; net" has a type sqlc could not infer
  add a cast, as <expr>::<type>, or coalesce(<expr>, <default>)::<type> if it can be NULL, as sqlc reports every cast NOT NULL; with a database block in sqlc.yaml, also set analyzer: {database: false}, as only sqlc's static analysis can be told from a guess; or set unknown_type: Any to leave it unchecked"#
    );
}

#[test]
fn queries_that_spell_one_function_are_an_error() {
    let named = |name: &str| plugin::Query {
        name: name.into(),
        ..query(":exec", "SELECT 1")
    };
    let error = format!(
        "{:#}",
        try_resolve_all(vec![named("GetA"), named("Get_A")]).unwrap_err()
    );
    assert!(error.contains(r#"both become the Python name "get_a""#), "{error}");
}

#[test]
fn queries_that_spell_one_class_are_an_error() {
    // `get1` and `get_1` are two functions, but both rows are `Get1Row`: the
    // second class would silently replace the first.
    let row = |name: &str| plugin::Query {
        name: name.into(),
        columns: authors()
            .into_iter()
            .map(of_authors)
            .take(2)
            .chain([column("x", "text", true)])
            .collect(),
        ..query(":one", "SELECT 1")
    };
    let copy = |name: &str| plugin::Query {
        name: name.into(),
        ..query(":copyfrom", "SELECT 1")
    };
    for queries in [vec![row("Get1"), row("Get_1")], vec![copy("Add1"), copy("Add_1")]] {
        let error = format!("{:#}", try_resolve_all(queries).unwrap_err());
        assert!(error.contains("both become the Python name \""), "{error}");
    }
}

#[test]
fn a_copyfrom_copies_only_a_statement_that_binds_each_parameter_once() {
    let copy = |text: &str, params: Vec<plugin::Parameter>| {
        resolve(plugin::Query {
            insert_into_table: Some(plugin::Identifier {
                name: "authors".into(),
                ..Default::default()
            }),
            params,
            ..query(":copyfrom", text)
        })
    };
    let name = || param(1, column("name", "text", true));
    let bio = || param(2, column("bio", "text", false));
    let both = copy("INSERT INTO authors (name, bio) VALUES ($1, $2)", vec![name(), bio()]);
    let columns = both.copy_into.map(|into| into.columns);
    assert_eq!(columns, Some(vec!["name".to_string(), "bio".to_string()]));
    // `COPY` would write one column for the two the statement inserts.
    let twice = copy("INSERT INTO authors (name, bio) VALUES ($1, $1)", vec![name()]);
    assert_eq!(twice.copy_into, None);
}

#[test]
fn a_copyfrom_copies_no_statement_that_discards_the_values_of_identity_columns() {
    let copy = |overriding: &str| {
        let text = format!("INSERT INTO authors (id, name) {overriding} VALUES ($1, $2)");
        resolve(plugin::Query {
            insert_into_table: Some(plugin::Identifier {
                name: "authors".into(),
                ..Default::default()
            }),
            params: vec![
                param(1, column("id", "pg_catalog.int8", true)),
                param(2, column("name", "text", true)),
            ],
            ..query(":copyfrom", &text)
        })
    };
    // `COPY` stores the id it is given, where this statement takes the sequence's
    assert_eq!(copy("OVERRIDING USER VALUE").copy_into, None);
    assert_eq!(copy("overriding /* c */ user\nvalue").copy_into, None);
    // As `COPY` does by itself
    assert!(copy("OVERRIDING SYSTEM VALUE").copy_into.is_some());
    assert!(copy("").copy_into.is_some());
}

#[test]
fn a_query_is_a_snake_case_function_a_private_constant_and_perhaps_a_class() {
    let query = resolve(query(":exec", "SELECT 1"));
    assert_eq!(
        (query.function.as_str(), query.constant.as_str(), query.classes().next()),
        ("get_author", "_GET_AUTHOR", None)
    );
    let copy = resolve(self::query(":copyfrom", "SELECT 1"));
    assert_eq!(copy.takes, Takes::Rows("GetAuthorParams".into()));
    let Command::One(Shape::Row(class, _)) =
        returning(":one", vec![column("bio", "text", false), column("name", "text", true)])
    else {
        panic!("not a row");
    };
    assert_eq!(class, "GetAuthorRow");
}

#[test]
fn a_function_named_like_a_keyword_reserved_name_or_builtin_takes_a_trailing_underscore() {
    // A builtin of any Python it runs on: `sentinel` is new in 3.15
    for (name, function) in [
        ("Import", "import_"),
        ("Models", "models_"),
        ("Any", "any_"),
        ("Sentinel", "sentinel_"),
    ] {
        let query = resolve(plugin::Query {
            name: name.into(),
            ..query(":exec", "SELECT 1")
        });
        assert_eq!(query.function, function);
    }
}

#[test]
fn a_function_keeps_the_name_of_a_local_of_its_body() {
    for name in ["Row", "Value", "Cur", "Connection"] {
        let query = resolve(plugin::Query {
            name: name.into(),
            ..query(":exec", "SELECT 1")
        });
        assert_eq!(query.function, name.to_lowercase());
    }
}

#[test]
fn a_query_whose_name_becomes_no_public_python_name_is_an_error() {
    let error = error(plugin::Query {
        name: "Größe".into(),
        ..query(":exec", "SELECT 1")
    });
    assert!(
        error.contains(r#""Größe" does not become a usable Python name; name the query otherwise"#),
        "{error}"
    );
}

#[test]
fn a_query_comment_loses_the_space_sqlc_puts_before_each_line() {
    let query = resolve(plugin::Query {
        comments: vec![" One.".into(), "  Two, indented.".into()],
        ..query(":exec", "SELECT 1")
    });
    assert_eq!(query.comment, "One.\n Two, indented.");
}

#[test]
fn errors_name_the_file_and_the_query() {
    let error = error(query(":execlastid", "SELECT 1"));
    assert!(error.starts_with(r#"authors.sql: query "GetAuthor": "#), "{error}");
}

#[test]
fn commands_an_engine_cannot_run_are_errors_that_say_what_to_use() {
    assert!(
        error(query(":execlastid", ""))
            .contains(":execlastid is not supported on postgresql; use INSERT ... RETURNING with :one")
    );
    // A batch runs per row of its parameters, on PostgreSQL alone.
    for batch in [":batchexec", ":batchmany", ":batchone"] {
        let error = error(query(batch, ""));
        assert!(error.contains("has no parameters to run it per row"), "{error}");
        let error = format!("{:#}", on("sqlite", query(batch, "")).unwrap_err());
        assert!(error.contains(&format!("{batch} is PostgreSQL only")), "{error}");
    }
    assert!(error(query(":frobnicate", "")).contains(":frobnicate"));
}

#[test]
fn a_merge_cannot_count_its_rows() {
    // asyncpg's count of a MERGE is -1, psycopg's the real one.
    let merge = "MERGE INTO authors a USING (SELECT 1 AS id) s ON a.id = s.id \
                 WHEN MATCHED THEN DELETE";
    for cmd in [":execrows", ":execresult"] {
        let error = error(query(cmd, merge));
        assert!(error.contains(&format!("{cmd} counts -1 rows for a MERGE")), "{error}");
    }
    resolve(query(":exec", merge));
    resolve(query(":execrows", "DELETE FROM authors WHERE name = 'merge into'"));
}

#[test]
fn a_mysql_comment_marker_in_a_string_preserves_the_stream_and_row_type() {
    for marker in ["/*!", "/*M!"] {
        let query = on(
            "mysql",
            plugin::Query {
                columns: vec![of_authors(column("id", "bigint", true))],
                ..query(":many", &format!("SELECT id FROM authors WHERE name = '{marker}'"))
            },
        )
        .unwrap();
        assert_eq!(query.stream.as_deref(), Some("stream_get_author"));
        assert!(!query.columns[0].ty.optional);
    }
}

#[test]
fn a_mysql_copyfrom_has_no_percent_sign() {
    // sqlc accepts ON DUPLICATE KEY UPDATE in a :copyfrom, where PyMySQL
    // before 1.2.4 leaves a doubled `%` doubled: 'x%%' would be stored.
    let copy = |text: &str| {
        on(
            "mysql",
            plugin::Query {
                params: vec![named(1, "name", "text", true)],
                ..query(":copyfrom", text)
            },
        )
    };
    let error = error_of(copy(
        "INSERT INTO authors (name) VALUES (?) ON DUPLICATE KEY UPDATE name = 'x%'",
    ));
    assert!(
        error.ends_with(
            ":copyfrom has a % in its statement, which a MySQL driver's executemany doubles or \
             not by its version; write it without one, as MOD(a, b) for a % b"
        ),
        "{error}"
    );
    copy("INSERT INTO authors (name) VALUES (?)").unwrap();
}

#[test]
fn a_mysql_copyfrom_binds_nothing_after_on_duplicate_key_update() {
    // The drivers' `executemany` formats the VALUES list alone, and raises a TypeError for a third value.
    let copy = |text: &str, count: i32| {
        on(
            "mysql",
            plugin::Query {
                params: (1..=count).map(|n| named(n, "qty", "int", true)).collect(),
                ..query(":copyfrom", text)
            },
        )
    };
    for text in [
        "INSERT INTO items (sku, qty) VALUES (?, ?) ON DUPLICATE KEY UPDATE qty = qty + ?",
        // An executable comment does not hide the clause from the driver
        "INSERT /*! IGNORE */ INTO items (sku, qty) VALUES (?, ?) ON DUPLICATE KEY UPDATE qty = qty + ?",
    ] {
        let error = error_of(copy(text, 3));
        assert!(
            error.ends_with(
                ":copyfrom binds a parameter after ON DUPLICATE KEY UPDATE, where a MySQL driver's \
                 executemany formats none and raises; write the row's own value as VALUES(column), \
                 or use :exec, once per row"
            ),
            "{text}: {error}"
        );
    }
    copy(
        "INSERT INTO items (sku, qty) VALUES (?, ?) ON DUPLICATE KEY UPDATE qty = qty + VALUES(qty)",
        2,
    )
    .unwrap();
}

#[test]
fn a_copyfrom_takes_no_slice() {
    // sqlc accepts one, and `executemany` would run the statement with its
    // marker still in it.
    let slice = plugin::Column {
        is_sqlc_slice: true,
        ..column("ids", "integer", true)
    };
    let error = error_of(on(
        "sqlite",
        plugin::Query {
            params: vec![param(1, slice)],
            ..query(":copyfrom", "INSERT INTO t (a) VALUES (/*SLICE:ids*/?)")
        },
    ));
    assert!(
        error.ends_with(
            ":copyfrom runs one statement for every row, and a sqlc.slice makes it another for \
             each number of items; use :exec, once per row"
        ),
        "{error}"
    );
}

#[test]
fn a_type_sqlc_only_guessed_is_an_error_that_asks_for_a_cast() {
    // sqlc's static analyser reports `quantity * unit_price` on numeric
    // columns as a bare `int`, and `abs(unit_price)` as the `bigint` of an
    // overload it guessed: PostgreSQL returns a Decimal for both. Read
    // through a CTE or a subquery, the call loses `is_func_call` but keeps
    // the guess, in SQL's standard spelling: `avg(unit_price)` is a
    // `double precision`.
    // A call's own type need not look like a guess: sqlc reports
    // `string_agg(name, ',')` as `bytea`, which PostgreSQL returns as `str`.
    let expression = column("total", "int", true);
    let call = plugin::Column {
        is_func_call: true,
        ..column("names", "bytea", true)
    };
    let through_cte = plugin::Column {
        table: Some(plugin::Identifier {
            name: "t".into(),
            ..Default::default()
        }),
        ..column("mean", "double precision", true)
    };
    let through_subquery = column("s", "bigint", true);
    for column in [expression, call, through_cte, through_subquery] {
        let name = column.name.clone();
        let call = column.is_func_call;
        let error = error(plugin::Query {
            columns: vec![column],
            ..query(":one", "SELECT price * 1.5 AS total FROM sales")
        });
        let problem = match call {
            true => "is a function's result, which sqlc types by the function's first overload",
            false => "has a type sqlc could not infer",
        };
        assert!(
            error.contains(&format!(r#"column "{name}" {problem}"#))
                && error.contains("add a cast, as <expr>::<type>, or coalesce("),
            "{error}"
        );
    }
}

#[test]
fn a_bare_integer_is_a_type_sqlc_read_only_of_a_parameter_without_a_column() {
    // sqlc spells a column's own type `pg_catalog.int4`. Through a CTE,
    // `age(created_at)`, an interval, is the `integer` of `age(xid)`, the
    // function's first overload; and sqlc's database analysis spells every
    // `int4` so, with a NOT NULL nothing proves. A `LIMIT`'s parameter is
    // sqlc's own `integer`.
    let through = |table: &str| plugin::Identifier {
        name: table.into(),
        ..Default::default()
    };
    let through_cte = plugin::Column {
        table: Some(through("c")),
        ..column("a", "integer", true)
    };
    let through_subquery = column("a", "integer", true);
    for column in [through_cte.clone(), through_subquery.clone()] {
        let error = error(plugin::Query {
            columns: vec![column],
            ..query(":one", "SELECT 1")
        });
        assert!(
            error.contains(r#"column "a" has a type sqlc could not infer"#)
                && error.contains("also set analyzer: {database: false}"),
            "{error}"
        );
    }
    let taking = |parameter: plugin::Column| {
        try_resolve(plugin::Query {
            params: vec![param(1, parameter)],
            ..query(":exec", "SELECT 1 LIMIT $1")
        })
    };
    let error = error_of(taking(through_cte));
    assert!(
        error.contains(r#"parameter $1 ("a") has a type sqlc could not infer"#),
        "{error}"
    );
    let limit = taking(through_subquery).unwrap();
    assert_eq!(limit.params[0].ty.describe(crate::python::Scope::Queries), "int");
}

#[test]
fn nullable_expressions_survive_a_cte_a_derived_table_and_an_outer_join() {
    let sql = "WITH contact(id, label) AS (SELECT id, (bio || 'x')::text FROM AUTHORS),
                    renamed AS (SELECT id, label AS address FROM contact)
               SELECT a.id, r.address, (r.id + 1)::bigint
               FROM authors a LEFT JOIN (SELECT id, address FROM renamed) AS r ON r.id = a.id";
    let resolved = resolve(plugin::Query {
        columns: vec![
            column("id", "pg_catalog.int8", true),
            column("address", "text", true),
            column("next", "pg_catalog.int8", true),
        ],
        ..query(":many", sql)
    });
    assert_eq!(
        resolved.columns.iter().map(|c| c.ty.optional).collect::<Vec<_>>(),
        [false, true, true]
    );
}

#[test]
fn each_set_operation_arm_must_supply_the_same_type_and_can_supply_nulls() {
    let read = |sql| {
        try_resolve(plugin::Query {
            columns: vec![column("v", "pg_catalog.int8", true)],
            ..query(":many", sql)
        })
    };
    for sql in [
        "SELECT id AS v FROM authors UNION ALL SELECT id::numeric FROM authors",
        "WITH a AS (SELECT id FROM authors EXCEPT SELECT id::numeric FROM authors) SELECT id AS v FROM a",
    ] {
        assert!(error_of(read(sql)).contains("set-operation branches"), "{sql}");
    }
    let rows = read("SELECT id AS v FROM authors UNION ALL SELECT NULL::bigint").unwrap();
    assert!(rows.columns[0].ty.optional);
    let rows = read("SELECT id AS v FROM authors INTERSECT SELECT id::bigint FROM authors").unwrap();
    assert!(!rows.columns[0].ty.optional);
}

#[test]
fn a_recursive_postgres_cte_keeps_the_anchor_type_and_allows_null_in_later_rows() {
    let result = resolve(plugin::Query {
        columns: vec![column("n", "pg_catalog.int8", true)],
        ..query(
            ":many",
            "WITH RECURSIVE r(n) AS (SELECT id FROM authors UNION ALL SELECT NULL::bigint FROM r WHERE n IS NOT NULL) SELECT n FROM r",
        )
    });
    assert!(result.columns[0].ty.optional);
}

#[test]
fn a_not_null_column_read_from_a_nullable_one_is_a_coalesce_that_needs_a_cast() {
    // sqlc reports `coalesce(bio, 1.5)` as `bio` itself, NOT NULL: its first
    // argument's table, name and type, which PostgreSQL may well not return.
    // Read from a subquery, that report is all that shows it.
    let coalesced = plugin::Column {
        original_name: "bio".into(),
        ..of_authors(column("v", "text", true))
    };
    let error = error(plugin::Query {
        columns: vec![coalesced.clone()],
        ..query(
            ":one",
            "SELECT bio AS v FROM (SELECT coalesce(bio, 1.5) AS bio FROM authors) a",
        )
    });
    assert!(
        error.contains(r#"column "v" comes from a derived expression"#),
        "{error}"
    );
    // The column itself, or a coalesce of a NOT NULL one, is the column.
    for column in [
        plugin::Column {
            not_null: false,
            ..coalesced
        },
        plugin::Column {
            original_name: "name".into(),
            ..of_authors(column("name", "text", true))
        },
    ] {
        resolve(plugin::Query {
            columns: vec![column],
            ..query(":one", "SELECT 1")
        });
    }
}

#[test]
fn a_coalesce_a_nullif_or_a_case_alone_needs_a_cast_whatever_sqlc_reports() {
    // sqlc reports `nullif(name, '')` as a NOT NULL `bool`, `coalesce(bio, name)` as `bio` itself, and a `CASE` as
    // its `ELSE`: types as sure as a column's own, which the driver does not return.
    let bio = plugin::Column {
        original_name: "bio".into(),
        ..of_authors(column("v", "text", false))
    };
    for (sql, column) in [
        ("SELECT nullif(name, '') AS v FROM authors", column("v", "bool", true)),
        ("SELECT coalesce(bio, name) AS v FROM authors", bio.clone()),
        ("SELECT (coalesce(bio, name)) AS v FROM authors", bio),
        (
            "SELECT CASE WHEN id > 1 THEN born ELSE '2020-01-01' END AS v FROM authors",
            column("v", "text", true),
        ),
    ] {
        let error = error(plugin::Query {
            columns: vec![column.clone()],
            ..query(":one", sql)
        });
        assert!(
            error.contains(
                r#"column "v" is a coalesce, a nullif or a CASE, which sqlc types by one operand whatever the others are; add a cast, as <expr>::<type>, or coalesce("#
            ) && error.contains("also set analyzer: {database: false}"),
            "{error}"
        );
    }
    // A cast names the type, so sqlc's report of it stands.
    for sql in [
        "SELECT nullif(name, '')::text AS v FROM authors",
        "SELECT CAST(coalesce(bio, name) AS text) AS v FROM authors",
        "SELECT (CASE WHEN id > 1 THEN 'a' ELSE 'b' END)::text AS v FROM authors",
    ] {
        resolve(plugin::Query {
            columns: vec![column("v", "text", true)],
            ..query(":one", sql)
        });
    }
}

#[test]
fn a_list_we_cannot_read_is_an_error_where_the_statement_holds_a_coalesce_a_nullif_or_a_case() {
    // SQLite and MySQL take `offset` as a column's name, where we take it as the end of the list. sqlc reports the
    // `coalesce` as `bio` itself and the `nullif` as a NOT NULL `bool`, and nothing tells which column either is.
    let offset = of_authors(column("offset", "text", true));
    let bio = plugin::Column {
        original_name: "bio".into(),
        ..of_authors(column("v", "text", true))
    };
    for (sql, column) in [
        ("SELECT coalesce(bio, 1.5) AS v, offset FROM authors", bio),
        (
            "SELECT nullif(name, '') AS v, offset FROM authors",
            column("v", "bool", true),
        ),
        (
            "SELECT CASE WHEN id > 1 THEN bio ELSE 'x' END AS v, offset FROM authors",
            column("v", "text", true),
        ),
    ] {
        let error = error(plugin::Query {
            columns: vec![column, offset.clone()],
            ..query(":one", sql)
        });
        assert!(
            error.contains(
                "has a result list we could not read, so its coalesce, nullif or CASE, which sqlc types by one \
                 operand whatever the others are, may be any of its columns; if a column is named as a keyword, as \
                 \"offset\", quote it"
            ),
            "{error}"
        );
    }
    // Without one of them every column is as sqlc reports it, and may be NULL.
    let name = of_authors(column("name", "text", true));
    let unread = resolve(plugin::Query {
        columns: vec![name, offset],
        ..query(":one", "SELECT name, offset FROM authors")
    });
    assert!(unread.columns.iter().all(|field| field.ty.optional));
}

#[test]
fn a_command_that_returns_rows_needs_a_column() {
    // sqlc also reports none for `SELECT *` over a `VALUES` list, which does
    // return rows, so the error cannot just say to use `:exec`.
    assert!(error(query(":one", "UPDATE t SET a = 1")).contains(
        ":one has no result columns in sqlc's report; if the query returns rows, \
         list them with casts (SELECT n::int, s::text FROM ...), otherwise use :exec"
    ));
}

#[test]
fn execlastid_is_mysqls_alone() {
    let insert = query(":execlastid", "INSERT INTO authors (name) VALUES ('a')");
    assert_eq!(on("mysql", insert.clone()).unwrap().command, Command::ExecLastId);
    for engine in ["postgresql", "sqlite"] {
        let error = format!("{:#}", on(engine, insert.clone()).unwrap_err());
        assert!(
            error.contains("use INSERT ... RETURNING with :one"),
            "{engine}: {error}"
        );
    }
}

#[test]
fn a_batch_is_its_command_per_row_of_its_params() {
    for (batch, single) in [(":batchexec", ":exec"), (":batchone", ":one"), (":batchmany", ":many")] {
        let columns = vec![column("bio", "text", false), column("name", "text", true)];
        let resolved = resolve(plugin::Query {
            params: vec![param(1, column("id", "pg_catalog.int8", true))],
            columns: columns.clone(),
            ..query(batch, "SELECT bio, name FROM authors WHERE id = $1")
        });
        let one = resolve(plugin::Query {
            params: vec![param(1, column("id", "pg_catalog.int8", true))],
            columns,
            ..query(single, "SELECT bio, name FROM authors WHERE id = $1")
        });
        assert_eq!(resolved.command, one.command, "{batch}");
        assert_eq!(resolved.takes, Takes::Rows("GetAuthorParams".into()));
    }
}

#[test]
fn a_function_returns_what_one_run_of_its_command_does_or_a_list_of_that_per_row() {
    let returns = |cmd: &str, columns: Vec<plugin::Column>, raise_on_none: bool| {
        let config = Config {
            raise_on_none,
            ..Config::of(Backend::Psycopg)
        };
        let query = plugin::Query {
            text: selecting(&columns) + " WHERE id = $1",
            columns,
            params: vec![param(1, column("id", "int8", true))],
            ..query(cmd, "")
        };
        let mut modules = try_resolve_with("postgresql", vec![query], &config).unwrap();
        let query = modules.remove(0).queries.remove(0);
        python::unparse(&Backend::Psycopg.returns(&query, true))
    };
    let author = || authors().into_iter().map(of_authors).collect::<Vec<_>>();
    let bio = || vec![column("bio", "text", false)];

    assert_eq!(returns(":exec", vec![], false), "None");
    assert_eq!(returns(":execrows", vec![], false), "int");
    // The library's own result: here, the cursor.
    assert_eq!(
        returns(":execresult", vec![], false),
        "psycopg.AsyncRawCursor[tuple[Any, ...]]"
    );
    assert_eq!(returns(":many", author(), false), "list[models.Author]");
    // Without a row, `None`, unless that raises; a NULL is `None` once.
    assert_eq!(returns(":one", author(), false), "models.Author | None");
    assert_eq!(returns(":one", author(), true), "models.Author");
    assert_eq!(returns(":one", bio(), false), "str | None");
    assert_eq!(returns(":one", bio(), true), "str | None");
    // Per row: nothing, or each row's result.
    assert_eq!(returns(":batchexec", vec![], false), "None");
    assert_eq!(returns(":batchone", bio(), false), "list[str | None]");
    assert_eq!(returns(":batchmany", author(), false), "list[list[models.Author]]");
}

#[test]
fn a_command_the_plugin_does_not_know_is_an_error() {
    let error = error(query(":stream", "SELECT 1"));
    assert!(
        error.ends_with(":stream is not a command sqlc-gen-python knows"),
        "{error}"
    );
}

#[test]
fn a_many_that_only_reads_also_streams_under_its_name_with_no_trailing_underscore() {
    let columns = vec![column("bio", "text", false), column("name", "text", true)];
    let stream = |cmd: &str, name: &str, text: &str| {
        let params = match cmd.starts_with(":batch") {
            true => vec![param(1, column("id", "pg_catalog.int8", true))],
            false => vec![],
        };
        resolve(plugin::Query {
            name: name.into(),
            params,
            columns: columns.clone(),
            ..query(cmd, text)
        })
        .stream
    };
    let select = "SELECT bio, name FROM authors";
    assert_eq!(
        stream(":many", "ListAuthors", select),
        Some("stream_list_authors".into())
    );
    // `list_` hides a builtin, which `stream_list` does not
    assert_eq!(stream(":many", "List", select), Some("stream_list".into()));
    // A PostgreSQL cursor holds no write, and a batch runs once per row
    assert_eq!(
        stream(":many", "Purge", "DELETE FROM authors RETURNING bio, name"),
        None
    );
    assert_eq!(
        stream(
            ":batchmany",
            "ListAuthors",
            "SELECT bio, name FROM authors WHERE id = $1"
        ),
        None
    );
    assert_eq!(stream(":one", "GetAuthor", select), None);
}

#[test]
fn a_copyfrom_returns_the_database_row_count() {
    let copy = resolve(query(":copyfrom", "SELECT 1"));
    for backend in [Backend::SqlAlchemy, Backend::Psycopg] {
        assert_eq!(crate::python::unparse(&backend.returns(&copy, false)), "int");
    }
}
