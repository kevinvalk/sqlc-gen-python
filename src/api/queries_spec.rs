//! Specification for resolving queries into modules, functions and commands.

use super::*;
use crate::api::fixtures::authors::{resolve, *};
use crate::api::fixtures::*;
use crate::backend::Backend;

// -- Modules -----------------------------------------------------------------

#[test]
fn each_query_file_is_a_module_named_after_its_stem_in_sqlc_order() {
    let file = |name: &str, filename: &str| plugin::Query {
        name: name.into(),
        filename: filename.into(),
        ..query(":exec", "SELECT 1")
    };
    let modules = try_resolve_all(vec![
        file("A", "b.sql"),
        file("B", "a.sql"),
        file("C", "b.sql"),
    ])
    .unwrap();
    let shape: Vec<(&str, Vec<&str>)> = (modules.iter())
        .map(|m| {
            (
                m.name.as_str(),
                m.queries.iter().map(|q| q.function.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(shape, [("b", vec!["a", "c"]), ("a", vec!["b"])]);
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
    assert!(
        error.contains(r#"both become the module "a_b.py""#),
        "{error}"
    );
}

#[test]
fn every_problem_is_reported_at_once() {
    let guessed = |name: &str| plugin::Query {
        name: name.into(),
        columns: vec![column("v", "int", true)],
        ..query(":one", "SELECT 1")
    };
    let error = format!(
        "{:#}",
        try_resolve_all(vec![guessed("A"), guessed("B")]).unwrap_err()
    );
    assert!(error.starts_with("2 problems:"), "{error}");
    assert!(
        error.contains(r#"query "A""#) && error.contains(r#"query "B""#),
        "{error}"
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
    assert!(
        error.contains(r#"both become the Python name "get_a""#),
        "{error}"
    );
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
    for queries in [
        vec![row("Get1"), row("Get_1")],
        vec![copy("Add1"), copy("Add_1")],
    ] {
        let error = format!("{:#}", try_resolve_all(queries).unwrap_err());
        assert!(error.contains("both become the Python name \""), "{error}");
    }
}

// -- Names -------------------------------------------------------------------

#[test]
fn a_query_is_a_snake_case_function_a_private_constant_and_perhaps_a_class() {
    let query = resolve(query(":exec", "SELECT 1"));
    assert_eq!(
        (
            query.function.as_str(),
            query.constant.as_str(),
            query.classes().next()
        ),
        ("get_author", "_GET_AUTHOR", None)
    );
    let copy = resolve(self::query(":copyfrom", "SELECT 1"));
    assert_eq!(copy.takes, Takes::Rows("GetAuthorParams".into()));
    let Command::One(Shape::Row(class, _)) = returning(
        ":one",
        vec![column("bio", "text", false), column("name", "text", true)],
    ) else {
        panic!("not a row");
    };
    assert_eq!(class, "GetAuthorRow");
}

#[test]
fn a_function_named_like_a_keyword_reserved_name_or_builtin_takes_a_trailing_underscore() {
    for (name, function) in [
        ("Import", "import_"),
        ("Models", "models_"),
        ("Any", "any_"),
    ] {
        let query = resolve(plugin::Query {
            name: name.into(),
            ..query(":exec", "SELECT 1")
        });
        assert_eq!(query.function, function);
    }
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
    assert!(
        error.starts_with(r#"authors.sql: query "GetAuthor": "#),
        "{error}"
    );
}

// -- Commands ----------------------------------------------------------------

#[test]
fn commands_an_engine_cannot_run_are_errors_that_say_what_to_use() {
    assert!(error(query(":execlastid", "")).contains(
        ":execlastid is not supported on postgresql; use INSERT ... RETURNING with :one"
    ));
    // A batch runs per row of its parameters, on PostgreSQL alone.
    for batch in [":batchexec", ":batchmany", ":batchone"] {
        let error = error(query(batch, ""));
        assert!(
            error.contains("has no parameters to run it per row"),
            "{error}"
        );
        let error = format!("{:#}", on("sqlite", query(batch, "")).unwrap_err());
        assert!(
            error.contains(&format!("{batch} is PostgreSQL only")),
            "{error}"
        );
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
        assert!(
            error.contains(&format!("{cmd} counts -1 rows for a MERGE")),
            "{error}"
        );
    }
    resolve(query(":exec", merge));
    resolve(query(
        ":execrows",
        "DELETE FROM authors WHERE name = 'merge into'",
    ));
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
            ..query(":one", "SELECT 1")
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
fn a_not_null_column_read_from_a_nullable_one_is_a_coalesce_that_needs_a_cast() {
    // sqlc reports `coalesce(bio, 1.5)` as `bio` itself, NOT NULL: its first
    // argument's table, name and type, which PostgreSQL may well not return.
    let coalesced = plugin::Column {
        original_name: "bio".into(),
        ..of_authors(column("v", "text", true))
    };
    let error = error(plugin::Query {
        columns: vec![coalesced.clone()],
        ..query(":one", "SELECT coalesce(bio, 1.5) AS v FROM authors")
    });
    assert!(
        error.contains(r#"column "v" has a type sqlc could not infer; add a cast, as coalesce("#),
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
    assert_eq!(
        on("mysql", insert.clone()).unwrap().command,
        Command::ExecLastId
    );
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
    for (batch, single) in [
        (":batchexec", ":exec"),
        (":batchone", ":one"),
        (":batchmany", ":many"),
    ] {
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

// -- What a function returns --------------------------------------------------

#[test]
fn a_function_returns_what_one_run_of_its_command_does_or_a_list_of_that_per_row() {
    let returns = |cmd: &str, columns: Vec<plugin::Column>, raise_on_none: bool| {
        let config = Config {
            raise_on_none,
            ..Config::of(Backend::Psycopg)
        };
        let query = plugin::Query {
            columns,
            params: vec![param(1, column("id", "int8", true))],
            ..query(cmd, "SELECT $1")
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
    assert_eq!(
        returns(":batchmany", author(), false),
        "list[list[models.Author]]"
    );
}
